// SPDX-License-Identifier: GPL-3.0-or-later
//! WASAPI loopback capture backend (Windows).
//!
//! Loops back the default render endpoint — whatever the system is playing — via
//! a shared-mode WASAPI client opened with `AUDCLNT_STREAMFLAGS_LOOPBACK`. Shared
//! mode can only capture at the device *mix format*, so this backend converts the
//! captured frames (float32 or 16-bit PCM, any channel count) down to the
//! `rate`/`channels` cavacore was planned for, linearly resampling and
//! up/down-mixing as needed. The rest of the pipeline is then identical to Linux.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::core::GUID;

use super::{AudioCapture, CaptureError};

/// `wFormatTag` for raw IEEE float samples.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
/// `wFormatTag` marking a `WAVEFORMATEXTENSIBLE`; the real format is in `SubFormat`.
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT` — float samples inside a `WAVEFORMATEXTENSIBLE`.
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
/// `AUDCLNT_BUFFERFLAGS_SILENT`: the packet is silence; its data may be skipped.
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

/// How the captured bytes encode each sample.
#[derive(Clone, Copy, PartialEq)]
enum SampleKind {
    /// 32-bit IEEE float, already in roughly [-1, 1].
    F32,
    /// 16-bit signed PCM; normalized by 1/32768 to match the float path.
    I16,
}

/// A WASAPI loopback stream feeding interleaved `f64` samples at the requested
/// `rate`/`channels`.
pub struct WasapiCapture {
    client: IAudioClient,
    capture: IAudioCaptureClient,

    /// Captured device format.
    device_rate: u32,
    device_channels: usize,
    kind: SampleKind,
    /// Bytes per device frame (`device_channels * sample_size`).
    frame_bytes: usize,

    /// Requested output format.
    target_rate: u32,
    target_channels: usize,

    /// Converted, target-rate interleaved samples awaiting consumption.
    pending: VecDeque<f64>,
    /// Linear-resampler state: previous target-channel frame and the fractional
    /// position between it and the next incoming frame.
    prev: Option<Vec<f64>>,
    frac: f64,
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
            CoTaskMemFree(Some(mix as *const _));
            res.map_err(|e| CaptureError::Init(format!("IAudioClient::Initialize: {e}")))?;

            let kind = kind
                .ok_or_else(|| CaptureError::Init("unsupported mix sample format".into()))?;

            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| CaptureError::Init(format!("IAudioCaptureClient: {e}")))?;
            client
                .Start()
                .map_err(|e| CaptureError::Init(format!("IAudioClient::Start: {e}")))?;

            Ok(Self {
                client,
                capture,
                device_rate,
                device_channels,
                kind,
                frame_bytes,
                target_rate,
                target_channels,
                pending: VecDeque::new(),
                prev: None,
                frac: 0.0,
            })
        }
    }

    /// Drain every queued loopback packet into `pending`, converting format,
    /// channel layout and sample rate. Returns whether any real audio arrived.
    fn pump(&mut self) -> bool {
        let mut got = false;
        unsafe {
            loop {
                let frames = match self.capture.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if frames == 0 {
                    break;
                }

                let mut data: *mut u8 = std::ptr::null_mut();
                let mut n_frames: u32 = 0;
                let mut flags: u32 = 0;
                if self
                    .capture
                    .GetBuffer(&mut data, &mut n_frames, &mut flags, None, None)
                    .is_err()
                {
                    break;
                }
                got = true;

                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT != 0;
                for f in 0..n_frames as usize {
                    let frame = if silent || data.is_null() {
                        vec![0.0f64; self.target_channels]
                    } else {
                        let base = data.add(f * self.frame_bytes);
                        self.downmix(base)
                    };
                    self.resample_push(&frame);
                }

                let _ = self.capture.ReleaseBuffer(n_frames);
            }
        }
        got
    }

    /// Read one device frame at `base` and map it to a `target_channels` frame.
    unsafe fn downmix(&self, base: *const u8) -> Vec<f64> {
        let read = |ch: usize| -> f64 {
            unsafe {
                match self.kind {
                    SampleKind::F32 => {
                        let p = base.add(ch * 4) as *const f32;
                        p.read_unaligned() as f64
                    }
                    SampleKind::I16 => {
                        let p = base.add(ch * 2) as *const i16;
                        p.read_unaligned() as f64 / 32768.0
                    }
                }
            }
        };
        let dc = self.device_channels;
        if self.target_channels == 1 {
            // Mono: average all device channels.
            let sum: f64 = (0..dc).map(read).sum();
            return vec![sum / dc as f64];
        }
        (0..self.target_channels)
            .map(|c| read(c.min(dc - 1)))
            .collect()
    }

    /// Streaming linear resampler: feed one device-rate frame, emit zero or more
    /// target-rate frames into `pending`.
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

impl Drop for WasapiCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

impl AudioCapture for WasapiCapture {
    fn read(&mut self, buf: &mut [f64]) -> Result<usize, CaptureError> {
        // Fill the whole buffer (the caller pushes all of it to the ring). Loop
        // pumping packets, but cap the wait to the real-time span this chunk
        // represents so an idle device (no loopback packets) yields silence at a
        // steady cadence instead of spinning or blocking forever.
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
        Ok(buf.len())
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
        tag = if subformat == SUBTYPE_IEEE_FLOAT {
            WAVE_FORMAT_IEEE_FLOAT
        } else {
            // Anything non-float we treat as PCM below, keyed on bit depth.
            0x0001
        };
    }

    let kind = match (tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => Some(SampleKind::F32),
        (0x0001, 16) => Some(SampleKind::I16),
        _ => None,
    };
    (rate, channels, kind, frame_bytes)
}
