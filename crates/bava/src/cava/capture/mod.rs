// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio capture backends.
//!
//! Shared utilities used by WASAPI and Core Audio backends:
//!
//! cavacore needs a stream of interleaved PCM samples. The natural source is
//! whatever is currently playing on the default output: on Linux the *monitor*
//! of the default sink (via PulseAudio), on Windows a WASAPI *loopback* capture
//! of the default render endpoint, on macOS a Core Audio *process tap* of the
//! system mix. [`AudioCapture`] abstracts over the mechanism; [`open`] selects
//! the backend for the current platform.

#[cfg(target_os = "linux")]
pub mod pulse;

#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub mod pipewire;

#[cfg(target_os = "windows")]
pub mod wasapi;

#[cfg(target_os = "macos")]
pub mod coreaudio;

/// Open the platform's default capture backend as a boxed [`AudioCapture`].
///
/// `device` optionally pins a source (a PulseAudio source name on Linux; ignored
/// on Windows, which always loops back the default render endpoint). `rate` /
/// `channels` are the format cavacore was planned for; backends that cannot force
/// a format (WASAPI shared loopback) resample/convert to match. `frame_samples`
/// is the per-channel read size the caller will use.
pub fn open(
    device: Option<&str>,
    rate: u32,
    channels: u8,
    frame_samples: usize,
) -> Result<Box<dyn AudioCapture>, CaptureError> {
    #[cfg(target_os = "linux")]
    {
        // Prefer the native PipeWire backend (follows the default sink + negotiates
        // format); fall back to PulseAudio (real PulseAudio or pipewire-pulse) when
        // PipeWire isn't running or the feature is disabled.
        #[cfg(feature = "pipewire")]
        {
            match pipewire::PipeWireCapture::open(device, rate, channels, frame_samples) {
                Ok(cap) => return Ok(Box::new(cap)),
                Err(e) => bevy::log::warn!(
                    "bava: native PipeWire capture unavailable ({e}); falling back to PulseAudio"
                ),
            }
        }
        let cap = pulse::PulseCapture::open(device, rate, channels, frame_samples)?;
        Ok(Box::new(cap))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = device;
        let cap = wasapi::WasapiCapture::open(rate, channels, frame_samples)?;
        Ok(Box::new(cap))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = device;
        let cap = coreaudio::CoreAudioCapture::open(rate, channels, frame_samples)?;
        Ok(Box::new(cap))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (device, rate, channels, frame_samples);
        Err(CaptureError::Init(
            "no audio capture backend for this platform".into(),
        ))
    }
}

/// A blocking source of interleaved PCM audio for cavacore.
///
/// Implementations are moved onto a dedicated capture thread, so they only need
/// to be [`Send`].
pub trait AudioCapture: Send {
    /// Fill the **entire** `buf` with interleaved `f64` samples, blocking as
    /// needed. Backends capturing a live stream (PulseAudio monitor, WASAPI
    /// loopback) never end, so there is no end-of-stream signal; an idle source
    /// must pad with silence to keep a steady cadence rather than short-read.
    /// Errors are transient by convention; the caller may log and retry.
    fn read(&mut self, buf: &mut [f64]) -> Result<(), CaptureError>;

    /// Sample rate of the captured stream, in Hz.
    fn rate(&self) -> u32;

    /// Number of interleaved channels (1 or 2).
    fn channels(&self) -> u32;
}

/// Error type shared by capture backends.
#[derive(Debug)]
pub enum CaptureError {
    /// The backend could not be initialized (no server, bad device, ...).
    Init(String),
    /// A read failed; usually transient.
    // Only constructed by the PulseAudio backend; retained for completeness.
    #[allow(dead_code)]
    Read(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Init(m) => write!(f, "capture init failed: {m}"),
            CaptureError::Read(m) => write!(f, "capture read failed: {m}"),
        }
    }
}

impl std::error::Error for CaptureError {}

// Used by the WASAPI (Windows) and Core Audio (macOS) backends. Compiled
// unconditionally so rust-analyzer type-checks it on Linux; the dead_code lint
// is suppressed only where it is actually unreachable.
#[cfg_attr(
    not(any(target_os = "windows", target_os = "macos")),
    allow(dead_code)
)]
pub(super) struct LinearResampler {
    target_channels: usize,
    /// Previous device-rate frame (already down/up-mixed to target_channels).
    prev: Vec<f64>,
    has_prev: bool,
    /// Fractional position within the current input interval [0, 1).
    frac: f64,
}

#[cfg_attr(
    not(any(target_os = "windows", target_os = "macos")),
    allow(dead_code)
)]
impl LinearResampler {
    pub(super) fn new(target_channels: usize) -> Self {
        Self {
            target_channels,
            prev: vec![0.0; target_channels],
            has_prev: false,
            frac: 0.0,
        }
    }

    /// Clear interpolation state (call after a device format/rate change).
    #[allow(dead_code)]
    pub(super) fn reset(&mut self) {
        self.has_prev = false;
        self.frac = 0.0;
    }

    /// Feed one device-rate mixed frame and push zero or more target-rate
    /// frames into `pending`.
    ///
    /// `step = device_rate as f64 / target_rate as f64`; pass `1.0` when
    /// rates are equal to skip interpolation entirely.
    pub(super) fn push(
        &mut self,
        step: f64,
        cur: &[f64],
        pending: &mut std::collections::VecDeque<f64>,
    ) {
        if step == 1.0 {
            pending.extend(cur.iter().copied());
            return;
        }
        if self.has_prev {
            while self.frac < 1.0 {
                for c in 0..self.target_channels {
                    pending.push_back(self.prev[c] + (cur[c] - self.prev[c]) * self.frac);
                }
                self.frac += step;
            }
            self.frac -= 1.0;
        }
        self.prev.clear();
        self.prev.extend_from_slice(cur);
        self.has_prev = true;
    }
}
