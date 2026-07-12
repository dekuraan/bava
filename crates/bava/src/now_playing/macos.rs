// SPDX-License-Identifier: MIT OR Apache-2.0
//! macOS now-playing backend: the MediaRemote adapter.
//!
//! macOS has no public system-wide now-playing API, and as of macOS 15.4 the
//! private `MediaRemote.framework` rejects unentitled callers. The robust
//! workaround (ungive/mediaremote-adapter) drives MediaRemote through the
//! *entitled* system `/usr/bin/perl`, which loads a small helper framework and
//! streams now-playing updates as JSON lines on stdout. We spawn that and parse
//! the stream — no private linking, works on 15.4+ without disabling SIP.
//!
//! Each stdout line is `{ "type": "data", "payload": { … } }`. We run the
//! adapter with `--no-diff`, so every line is a full snapshot of the current
//! track (or an empty payload when nothing is playing). Album art arrives inline
//! as base64 `artworkData`; we decode it through the shared [`decode_art_bytes`]
//! and only re-decode when the track identity changes.
//!
//! The adapter (`mediaremote-adapter.pl` + `MediaRemoteAdapter.framework`) must be
//! located at runtime; see [`locate_adapter`] for the search order.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use base64::Engine;
use bevy::prelude::*;
use crossbeam_channel::Sender;
use serde::Deserialize;

use super::{decode_art_bytes, NowPlayingMsg, NowPlaying};

/// One line of the adapter's `stream` output.
#[derive(Deserialize)]
struct StreamLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: Payload,
}

/// The now-playing fields we care about (the adapter emits many more).
#[derive(Deserialize, Default)]
struct Payload {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    #[serde(rename = "artworkData")]
    artwork_data: Option<String>,
}

/// Background loop: spawn the adapter, parse its stream, respawn if it dies.
pub(super) fn run(tx: Sender<NowPlayingMsg>) {
    let Some((script, framework)) = locate_adapter() else {
        warn!(
            "bava: mediaremote-adapter not found; now-playing disabled. \
             Set BAVA_MEDIAREMOTE_ADAPTER_DIR to a directory containing \
             mediaremote-adapter.pl and MediaRemoteAdapter.framework"
        );
        return;
    };

    loop {
        if !stream_once(&script, &framework, &tx) {
            // Spawn failed outright (missing perl, bad paths); stop retrying so we
            // don't spin. A clean child exit (player quit, adapter restart) falls
            // through to the retry sleep below instead.
            return;
        }
        // Child ended (stream closed). Clear state and retry after a short wait.
        let _ = tx.send(NowPlayingMsg::Track(NowPlaying::default()));
        let _ = tx.send(NowPlayingMsg::Art(None));
        thread::sleep(Duration::from_secs(2));
    }
}

/// Run one adapter process to completion, forwarding updates. Returns `false`
/// only if the process could not be spawned at all.
fn stream_once(script: &PathBuf, framework: &PathBuf, tx: &Sender<NowPlayingMsg>) -> bool {
    let mut child = match Command::new("/usr/bin/perl")
        .arg(script)
        .arg(framework)
        .arg("stream")
        .arg("--no-diff")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("bava: failed to launch mediaremote-adapter ({e}); now-playing disabled");
            return false;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return false;
    };

    // Track identity of the last art we decoded, so we don't re-decode the same
    // base64 on every (play/pause/elapsed) snapshot for one track. Artwork
    // presence is part of the key: MediaRemote populates `artworkData`
    // asynchronously, so a track's first snapshot routinely has none and
    // keying on identity alone would latch `Art(None)` for the whole track.
    let mut last_key: Option<(Option<String>, Option<String>, Option<String>, bool)> = None;

    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: StreamLine = match serde_json::from_str(line) {
            Ok(p) => p,
            // Non-data lines (status/errors) and partial reads are skipped.
            Err(_) => continue,
        };
        if parsed.kind != "data" {
            continue;
        }
        let p = parsed.payload;

        let track = NowPlaying {
            title: p.title.clone(),
            artist: p.artist.clone(),
            album: p.album.clone(),
            // Art is delivered inline as base64, not via a URL.
            art_url: None,
        };
        let key = (
            track.title.clone(),
            track.artist.clone(),
            track.album.clone(),
            p.artwork_data.is_some(),
        );
        let _ = tx.send(NowPlayingMsg::Track(track));

        if last_key.as_ref() != Some(&key) {
            last_key = Some(key);
            let art = p.artwork_data.as_deref().and_then(decode_artwork);
            let _ = tx.send(NowPlayingMsg::Art(art));
        }
    }

    // Reap the child so we don't leave a zombie before respawning.
    let _ = child.wait();
    true
}

/// Decode a base64 `artworkData` string to an RGBA8 texture, ignoring oversized
/// or malformed payloads.
fn decode_artwork(b64: &str) -> Option<super::DecodedArt> {
    // Guard against a pathologically large field before allocating the decode.
    if b64.len() > 32 * 1024 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    decode_art_bytes(&bytes)
}

/// Find the adapter's Perl script and helper framework. Search order:
///
/// 1. `$BAVA_MEDIAREMOTE_ADAPTER_DIR` — explicit override.
/// 2. `<exe dir>/../Resources` — the app-bundle layout (`bava.app/Contents/…`).
/// 3. `<exe dir>` — alongside the binary (dev / portable builds).
///
/// A directory matches only if it contains *both* `mediaremote-adapter.pl` and
/// `MediaRemoteAdapter.framework`.
fn locate_adapter() -> Option<(PathBuf, PathBuf)> {
    const SCRIPT: &str = "mediaremote-adapter.pl";
    const FRAMEWORK: &str = "MediaRemoteAdapter.framework";

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("BAVA_MEDIAREMOTE_ADAPTER_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("../Resources"));
            candidates.push(dir.to_path_buf());
        }
    }

    candidates.into_iter().find_map(|dir| {
        let script = dir.join(SCRIPT);
        let framework = dir.join(FRAMEWORK);
        (script.is_file() && framework.exists()).then_some((script, framework))
    })
}
