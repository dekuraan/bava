// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio capture backends.
//!
//! cavacore needs a stream of interleaved PCM samples. The natural source is
//! whatever is currently playing on the default output: on Linux the *monitor*
//! of the default sink (via PulseAudio), on Windows a WASAPI *loopback* capture
//! of the default render endpoint. [`AudioCapture`] abstracts over the mechanism;
//! [`open`] selects the backend for the current platform.

#[cfg(target_os = "linux")]
pub mod pulse;

#[cfg(target_os = "windows")]
pub mod wasapi;

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
        let cap = pulse::PulseCapture::open(device, rate, channels, frame_samples)?;
        Ok(Box::new(cap))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = device;
        let cap = wasapi::WasapiCapture::open(rate, channels, frame_samples)?;
        Ok(Box::new(cap))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
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
    /// Block until at least one frame is available, then fill `buf` with as many
    /// interleaved `f64` samples as possible and return how many were written.
    ///
    /// Returns `Ok(0)` only on end-of-stream. Errors are transient by
    /// convention; the caller may log and retry.
    fn read(&mut self, buf: &mut [f64]) -> Result<usize, CaptureError>;

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
