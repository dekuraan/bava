//! MPRIS now-playing + album-art integration.
//!
//! A background thread polls the active MPRIS player (e.g. spotifyd) over D-Bus,
//! publishing track metadata into the [`NowPlaying`] resource and fetching +
//! decoding album art into the [`AlbumArt`] resource. The Bevy systems here only
//! drain a channel, so D-Bus and HTTP never block the render loop.

use std::io::Read;
use std::thread;
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crossbeam_channel::{Receiver, Sender};

/// Current track metadata from the active MPRIS player.
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

/// Messages from the MPRIS thread to the Bevy world.
enum MprisMsg {
    Track(NowPlaying),
    Art(Option<DecodedArt>),
}

/// Bevy-side receiver. crossbeam's `Receiver` is `Send + Sync`.
#[derive(Resource)]
struct MprisRx(Receiver<MprisMsg>);

/// Polls MPRIS and serves now-playing + album art.
pub struct MprisPlugin;

impl Plugin for MprisPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = crossbeam_channel::unbounded();
        app.init_resource::<NowPlaying>()
            .init_resource::<AlbumArt>()
            .insert_resource(MprisRx(rx))
            .add_systems(Update, apply_mpris_updates);

        thread::Builder::new()
            .name("bava-mpris".into())
            .spawn(move || mpris_loop(tx))
            .expect("failed to spawn mpris thread");
    }
}

/// Drain MPRIS messages and update resources / create art textures.
fn apply_mpris_updates(
    rx: Res<MprisRx>,
    mut now_playing: ResMut<NowPlaying>,
    mut album_art: ResMut<AlbumArt>,
    mut images: ResMut<Assets<Image>>,
) {
    while let Ok(msg) = rx.0.try_recv() {
        match msg {
            MprisMsg::Track(track) => {
                if track.title != now_playing.title || track.artist != now_playing.artist {
                    info!(
                        "bava: now playing — {} · {}",
                        track.title.as_deref().unwrap_or("?"),
                        track.artist.as_deref().unwrap_or("?")
                    );
                }
                *now_playing = track;
            }
            MprisMsg::Art(Some(art)) => {
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
            MprisMsg::Art(None) => {
                album_art.image = None;
                album_art.size = None;
            }
        }
    }
}

/// Background poll loop. Tolerant of a missing D-Bus / no active player.
fn mpris_loop(tx: Sender<MprisMsg>) {
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
            let track = NowPlaying {
                title: metadata.title().map(str::to_owned),
                artist: metadata
                    .artists()
                    .and_then(|a| a.first().map(|s| s.to_string())),
                album: metadata.album_name().map(str::to_owned),
                art_url: metadata.art_url().map(str::to_owned),
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

/// Fetch album art from an `http(s)://` or `file://` URL and decode to RGBA8.
fn fetch_and_decode_art(url: &str) -> Option<DecodedArt> {
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read(path).ok()?
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let resp = ureq::get(url).call().ok()?;
        let mut buf = Vec::new();
        resp.into_reader().take(16 * 1024 * 1024).read_to_end(&mut buf).ok()?;
        buf
    } else {
        return None;
    };

    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(DecodedArt {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}
