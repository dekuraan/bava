// SPDX-License-Identifier: GPL-3.0-or-later
//! Core Audio process-tap capture backend (macOS 14.2+).
//!
//! Captures the system output mix with **no virtual device and no extra install**:
//! it creates a global *process tap* ([`AudioHardwareCreateProcessTap`] with a
//! [`CATapDescription`] that taps every process but excludes none), wraps it in a
//! private aggregate device ([`AudioHardwareCreateAggregateDevice`]), and runs an
//! IO proc on that device. The tap is *unmuted*, so audio still reaches the
//! speakers while we observe it.
//!
//! The tap delivers float samples at the device mix format (rate + channel count
//! we don't control), so — exactly like the Windows WASAPI backend — this converts
//! to the `rate`/`channels` cavacore was planned for: the realtime IO proc only
//! copies interleaved `f32` into a shared queue, and [`read`](CoreAudioCapture::read)
//! (on the capture thread) down/up-mixes and linearly resamples to the target.
//!
//! macOS gates process taps behind the **Audio Capture** privacy permission
//! (`NSAudioCaptureUsageDescription`); without it `AudioHardwareCreateProcessTap`
//! fails and we surface a [`CaptureError::Init`].

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
    kAudioTapPropertyUID, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
    AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{kAudioFormatFlagIsFloat, AudioBufferList, AudioStreamBasicDescription};
use objc2_core_foundation::{CFDictionary, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use super::{AudioCapture, CaptureError};

/// Shared hand-off between the realtime IO proc (producer) and `read` (consumer).
///
/// Holds device-format interleaved `f32` (one value per channel per frame, at the
/// tap's own rate). The IO proc must stay realtime-safe, so it only locks briefly
/// to append; all format conversion happens on the consumer side.
struct Shared {
    queue: Mutex<VecDeque<f32>>,
    /// Hard cap on buffered samples (~a few seconds) so a stalled consumer can't
    /// grow the queue without bound; oldest samples are dropped past the cap.
    cap: usize,
}

/// A Core Audio process-tap stream feeding interleaved `f64` at the requested
/// `rate`/`channels`.
pub struct CoreAudioCapture {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    shared: Arc<Shared>,

    /// Tap (device) format.
    device_rate: u32,
    device_channels: usize,

    /// Requested output format.
    target_rate: u32,
    target_channels: usize,

    /// Linear-resampler state: previous target-channel frame and the fractional
    /// position between it and the next incoming frame.
    prev: Option<Vec<f64>>,
    frac: f64,
    /// Converted, target-rate interleaved samples awaiting consumption.
    pending: VecDeque<f64>,
}

// The Core Audio object IDs are plain integers and the queue is behind a mutex;
// the value is only moved onto the capture thread that drives it.
unsafe impl Send for CoreAudioCapture {}

impl CoreAudioCapture {
    /// Build a global tap + aggregate device and start capturing, converting to
    /// `target_rate`/`target_channels`. `_frame_samples` is accepted for parity
    /// with the Pulse backend but unused — Core Audio delivers its own IO sizes.
    pub fn open(
        target_rate: u32,
        target_channels: u8,
        _frame_samples: usize,
    ) -> Result<Self, CaptureError> {
        let target_channels = target_channels.max(1) as usize;
        unsafe {
            // 1. Describe a stereo global tap: include everything, exclude nothing.
            //    Left unmuted (default) so playback is unaffected; private so it
            //    doesn't show up to other processes.
            let exclude: Retained<NSArray<NSNumber>> = NSArray::new();
            let desc = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &exclude,
            );
            desc.setName(&NSString::from_str("bava system capture"));
            desc.setPrivate(true);

            // 2. Create the tap object.
            let mut tap: AudioObjectID = 0;
            let status = AudioHardwareCreateProcessTap(Some(&*desc), &mut tap);
            if status != 0 || tap == 0 {
                return Err(CaptureError::Init(format!(
                    "AudioHardwareCreateProcessTap failed (status {status}); \
                     grant the app the Audio Recording permission (macOS 14.2+ required)"
                )));
            }

            // Past here, any early return must clean up the tap.
            let result = (|| {
                // 3. Read the tap's UID (a CFString) and stream format (an ASBD).
                let uid = read_tap_uid(tap)?;
                let asbd = read_tap_format(tap)?;
                if asbd.mFormatFlags & kAudioFormatFlagIsFloat == 0 || asbd.mBitsPerChannel != 32 {
                    return Err(CaptureError::Init(format!(
                        "unexpected tap format: {} bits, flags {:#x} (expected 32-bit float)",
                        asbd.mBitsPerChannel, asbd.mFormatFlags
                    )));
                }
                let device_rate = asbd.mSampleRate as u32;
                let device_channels = asbd.mChannelsPerFrame.max(1) as usize;

                // 4. Wrap the tap in a private aggregate device. The description is
                //    an NSDictionary (toll-free-bridged to CFDictionary):
                //      { uid: <our-uuid>, private: 1, tapautostart: 1,
                //        taps: [ { uid: <tap-uid> } ] }
                let agg = create_aggregate(&uid)?;

                // 5. Register the IO proc and start IO. The client pointer is the
                //    shared queue; it stays valid until Drop tears the proc down
                //    before releasing the Arc.
                let shared = Arc::new(Shared {
                    queue: Mutex::new(VecDeque::new()),
                    // ~4 s of device-rate audio.
                    cap: (device_rate as usize * device_channels * 4).max(1),
                });
                let client = Arc::as_ptr(&shared) as *mut c_void;

                let mut proc_id: AudioDeviceIOProcID = None;
                let status = AudioDeviceCreateIOProcID(
                    agg,
                    Some(io_proc),
                    client,
                    NonNull::from(&mut proc_id),
                );
                if status != 0 || proc_id.is_none() {
                    AudioHardwareDestroyAggregateDevice(agg);
                    return Err(CaptureError::Init(format!(
                        "AudioDeviceCreateIOProcID failed (status {status})"
                    )));
                }

                let status = AudioDeviceStart(agg, proc_id);
                if status != 0 {
                    AudioDeviceDestroyIOProcID(agg, proc_id);
                    AudioHardwareDestroyAggregateDevice(agg);
                    return Err(CaptureError::Init(format!(
                        "AudioDeviceStart failed (status {status})"
                    )));
                }

                Ok(Self {
                    tap,
                    aggregate: agg,
                    proc_id,
                    shared,
                    device_rate,
                    device_channels,
                    target_rate,
                    target_channels,
                    prev: None,
                    frac: 0.0,
                    pending: VecDeque::new(),
                })
            })();

            if result.is_err() {
                AudioHardwareDestroyProcessTap(tap);
            }
            result
        }
    }

    /// Drain device-format frames from the shared queue, converting channel layout
    /// and sample rate into `pending`. Returns whether any samples were consumed.
    fn pump(&mut self) -> bool {
        // Snapshot whole frames out from under the lock, then convert lock-free.
        let frames: Vec<f32> = {
            let mut q = self.shared.queue.lock().unwrap();
            if q.is_empty() {
                return false;
            }
            // Only take complete device frames; leave any partial tail buffered.
            let whole = (q.len() / self.device_channels) * self.device_channels;
            q.drain(..whole).collect()
        };
        if frames.is_empty() {
            return false;
        }
        for frame in frames.chunks_exact(self.device_channels) {
            let mixed = self.downmix(frame);
            self.resample_push(&mixed);
        }
        true
    }

    /// Map one device frame (interleaved `f32`) to a `target_channels` frame.
    fn downmix(&self, frame: &[f32]) -> Vec<f64> {
        let dc = self.device_channels;
        if self.target_channels == 1 {
            let sum: f64 = frame.iter().map(|&s| s as f64).sum();
            return vec![sum / dc as f64];
        }
        (0..self.target_channels)
            .map(|c| frame[c.min(dc - 1)] as f64)
            .collect()
    }

    /// Streaming linear resampler: feed one device-rate frame, emit zero or more
    /// target-rate frames into `pending`. (Identical scheme to the WASAPI backend.)
    fn resample_push(&mut self, cur: &[f64]) {
        if self.device_rate == self.target_rate {
            self.pending.extend(cur.iter().copied());
            return;
        }
        let step = self.device_rate as f64 / self.target_rate as f64;
        if let Some(prev) = self.prev.take() {
            while self.frac < 1.0 {
                for c in 0..self.target_channels {
                    self.pending
                        .push_back(prev[c] + (cur[c] - prev[c]) * self.frac);
                }
                self.frac += step;
            }
            self.frac -= 1.0;
        }
        self.prev = Some(cur.to_vec());
    }
}

impl Drop for CoreAudioCapture {
    fn drop(&mut self) {
        unsafe {
            // Stop and unregister the IO proc *first* so `io_proc` can't run after
            // the shared Arc is released, then tear down device and tap.
            AudioDeviceStop(self.aggregate, self.proc_id);
            AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.aggregate);
            AudioHardwareDestroyProcessTap(self.tap);
        }
    }
}

impl AudioCapture for CoreAudioCapture {
    fn read(&mut self, buf: &mut [f64]) -> Result<(), CaptureError> {
        // Mirror the WASAPI backend: fill the whole buffer, but cap the wait to the
        // real-time span this chunk represents so a silent device still yields a
        // steady cadence of (zero-filled) frames instead of blocking forever.
        let frames = buf.len() / self.target_channels.max(1);
        let budget = Duration::from_secs_f64(frames as f64 / self.target_rate as f64);
        let start = Instant::now();
        while self.pending.len() < buf.len() {
            if !self.pump() {
                if start.elapsed() >= budget {
                    break;
                }
                std::thread::sleep(Duration::from_millis(3));
            }
        }
        for slot in buf.iter_mut() {
            *slot = self.pending.pop_front().unwrap_or(0.0);
        }
        Ok(())
    }

    fn rate(&self) -> u32 {
        self.target_rate
    }

    fn channels(&self) -> u32 {
        self.target_channels as u32
    }
}

/// The realtime IO proc: append the tap's input frames to the shared queue.
///
/// Runs on a Core Audio realtime thread, so it does the minimum: interleave (the
/// tap may hand back planar buffers) into `f32` and push under a short-held lock.
/// All resampling/mixing happens later in [`CoreAudioCapture::pump`].
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<objc2_core_audio_types::AudioTimeStamp>,
    in_input: NonNull<AudioBufferList>,
    _in_time: NonNull<objc2_core_audio_types::AudioTimeStamp>,
    _out_output: NonNull<AudioBufferList>,
    _out_time: NonNull<objc2_core_audio_types::AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    if client.is_null() {
        return 0;
    }
    let shared = unsafe { &*(client as *const Shared) };
    let list = unsafe { in_input.as_ref() };
    let n_buffers = list.mNumberBuffers as usize;
    if n_buffers == 0 {
        return 0;
    }
    // `mBuffers` is a C flexible array; view it as a slice of the real length.
    let buffers =
        unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), n_buffers) };

    let mut q = match shared.queue.lock() {
        Ok(q) => q,
        Err(_) => return 0,
    };

    if n_buffers == 1 {
        // One interleaved buffer: copy its f32 samples straight through.
        let b = &buffers[0];
        if !b.mData.is_null() {
            let count = b.mDataByteSize as usize / std::mem::size_of::<f32>();
            let data = b.mData as *const f32;
            for i in 0..count {
                q.push_back(unsafe { data.add(i).read_unaligned() });
            }
        }
    } else {
        // Planar: one buffer per channel; interleave frame by frame.
        let frames = buffers
            .iter()
            .map(|b| b.mDataByteSize as usize / std::mem::size_of::<f32>())
            .min()
            .unwrap_or(0);
        for i in 0..frames {
            for b in buffers {
                if b.mData.is_null() {
                    q.push_back(0.0);
                } else {
                    let data = b.mData as *const f32;
                    q.push_back(unsafe { data.add(i).read_unaligned() });
                }
            }
        }
    }

    // Bound the queue: drop the oldest overflow so a stalled consumer can't grow it.
    if q.len() > shared.cap {
        let overflow = q.len() - shared.cap;
        q.drain(..overflow);
    }
    0
}

/// A global-scope, main-element property address for `selector`.
fn property_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Read `kAudioTapPropertyUID` off the tap object as an owned `NSString`.
unsafe fn read_tap_uid(tap: AudioObjectID) -> Result<Retained<NSString>, CaptureError> {
    let addr = property_address(kAudioTapPropertyUID);
    let mut cfstr: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut cfstr as *mut _ as *mut c_void).unwrap(),
        )
    };
    if status != 0 || cfstr.is_null() {
        return Err(CaptureError::Init(format!(
            "read tap UID failed (status {status})"
        )));
    }
    // CFString is toll-free bridged to NSString; adopt the +1 retain we own.
    let ns = unsafe { Retained::from_raw(cfstr as *mut NSString) };
    ns.ok_or_else(|| CaptureError::Init("tap UID was null".into()))
}

/// Read `kAudioTapPropertyFormat` off the tap object as an ASBD.
unsafe fn read_tap_format(
    tap: AudioObjectID,
) -> Result<AudioStreamBasicDescription, CaptureError> {
    let addr = property_address(kAudioTapPropertyFormat);
    // ASBD is a plain `repr(C)` POD with no `Default`; zero-init is the idiom.
    let mut asbd: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut asbd as *mut _ as *mut c_void).unwrap(),
        )
    };
    if status != 0 {
        return Err(CaptureError::Init(format!(
            "read tap format failed (status {status})"
        )));
    }
    Ok(asbd)
}

/// Build the aggregate-device description dictionary and create the device.
unsafe fn create_aggregate(tap_uid: &NSString) -> Result<AudioObjectID, CaptureError> {
    // String keys come from Core Audio as C strings; bridge them to NSString.
    // Each key must be held in a named local: the dictionary borrows the `&NSString`
    // only for the duration of the call, but a temporary would drop at the `;`.
    let key = |k: &std::ffi::CStr| NSString::from_str(k.to_str().unwrap_or_default());

    // The single sub-tap entry: { "uid": <tap-uid> }.
    let sub_uid_key = key(kAudioSubTapUIDKey);
    let sub_tap: Retained<NSDictionary<NSString, NSString>> =
        NSDictionary::from_slices(&[&*sub_uid_key], &[tap_uid]);
    let tap_list: Retained<NSArray<NSDictionary<NSString, NSString>>> =
        NSArray::from_slice(&[&*sub_tap]);

    // A unique UID for our aggregate device, plus a shared boolean value.
    let agg_uid = NSString::from_str("com.bava.system-capture.aggregate");
    let one = NSNumber::numberWithBool(true);

    let k_uid = key(kAudioAggregateDeviceUIDKey);
    let k_private = key(kAudioAggregateDeviceIsPrivateKey);
    let k_autostart = key(kAudioAggregateDeviceTapAutoStartKey);
    let k_taps = key(kAudioAggregateDeviceTapListKey);
    let keys: [&NSString; 4] = [&k_uid, &k_private, &k_autostart, &k_taps];

    // Erase each value to `&AnyObject` (the dict's object type). Deref the
    // `Retained` to the object first so `AsRef<AnyObject>` resolves unambiguously.
    let v_uid: &AnyObject = (*agg_uid).as_ref();
    let v_bool: &AnyObject = (*one).as_ref();
    let v_taps: &AnyObject = (*tap_list).as_ref();
    let values: [&AnyObject; 4] = [v_uid, v_bool, v_bool, v_taps];

    let desc: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&keys, &values);

    // NSDictionary* is toll-free bridged to CFDictionaryRef.
    let desc_ref: &NSDictionary<NSString, AnyObject> = &desc;
    let cf = unsafe { &*(desc_ref as *const NSDictionary<NSString, AnyObject> as *const CFDictionary) };

    let mut agg: AudioObjectID = 0;
    let status = unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut agg)) };
    if status != 0 || agg == 0 {
        return Err(CaptureError::Init(format!(
            "AudioHardwareCreateAggregateDevice failed (status {status})"
        )));
    }
    Ok(agg)
}
