// SPDX-License-Identifier: MIT OR Apache-2.0
//! Browser now-playing.
//!
//! There is no media session to poll: a page cannot read metadata out of a
//! cross-origin `<iframe>`, and the captured `MediaStream` carries audio only.
//! So the page pushes instead — `web/bava.js` reads the title from the YouTube
//! IFrame Player API and fetches the video's thumbnail, then calls
//! [`bava_set_now_playing`] / [`bava_set_album_art`].
//!
//! Art decoding (and the palette extraction the dynamic colors ride on) happens
//! on the main thread here rather than a worker, since wasm32-unknown-unknown
//! has no threads. It runs once per track change, not per frame.

use std::sync::Mutex;

use bevy::prelude::*;
use wasm_bindgen::prelude::wasm_bindgen;

use super::{NowPlaying, NowPlayingMsg, decode_art_bytes};

/// Metadata pushed by the page since the last drain. Only the newest of each
/// kind matters, so this collapses rather than queues: a track skipped through
/// three videos before the app next runs should land on the third.
struct Pending {
    track: Option<NowPlaying>,
    /// `Some(None)` means "art was explicitly cleared", distinct from `None`
    /// ("nothing new"), so switching to a video whose thumbnail failed to load
    /// drops the previous cover instead of keeping it.
    art: Option<Option<Vec<u8>>>,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    track: None,
    art: None,
});

/// Set the current track. Called from JS whenever the embedded player reports
/// new metadata (or the user loads a different video).
#[wasm_bindgen]
pub fn bava_set_now_playing(title: Option<String>, artist: Option<String>, album: Option<String>) {
    if let Ok(mut pending) = PENDING.lock() {
        pending.track = Some(NowPlaying {
            title: title.filter(|s| !s.is_empty()),
            artist: artist.filter(|s| !s.is_empty()),
            album: album.filter(|s| !s.is_empty()),
            art_url: None,
        });
    }
}

/// Set (or with an empty slice, clear) the cover art as encoded JPEG/PNG bytes.
/// The page fetches the thumbnail itself so that a CORS failure is a JS problem
/// with a clear console message, not an opaque decode error in here.
#[wasm_bindgen]
pub fn bava_set_album_art(bytes: &[u8]) {
    if let Ok(mut pending) = PENDING.lock() {
        pending.art = Some(if bytes.is_empty() {
            None
        } else {
            Some(bytes.to_vec())
        });
    }
}

/// Drain what the page pushed into the channel the plugin's systems already
/// read. Replaces the platform poll thread, which wasm has no way to spawn.
pub fn pump(tx: &crossbeam_channel::Sender<NowPlayingMsg>) {
    let Ok(mut pending) = PENDING.lock() else {
        return;
    };
    if let Some(track) = pending.track.take() {
        let _ = tx.send(NowPlayingMsg::Track(track));
    }
    if let Some(art) = pending.art.take() {
        // Drop the lock before decoding: `decode_art_bytes` also runs the
        // palette extraction, and holding it across that would block any push
        // JS makes in the meantime.
        drop(pending);
        let decoded = art.as_deref().and_then(decode_art_bytes);
        if decoded.is_none() && art.is_some() {
            warn!("bava: could not decode the pushed album art; keeping no cover");
        }
        let _ = tx.send(NowPlayingMsg::Art(decoded));
    }
}
