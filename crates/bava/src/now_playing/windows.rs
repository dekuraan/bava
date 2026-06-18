// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows now-playing backend: the System Media Transport Controls.
//!
//! Polls the current `GlobalSystemMediaTransportControlsSession` — the same
//! session that drives the OS media flyout — for title/artist/album, and reads
//! the session's embedded thumbnail stream for album art. Art arrives as raw
//! encoded bytes (no URL), decoded through the shared [`decode_art_bytes`].

use std::thread;
use std::time::Duration;

use bevy::prelude::*;
use crossbeam_channel::Sender;
use windows::core::HSTRING;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as MediaProperties,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::{decode_art_bytes, DecodedArt, NowPlayingMsg, NowPlaying};

/// Background poll loop. Tolerant of there being no current session.
pub(super) fn run(tx: Sender<NowPlayingMsg>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let manager = match SessionManager::RequestAsync().and_then(|op| op.join()) {
        Ok(m) => m,
        Err(e) => {
            warn!("bava: SMTC unavailable ({e}); now-playing disabled");
            return;
        }
    };

    // Refetch art only when the track identity changes.
    let mut last_key: Option<(Option<String>, Option<String>, Option<String>)> = None;

    loop {
        match read_session(&manager) {
            Some((track, props)) => {
                let key = (track.title.clone(), track.artist.clone(), track.album.clone());
                let _ = tx.send(NowPlayingMsg::Track(track));
                // Reading + decoding the thumbnail is expensive, so only do it
                // when the track actually changed, not on every poll.
                if last_key.as_ref() != Some(&key) {
                    last_key = Some(key);
                    let art = props.Thumbnail().ok().and_then(read_thumbnail);
                    let _ = tx.send(NowPlayingMsg::Art(art));
                }
            }
            None => {
                // No session right now; clear state once.
                if last_key.take().is_some() {
                    let _ = tx.send(NowPlayingMsg::Track(NowPlaying::default()));
                    let _ = tx.send(NowPlayingMsg::Art(None));
                }
            }
        }

        thread::sleep(Duration::from_millis(500));
    }
}

/// Read the current session's metadata, returning it alongside the raw
/// `MediaProperties` so the caller can lazily read the thumbnail only when the
/// track changed. `None` if there is no current session or it can't be read.
fn read_session(manager: &SessionManager) -> Option<(NowPlaying, MediaProperties)> {
    let session = manager.GetCurrentSession().ok()?;
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;

    let track = NowPlaying {
        title: props.Title().ok().and_then(hstring_opt),
        artist: props.Artist().ok().and_then(hstring_opt),
        album: props.AlbumTitle().ok().and_then(hstring_opt),
        // SMTC exposes no art URL; the thumbnail is read separately.
        art_url: None,
    };

    Some((track, props))
}

/// Map an `HSTRING` to `Some(String)`, treating empty as absent.
fn hstring_opt(h: HSTRING) -> Option<String> {
    if h.is_empty() {
        None
    } else {
        Some(h.to_string())
    }
}

/// Open a thumbnail stream reference, read all its bytes, and decode to RGBA8.
fn read_thumbnail(reference: IRandomAccessStreamReference) -> Option<DecodedArt> {
    let stream = reference.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 {
        return None;
    }

    let reader = DataReader::CreateDataReader(&stream).ok()?;
    // Wait for the reader to buffer `size` bytes from the stream.
    reader.LoadAsync(size).ok()?.join().ok()?;

    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    decode_art_bytes(&buf)
}
