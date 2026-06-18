// SPDX-License-Identifier: GPL-3.0-or-later
//! Now-playing + album-art integration.
//!
//! A background thread polls the active media session — MPRIS over D-Bus on
//! Linux ([`linux`]), the System Media Transport Controls on Windows
//! ([`windows`]), or the MediaRemote adapter on macOS ([`macos`]) — publishing
//! track metadata into the [`NowPlaying`] resource and decoded album art into the
//! [`AlbumArt`] resource. The Bevy systems here only drain a channel, so the
//! platform APIs never block the render loop.

use std::thread;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crossbeam_channel::Receiver;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// The platform now-playing poll loop. Each backend owns its own polling cadence
/// and sends [`NowPlayingMsg`]s until the process exits.
#[cfg(target_os = "linux")]
use linux::run as now_playing_run;
#[cfg(target_os = "macos")]
use macos::run as now_playing_run;
#[cfg(target_os = "windows")]
use windows::run as now_playing_run;

/// Fallback for platforms without a media-session backend.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn now_playing_run(_tx: crossbeam_channel::Sender<NowPlayingMsg>) {
    warn!("bava: now-playing unsupported on this platform");
}

/// Current track metadata from the active media session.
#[derive(Resource, Default, Debug, Clone)]
pub struct NowPlaying {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Raw `mpris:artUrl`, retained for debugging.
    pub art_url: Option<String>,
}

/// Handle to the decoded album-art texture, if any.
#[derive(Resource, Default)]
pub struct AlbumArt {
    pub image: Option<Handle<Image>>,
    /// Pixel dimensions of the current art (width, height).
    pub size: Option<(u32, u32)>,
}

/// Decoded RGBA8 album art produced off-thread.
struct DecodedArt {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// Messages from the now-playing thread to the Bevy world.
enum NowPlayingMsg {
    Track(NowPlaying),
    Art(Option<DecodedArt>),
}

/// Bevy-side receiver. crossbeam's `Receiver` is `Send + Sync`.
#[derive(Resource)]
struct NowPlayingRx(Receiver<NowPlayingMsg>);

/// Polls the platform media session and serves now-playing + album art.
pub struct NowPlayingPlugin;

impl Plugin for NowPlayingPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = crossbeam_channel::unbounded();
        app.init_resource::<NowPlaying>()
            .init_resource::<AlbumArt>()
            .insert_resource(NowPlayingRx(rx))
            .add_systems(Update, apply_now_playing_updates);

        thread::Builder::new()
            .name("bava-now-playing".into())
            .spawn(move || now_playing_run(tx))
            .expect("failed to spawn now-playing thread");
    }
}

/// Drain now-playing messages and update resources / create art textures.
fn apply_now_playing_updates(
    rx: Res<NowPlayingRx>,
    mut now_playing: ResMut<NowPlaying>,
    mut album_art: ResMut<AlbumArt>,
    mut images: ResMut<Assets<Image>>,
) {
    while let Ok(msg) = rx.0.try_recv() {
        match msg {
            NowPlayingMsg::Track(track) => {
                if track.title != now_playing.title || track.artist != now_playing.artist {
                    info!(
                        "bava: now playing — {} · {}",
                        track.title.as_deref().unwrap_or("?"),
                        track.artist.as_deref().unwrap_or("?")
                    );
                }
                *now_playing = track;
            }
            NowPlayingMsg::Art(Some(art)) => {
                let image = Image::new(
                    Extent3d {
                        width: art.width,
                        height: art.height,
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    art.rgba,
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::RENDER_WORLD,
                );
                album_art.image = Some(images.add(image));
                album_art.size = Some((art.width, art.height));
            }
            NowPlayingMsg::Art(None) => {
                album_art.image = None;
                album_art.size = None;
            }
        }
    }
}

/// Decode encoded image bytes (JPEG/PNG, from any source) to RGBA8. Shared by
/// both platform backends.
fn decode_art_bytes(bytes: &[u8]) -> Option<DecodedArt> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(DecodedArt {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}
