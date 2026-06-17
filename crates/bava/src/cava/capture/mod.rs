//! Audio capture backends.
//!
//! cavacore needs a stream of interleaved PCM samples. On Linux the natural
//! source is the *monitor* of the default output sink, which carries whatever is
//! currently playing (spotifyd, browsers, etc.). [`AudioCapture`] abstracts over
//! the capture mechanism so additional backends (e.g. native PipeWire) can be
//! added later; today only [`pulse`] is implemented.

pub mod pulse;

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
