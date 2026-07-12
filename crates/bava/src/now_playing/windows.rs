// SPDX-License-Identifier: MIT OR Apache-2.0
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

    // Refetch art only when the track identity — or thumbnail presence —
    // changes. Presence is part of the key because GSMTC populates thumbnails
    // asynchronously: the first snapshot of a track routinely has none, and
    // keying on identity alone would latch `Art(None)` for the whole track.
    let mut last_key: Option<(Option<String>, Option<String>, Option<String>, bool)> = None;

    loop {
        match read_session(&manager) {
            SessionRead::Track(track, props) => {
                let thumb = props.Thumbnail().ok();
                let key = (
                    track.title.clone(),
                    track.artist.clone(),
                    track.album.clone(),
                    thumb.is_some(),
                );
                let _ = tx.send(NowPlayingMsg::Track(track));
                // Reading + decoding the thumbnail is expensive, so only do it
                // when the track (or thumbnail availability) actually changed.
                if last_key.as_ref() != Some(&key) {
                    last_key = Some(key);
                    let art = thumb.and_then(read_thumbnail);
                    let _ = tx.send(NowPlayingMsg::Art(art));
                }
            }
            SessionRead::NoSession => {
                // No session right now; clear state once.
                if last_key.take().is_some() {
                    let _ = tx.send(NowPlayingMsg::Track(NowPlaying::default()));
                    let _ = tx.send(NowPlayingMsg::Art(None));
                }
            }
            // Keep the current HUD; the next poll usually succeeds.
            SessionRead::Transient => {}
        }

        thread::sleep(Duration::from_millis(500));
    }
}

/// One poll of the current session.
enum SessionRead {
    /// Metadata read OK; the raw `MediaProperties` ride along so the caller
    /// can lazily read the thumbnail only when the track changed.
    Track(NowPlaying, MediaProperties),
    /// There is no current media session — the HUD should clear.
    NoSession,
    /// A session exists but reading its properties failed. This happens
    /// routinely for one poll during track transitions; blanking the HUD for
    /// it would flicker (and force a pointless thumbnail re-decode after).
    Transient,
}

/// Read the current session's metadata.
fn read_session(manager: &SessionManager) -> SessionRead {
    let Ok(session) = manager.GetCurrentSession() else {
        return SessionRead::NoSession;
    };
    let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(|op| op.join()) else {
        return SessionRead::Transient;
    };

    let track = NowPlaying {
        title: props.Title().ok().and_then(hstring_opt),
        artist: props.Artist().ok().and_then(hstring_opt),
        album: props.AlbumTitle().ok().and_then(hstring_opt),
        // SMTC exposes no art URL; the thumbnail is read separately.
        art_url: None,
    };

    SessionRead::Track(track, props)
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
