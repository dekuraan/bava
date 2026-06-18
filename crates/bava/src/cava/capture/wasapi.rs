// SPDX-License-Identifier: GPL-3.0-or-later
//! WASAPI loopback capture backend (Windows).
//!
//! Loops back the default render endpoint — whatever the system is playing — via
//! a shared-mode WASAPI client opened with `AUDCLNT_STREAMFLAGS_LOOPBACK`. Shared
//! mode can only capture at the device *mix format*, so this backend converts the
//! captured frames (float32 or 16/24/32-bit PCM, any channel count) down to the
//! `rate`/`channels` cavacore was planned for, linearly resampling and
//! up/down-mixing as needed. The rest of the pipeline is then identical to Linux.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::core::GUID;

use super::{AudioCapture, CaptureError, LinearResampler};

/// `wFormatTag` for raw IEEE float samples.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
/// `wFormatTag` for integer PCM samples.
const WAVE_FORMAT_PCM: u16 = 0x0001;
/// `wFormatTag` marking a `WAVEFORMATEXTENSIBLE`; the real format is in `SubFormat`.
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT` — float samples inside a `WAVEFORMATEXTENSIBLE`.
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
/// `AUDCLNT_BUFFERFLAGS_SILENT`: the packet is silence; its data may be skipped.
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

/// How the captured bytes encode each sample. All are normalized to roughly
/// [-1, 1] to match cavacore's expectation.
#[derive(Clone, Copy, PartialEq)]
enum SampleKind {
    /// 32-bit IEEE float, already in roughly [-1, 1].
    F32,
    /// 16-bit signed PCM.
    I16,
    /// 24-bit signed PCM, packed in 3 bytes little-endian.
    I24,
    /// 32-bit signed PCM. Also covers 24-in-32 EXTENSIBLE containers: Windows
    /// mandates MSB-justified (left-aligned) packing, so `i32 / 2^31` is correct
    /// for both (the 24-bit value occupies the top 24 bits of the 32-bit word).
    I32,
}

impl SampleKind {
    /// Bytes occupied by one sample of this kind in the captured stream.
    fn size(self) -> usize {
        match self {
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
            SampleKind::F32 | SampleKind::I32 => 4,
        }
    }
}

/// Outcome of draining the loopback buffer once.
enum PumpStatus {
    /// At least one packet was converted into `pending`.
    Data,
    /// No packet was ready (idle device, or a transient non-fatal error).
    Idle,
    /// The endpoint was invalidated (device switch/removal); the stream is dead
    /// and must be reopened against the new default endpoint.
    Lost,
}

/// The COM objects + captured format for one open stream. Rebuilt wholesale by
/// [`WasapiCapture::reopen`] when the endpoint is invalidated.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    device_rate: u32,
    device_channels: usize,
    kind: SampleKind,
    /// Bytes per device frame (from `nBlockAlign` — may include padding beyond
    /// `device_channels * kind.size()`).
    frame_bytes: usize,
}

/// A WASAPI loopback stream feeding interleaved `f64` samples at the requested
/// `rate`/`channels`.
pub struct WasapiCapture {
    stream: Stream,

    /// Requested output format.
    target_rate: u32,
    target_channels: usize,

    /// Converted, target-rate interleaved samples awaiting consumption.
    pending: VecDeque<f64>,
    /// Linear resampler (shared impl with Core Audio backend).
    resampler: LinearResampler,
    /// Scratch holding one converted device frame, reused across frames.
    frame: Vec<f64>,
}

// The COM interfaces are only ever touched from the capture thread that opened
// them (which also calls `CoInitializeEx`); moving the value onto that thread is
// the only cross-thread use.
unsafe impl Send for WasapiCapture {}

impl WasapiCapture {
    /// Open a loopback capture on the default render endpoint, converting to
    /// `target_rate`/`target_channels`. `_frame_samples` is accepted for parity
    /// with the Pulse backend but unused — WASAPI delivers its own packet sizes.
    pub fn open(
        target_rate: u32,
        target_channels: u8,
        _frame_samples: usize,
    ) -> Result<Self, CaptureError> {
        let target_channels = target_channels.max(1) as usize;
        unsafe {
            // MTA so the client can be driven from this background thread. A
            // prior init in another mode is harmless for our usage.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let stream = Self::open_stream()?;
        Ok(Self {
            stream,
            target_rate,
            target_channels,
            pending: VecDeque::new(),
            resampler: LinearResampler::new(target_channels),
            frame: vec![0.0; target_channels],
        })
    }

    /// Build a fresh loopback stream on the current default render endpoint.
    /// Assumes COM is already initialized on the calling thread.
    fn open_stream() -> Result<Stream, CaptureError> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| CaptureError::Init(format!("MMDeviceEnumerator: {e}")))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| CaptureError::Init(format!("default render endpoint: {e}")))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| CaptureError::Init(format!("activate IAudioClient: {e}")))?;

            // Shared loopback must use the device mix format verbatim.
            let mix = client
                .GetMixFormat()
                .map_err(|e| CaptureError::Init(format!("GetMixFormat: {e}")))?;
            if mix.is_null() {
                return Err(CaptureError::Init("GetMixFormat returned null".into()));
            }
            let (device_rate, device_channels, kind, frame_bytes) = parse_format(mix);

            // ~1s ring; periodicity must be 0 for shared mode.
            let res = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                10_000_000,
                0,
                mix,
                None,
            );
            CoTaskMemFree(Some(mix as *const core::ffi::c_void));
            res.map_err(|e| CaptureError::Init(format!("IAudioClient::Initialize: {e}")))?;

            let kind =
                kind.ok_or_else(|| CaptureError::Init("unsupported mix sample format".into()))?;
            if device_channels == 0 {
                return Err(CaptureError::Init("device reports zero channels".into()));
            }

            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| CaptureError::Init(format!("IAudioCaptureClient: {e}")))?;
            client
                .Start()
                .map_err(|e| CaptureError::Init(format!("IAudioClient::Start: {e}")))?;

            Ok(Stream {
                client,
                capture,
                device_rate,
                device_channels,
                kind,
                frame_bytes,
            })
        }
    }

    /// Rebuild the stream after the endpoint was invalidated (e.g. the user
    /// switched the default output device). Resets resampler state — the new
    /// device may have a different format — but keeps `pending`, whose samples
    /// are already at the target rate. Returns whether the rebuild succeeded.
    fn reopen(&mut self) -> bool {
        match Self::open_stream() {
            Ok(stream) => {
                unsafe {
                    let _ = self.stream.client.Stop();
                }
                self.stream = stream;
                self.resampler.reset();
                true
            }
            Err(_) => false,
        }
    }

    /// Drain every queued loopback packet into `pending`, converting format,
    /// channel layout and sample rate.
    fn pump(&mut self) -> PumpStatus {
        let mut status = PumpStatus::Idle;
        let step = if self.stream.device_rate == self.target_rate {
            1.0
        } else {
            self.stream.device_rate as f64 / self.target_rate as f64
        };
        unsafe {
            loop {
                let frames = match self.stream.capture.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(e) => {
                        return if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
                            PumpStatus::Lost
                        } else {
                            status
                        };
                    }
                };
                if frames == 0 {
                    break;
                }

                let mut data: *mut u8 = std::ptr::null_mut();
                let mut n_frames: u32 = 0;
                let mut flags: u32 = 0;
                if let Err(e) =
                    self.stream
                        .capture
                        .GetBuffer(&mut data, &mut n_frames, &mut flags, None, None)
                {
                    return if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
                        PumpStatus::Lost
                    } else {
                        status
                    };
                }
                status = PumpStatus::Data;

                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT != 0;
                let frame_bytes = self.stream.frame_bytes;
                for f in 0..n_frames as usize {
                    if silent || data.is_null() {
                        self.frame.iter_mut().for_each(|x| *x = 0.0);
                    } else {
                        self.fill_frame(data.add(f * frame_bytes));
                    }
                    self.resampler.push(step, &self.frame, &mut self.pending);
                }

                let _ = self.stream.capture.ReleaseBuffer(n_frames);
            }
        }
        status
    }

    /// Convert one device frame at `base` into `self.frame` (a `target_channels`
    /// frame), reading per-channel at the sample stride for the device format and
    /// down/up-mixing channels.
    unsafe fn fill_frame(&mut self, base: *const u8) {
        let kind = self.stream.kind;
        let dc = self.stream.device_channels; // guaranteed >= 1 by open_stream
        let size = kind.size();
        let read = |ch: usize| -> f64 {
            unsafe {
                let p = base.add(ch * size);
                match kind {
                    SampleKind::F32 => (p as *const f32).read_unaligned() as f64,
                    SampleKind::I16 => (p as *const i16).read_unaligned() as f64 / 32768.0,
                    SampleKind::I32 => (p as *const i32).read_unaligned() as f64 / 2147483648.0,
                    SampleKind::I24 => {
                        // Sign-extend a 24-bit little-endian value to i32.
                        let lo = *p as i32;
                        let mid = *p.add(1) as i32;
                        let hi = *p.add(2) as i32;
                        let v = ((lo | (mid << 8) | (hi << 16)) << 8) >> 8;
                        v as f64 / 8388608.0
                    }
                }
            }
        };

        self.frame.clear();
        if self.target_channels == 1 {
            // Mono: average all device channels.
            let sum: f64 = (0..dc).map(read).sum();
            self.frame.push(sum / dc as f64);
        } else {
            for c in 0..self.target_channels {
                self.frame.push(read(c.min(dc - 1)));
            }
        }
    }

}

impl Drop for WasapiCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.stream.client.Stop();
        }
    }
}

impl AudioCapture for WasapiCapture {
    fn read(&mut self, buf: &mut [f64]) -> Result<(), CaptureError> {
        // Fill the whole buffer. Loop pumping packets, but cap the wait to the
        // real-time span this chunk represents so an idle device (no loopback
        // packets) yields silence at a steady cadence instead of spinning or
        // blocking forever. On endpoint invalidation, rebind to the new default.
        let frames = buf.len() / self.target_channels.max(1);
        let budget = Duration::from_secs_f64(frames as f64 / self.target_rate as f64);
        let start = Instant::now();
        while self.pending.len() < buf.len() {
            match self.pump() {
                PumpStatus::Data => {}
                PumpStatus::Idle => {
                    if start.elapsed() >= budget {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(3));
                }
                PumpStatus::Lost => {
                    if !self.reopen() {
                        // Back off so read() doesn't spin at 100% CPU while
                        // the device is disconnected and no replacement exists.
                        std::thread::sleep(Duration::from_millis(100));
                        break;
                    }
                }
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

/// Inspect a `WAVEFORMATEX*` (possibly a `WAVEFORMATEXTENSIBLE`) and extract the
/// rate, channel count, sample encoding and per-frame byte stride. Returns
/// `kind == None` for formats we can't decode.
unsafe fn parse_format(mix: *const WAVEFORMATEX) -> (u32, usize, Option<SampleKind>, usize) {
    let wf = unsafe { mix.read_unaligned() };
    let channels = wf.nChannels as usize;
    let rate = wf.nSamplesPerSec;
    let bits = wf.wBitsPerSample;
    let frame_bytes = wf.nBlockAlign as usize;

    let mut tag = wf.wFormatTag;
    if tag == WAVE_FORMAT_EXTENSIBLE && wf.cbSize >= 22 {
        let ext = unsafe { (mix as *const WAVEFORMATEXTENSIBLE).read_unaligned() };
        // Copy out of the packed struct before comparing (no refs to fields).
        let subformat = ext.SubFormat;
        // wValidBitsPerSample names the true data depth for PCM containers; for
        // floats it equals wBitsPerSample (32). We keep `bits` as the container
        // size (wBitsPerSample) because Windows mandates MSB-justified packing —
        // 24-in-32 puts data in the top 24 bits, so i32/2^31 stays correct.
        tag = if subformat == SUBTYPE_IEEE_FLOAT {
            WAVE_FORMAT_IEEE_FLOAT
        } else {
            WAVE_FORMAT_PCM
        };
    }

    let kind = match (tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => Some(SampleKind::F32),
        (WAVE_FORMAT_PCM, 16) => Some(SampleKind::I16),
        (WAVE_FORMAT_PCM, 24) => Some(SampleKind::I24),
        (WAVE_FORMAT_PCM, 32) => Some(SampleKind::I32),
        _ => None,
    };
    (rate, channels, kind, frame_bytes)
}
