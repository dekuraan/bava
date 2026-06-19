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
use std::ffi::{c_char, c_int, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyNominalSampleRate,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, kAudioTapPropertyUID, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectRemovePropertyListener, CATapDescription,
};
use objc2_core_audio_types::{kAudioFormatFlagIsFloat, AudioBufferList, AudioStreamBasicDescription};
use objc2_core_foundation::{CFDictionary, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use super::{AudioCapture, CaptureError, LinearResampler};

/// Shared hand-off between the realtime IO proc (producer) and `read` (consumer).
///
/// Holds device-format interleaved `f32` (one value per channel per frame, at the
/// tap's own rate). The IO proc uses `try_lock` — never blocking on a realtime
/// thread — so a missed lock drops the current callback's samples rather than
/// priority-inverting on the consumer.
struct Shared {
    queue: Mutex<VecDeque<f32>>,
    /// Hard cap on buffered samples (~4 s of device-rate audio) so a stalled
    /// consumer can't grow the queue without bound; oldest samples are dropped.
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

    /// Resampler (shared impl, avoids per-frame allocation).
    resampler: LinearResampler,
    /// Scratch buffer for one down/up-mixed frame (reused across frames).
    mixed: Vec<f64>,
    /// Converted, target-rate interleaved samples awaiting consumption.
    pending: VecDeque<f64>,
    /// Set by [`format_listener`] when the aggregate device's nominal sample
    /// rate changes; [`read`](CoreAudioCapture::read) then re-reads the tap
    /// format and fixes the resampler ratio so a mid-stream device rate change
    /// doesn't silently corrupt the output. The listener is registered on
    /// `aggregate` and removed in `Drop`; this `Arc` keeps the flag alive for
    /// the listener's `client_data` pointer.
    format_dirty: Arc<AtomicBool>,
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

        // Preflight the audio-capture (System Audio Recording) permission. A
        // denied/undetermined state otherwise surfaces only as an opaque
        // tap-creation failure; checking first lets us point the user straight at
        // the right Settings pane. If TCC can't be queried we proceed and let the
        // tap-creation path's own fallback handle it.
        if audio_capture_denied() {
            open_audio_privacy_settings();
            return Err(CaptureError::Init(
                "audio capture permission not granted — add bava under System Settings ▸ \
                 Privacy & Security ▸ Screen & System Audio Recording (\"System Audio Recording \
                 Only\") and relaunch (requires macOS 14.2+)"
                    .into(),
            ));
        }

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
                // The overwhelmingly common cause is a missing Audio Recording
                // permission. Surface an actionable error and pop the relevant
                // privacy pane (the same "System Audio Recording Only" list cava
                // points users at) so the visualizer doesn't just silently idle.
                open_audio_privacy_settings();
                return Err(CaptureError::Init(format!(
                    "AudioHardwareCreateProcessTap failed (status {status}). This usually \
                     means the Audio Recording permission is missing — grant it under \
                     System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording \
                     (\"System Audio Recording Only\"), then relaunch. Requires macOS 14.2+."
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

                // 4. Wrap the tap in a private aggregate device. Include the PID in
                //    the UID so a crashed prior instance never collides with us.
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

                // 6. Watch the aggregate's nominal sample rate so a mid-stream
                //    device rate change flips `format_dirty`; `read` then re-reads
                //    the tap format and corrects the resampler. Best-effort: a
                //    failed registration just means we don't get the notification.
                let format_dirty = Arc::new(AtomicBool::new(false));
                let addr = property_address(kAudioDevicePropertyNominalSampleRate);
                let st = AudioObjectAddPropertyListener(
                    agg,
                    NonNull::from(&addr),
                    Some(format_listener),
                    Arc::as_ptr(&format_dirty) as *mut c_void,
                );
                if st != 0 {
                    bevy::log::warn!(
                        "bava: could not watch device sample-rate changes (status {st})"
                    );
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
                    resampler: LinearResampler::new(target_channels),
                    mixed: vec![0.0; target_channels],
                    pending: VecDeque::new(),
                    format_dirty,
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
        let step = if self.device_rate == self.target_rate {
            1.0
        } else {
            self.device_rate as f64 / self.target_rate as f64
        };
        for frame in frames.chunks_exact(self.device_channels) {
            self.downmix_into(frame);
            // resampler, mixed, and pending are distinct fields; NLL permits
            // disjoint mutable + immutable borrows of separate struct fields.
            self.resampler.push(step, &self.mixed, &mut self.pending);
        }
        true
    }

    /// Down/up-mix one device frame (`f32` interleaved) into `self.mixed`
    /// (`f64`, `target_channels` long). Reuses the allocation across frames.
    fn downmix_into(&mut self, frame: &[f32]) {
        self.mixed.clear();
        let dc = self.device_channels;
        if self.target_channels == 1 {
            let sum: f64 = frame.iter().map(|&s| s as f64).sum();
            self.mixed.push(sum / dc as f64);
        } else {
            for c in 0..self.target_channels {
                self.mixed.push(frame[c.min(dc - 1)] as f64);
            }
        }
    }
}

impl Drop for CoreAudioCapture {
    fn drop(&mut self) {
        unsafe {
            // Remove the property listener before the aggregate (and the `Arc`
            // backing its client_data) go away, so it can't fire into freed memory.
            let addr = property_address(kAudioDevicePropertyNominalSampleRate);
            AudioObjectRemovePropertyListener(
                self.aggregate,
                NonNull::from(&addr),
                Some(format_listener),
                Arc::as_ptr(&self.format_dirty) as *mut c_void,
            );
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
        // If the device's sample rate changed under us, re-read the tap format
        // and fix the resampler ratio (and channel stride) so we don't keep
        // resampling against a stale device rate.
        if self.format_dirty.swap(false, Ordering::Relaxed) {
            if let Ok(asbd) = unsafe { read_tap_format(self.tap) } {
                let new_rate = (asbd.mSampleRate as u32).max(1);
                let new_channels = asbd.mChannelsPerFrame.max(1) as usize;
                if new_rate != self.device_rate || new_channels != self.device_channels {
                    self.device_rate = new_rate;
                    self.device_channels = new_channels;
                    self.resampler.reset();
                }
            }
        }

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
        // Bound the converted backlog so a brief consumer stall can't let
        // `pending` grow without limit; keep the most recent ~2 s, drop older.
        // (The device-rate queue in `Shared` is already capped; this caps the
        // post-resample side too.)
        let cap = (self.target_rate as usize * self.target_channels * 2).max(buf.len());
        if self.pending.len() > cap {
            let overflow = self.pending.len() - cap;
            self.pending.drain(..overflow);
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
/// Runs on a Core Audio realtime thread. Uses `try_lock` — never blocking — so
/// a busy consumer causes this callback to drop the current buffer rather than
/// priority-inverting on the consumer thread. All resampling/mixing happens
/// later in [`CoreAudioCapture::pump`].
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

    // try_lock so we never block on a realtime thread; drop this callback's
    // samples if the consumer is mid-drain.
    let mut q = match shared.queue.try_lock() {
        Ok(q) => q,
        Err(_) => return 0,
    };

    if n_buffers == 1 {
        // One interleaved buffer: copy its f32 samples straight through.
        // Null mData signals silence; push zeros to preserve frame timing,
        // matching the zero-fill the planar path does per channel.
        let b = &buffers[0];
        let count = b.mDataByteSize as usize / std::mem::size_of::<f32>();
        if b.mData.is_null() {
            for _ in 0..count {
                q.push_back(0.0);
            }
        } else {
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

/// Best-effort: open the macOS privacy pane where audio capture is granted.
///
/// Process taps are gated behind the same authorization as screen/system-audio
/// recording, so a denied permission otherwise surfaces only as an opaque
/// `AudioHardwareCreateProcessTap` status. Pointing the user straight at the
/// settings pane makes it fixable. Failure to spawn `open` is ignored — it is a
/// convenience, not a requirement.
fn open_audio_privacy_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.settings.PrivacySecurity?Privacy_ScreenCapture")
        .spawn();
}

// libSystem's dynamic loader, used to reach the private TCC framework for a
// permission preflight without link-time dependency on it. Always present on macOS.
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

/// Whether macOS reports the audio-capture (System Audio Recording) permission as
/// *not* granted, via the private `TCCAccessPreflight`. Returns `false` when the
/// check can't be performed, so callers fall back to attempting the tap.
fn audio_capture_denied() -> bool {
    const RTLD_NOW: c_int = 2;
    // TCCAccessPreflight: 0 = granted, 1 = denied, 2 = undetermined. Audio taps
    // get no automatic prompt, so anything but granted means the user must add us
    // in Settings first.
    type Preflight = unsafe extern "C" fn(*const c_void, *const c_void) -> c_int;
    unsafe {
        let handle = dlopen(
            c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC".as_ptr(),
            RTLD_NOW,
        );
        if handle.is_null() {
            return false;
        }
        let sym = dlsym(handle, c"TCCAccessPreflight".as_ptr());
        // A null `sym` transmutes to `None` (fn-pointer null niche).
        let denied = match std::mem::transmute::<*mut c_void, Option<Preflight>>(sym) {
            Some(preflight) => {
                let service = CFString::from_str("kTCCServiceAudioCapture");
                let status =
                    preflight((&*service) as *const CFString as *const c_void, std::ptr::null());
                status != 0
            }
            None => false,
        };
        dlclose(handle);
        denied
    }
}

/// Property-listener callback: flips the `format_dirty` flag passed as
/// `client_data` when the watched device property changes. Runs on a Core Audio
/// notification thread, so it does nothing but an atomic store.
unsafe extern "C-unwind" fn format_listener(
    _object: AudioObjectID,
    _n_addresses: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut c_void,
) -> i32 {
    if !client_data.is_null() {
        let flag = unsafe { &*(client_data as *const AtomicBool) };
        flag.store(true, Ordering::Relaxed);
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

    // Include the PID in the aggregate device UID so that a crashed prior instance
    // (which leaves the device registered in the HAL) can never collide with us.
    let agg_uid = NSString::from_str(&format!(
        "com.bava.system-capture.aggregate.{}",
        std::process::id()
    ));
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
