// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux now-playing backend: the active MPRIS player over D-Bus.
//!
//! Polls whichever MPRIS player is currently active (spotifyd, a browser, …) and
//! fetches album art from its `mpris:artUrl` (`http(s)://` or `file://`), with a
//! YouTube thumbnail fallback for players that expose no art URL.

use std::io::Read;
use std::thread;
use std::time::Duration;

use bevy::prelude::*;
use crossbeam_channel::Sender;

use super::{decode_art_bytes, DecodedArt, MprisMsg, NowPlaying};

/// Background poll loop. Tolerant of a missing D-Bus / no active player.
pub(super) fn run(tx: Sender<MprisMsg>) {
    use mpris::PlayerFinder;

    let finder = match PlayerFinder::new() {
        Ok(f) => f,
        Err(e) => {
            warn!("bava: MPRIS unavailable ({e}); now-playing disabled");
            return;
        }
    };

    let mut last_art_url: Option<String> = None;

    loop {
        // Pick whichever player is currently active (spotifyd, a browser, ...).
        let player = match finder.find_active() {
            Ok(p) => p,
            Err(_) => {
                // No player right now; clear state once and wait.
                if last_art_url.take().is_some() {
                    let _ = tx.send(MprisMsg::Track(NowPlaying::default()));
                    let _ = tx.send(MprisMsg::Art(None));
                }
                thread::sleep(Duration::from_millis(750));
                continue;
            }
        };

        if let Ok(metadata) = player.get_metadata() {
            // Prefer the player's own art; fall back to a derived thumbnail for
            // players that don't expose mpris:artUrl (e.g. Firefox/YouTube).
            let art_url = metadata
                .art_url()
                .map(str::to_owned)
                .or_else(|| metadata.url().and_then(youtube_thumbnail));

            let track = NowPlaying {
                title: metadata.title().map(str::to_owned),
                artist: metadata
                    .artists()
                    .and_then(|a| a.first().map(|s| s.to_string())),
                album: metadata.album_name().map(str::to_owned),
                art_url,
            };

            let art_url = track.art_url.clone();
            let _ = tx.send(MprisMsg::Track(track));

            // Only refetch art when the URL changes.
            if art_url != last_art_url {
                last_art_url = art_url.clone();
                match art_url.as_deref().and_then(fetch_and_decode_art) {
                    Some(art) => {
                        let _ = tx.send(MprisMsg::Art(Some(art)));
                    }
                    None => {
                        let _ = tx.send(MprisMsg::Art(None));
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(500));
    }
}

/// Derive a thumbnail image URL from a YouTube watch/short URL, so YouTube
/// playback (which exposes no `mpris:artUrl`) still gets a backdrop.
fn youtube_thumbnail(page_url: &str) -> Option<String> {
    if !(page_url.contains("youtube.com") || page_url.contains("youtu.be")) {
        return None;
    }
    // Handle `…watch?v=ID&…` and `youtu.be/ID?…` forms.
    let id = page_url
        .split_once("v=")
        .map(|(_, rest)| rest)
        .or_else(|| page_url.rsplit_once('/').map(|(_, rest)| rest))?;
    let id = id.split(['&', '?', '#']).next().unwrap_or(id);
    if id.is_empty() {
        return None;
    }
    Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"))
}

/// Fetch album art from an `http(s)://` or `file://` URL and decode to RGBA8.
fn fetch_and_decode_art(url: &str) -> Option<DecodedArt> {
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read(path).ok()?
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let resp = ureq::get(url).call().ok()?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(16 * 1024 * 1024)
            .read_to_end(&mut buf)
            .ok()?;
        buf
    } else {
        return None;
    };

    decode_art_bytes(&bytes)
}
