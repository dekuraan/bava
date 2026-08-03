// SPDX-License-Identifier: MIT OR Apache-2.0
//! Browser audio capture.
//!
//! There is no loopback device on the web, so the page supplies the audio
//! instead: `web/bava.js` asks for a tab share (`getDisplayMedia` with
//! `audio: true`), runs the resulting `MediaStreamTrack` through an
//! `AudioWorklet`, and pushes each block of interleaved samples in here via
//! [`bava_push_audio`]. The [`drain`] system then moves them into the same
//! [`AudioRing`](super::super::AudioRing) the native capture thread feeds, so
//! everything downstream — chunking, the plan rebuild to the negotiated rate,
//! `feed_cava` — is identical to the desktop build.
//!
//! This is why the web build has no [`AudioCapture`](super::AudioCapture) impl:
//! that trait is a *blocking pull*, and wasm32-unknown-unknown has no thread to
//! block on. Audio is pushed from JS on the main thread instead.

use std::collections::VecDeque;
use std::sync::Mutex;

use wasm_bindgen::prelude::wasm_bindgen;

/// Samples handed over by the page, waiting for the next [`drain`].
struct Incoming {
    samples: VecDeque<f64>,
    /// Format of the `AudioContext` the page built — typically 48 kHz, whatever
    /// the output device runs at. Zero until the first push.
    rate: u32,
    channels: u32,
}

/// The hand-off buffer. A plain `Mutex` is right even though the shipped wasm is
/// single-threaded: it is never contended (JS pushes and Bevy drains on the same
/// thread, never re-entrantly), and it keeps this sound if the build ever moves
/// the worklet to a worker with shared memory.
static INCOMING: Mutex<Incoming> = Mutex::new(Incoming {
    samples: VecDeque::new(),
    rate: 0,
    channels: 0,
});

/// Upper bound on the hand-off backlog, in samples. The buffer only grows while
/// the render loop is not draining it — a backgrounded tab, where
/// `requestAnimationFrame` stops but the `AudioWorklet` keeps running. Without a
/// cap that leaks for as long as the tab stays hidden; with it, returning to the
/// tab resumes on live audio instead of replaying minutes of backlog.
const MAX_BACKLOG: usize = 96_000 * 2;

/// Push a block of interleaved PCM from the page's `AudioWorklet`.
///
/// `samples` is interleaved `[L, R, L, R, …]` (or mono) in the `-1.0..=1.0`
/// range Web Audio uses, `channels` its stride, and `rate` the `AudioContext`
/// sample rate. Called from JS on every worklet block; it must stay cheap.
#[wasm_bindgen]
pub fn bava_push_audio(samples: &[f32], channels: u32, rate: u32) {
    // A poisoned lock would mean a panic inside a previous push. Recover rather
    // than propagate: the app is still perfectly able to visualize.
    let mut incoming = match INCOMING.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    incoming.rate = rate;
    incoming.channels = channels;
    incoming.samples.extend(samples.iter().map(|&s| s as f64));
    let overflow = incoming.samples.len().saturating_sub(MAX_BACKLOG);
    if overflow > 0 {
        incoming.samples.drain(..overflow);
    }
}

/// Discard anything buffered — called from JS when a capture stops or is
/// replaced, so a new stream doesn't start by replaying the old one's tail.
#[wasm_bindgen]
pub fn bava_reset_audio() {
    if let Ok(mut incoming) = INCOMING.lock() {
        incoming.samples.clear();
    }
}

/// Move everything the page has pushed into `out`, reporting the stream's
/// `(rate, channels)` — both zero until the first push.
pub fn take(out: &mut VecDeque<f64>) -> (u32, u32) {
    let mut incoming = match INCOMING.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    out.extend(incoming.samples.drain(..));
    (incoming.rate, incoming.channels)
}
