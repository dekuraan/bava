// SPDX-License-Identifier: MIT OR Apache-2.0
//! Now-playing + album-art integration.
//!
//! A background thread polls the active media session — MPRIS over D-Bus on
//! Linux ([`linux`]), the System Media Transport Controls on Windows
//! ([`windows`]), or the MediaRemote adapter on macOS ([`macos`]) — publishing
//! track metadata into the [`NowPlaying`] resource and decoded album art into the
//! [`AlbumArt`] resource. The Bevy systems here only drain a channel, so the
//! platform APIs never block the render loop.

#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use bevy::asset::RenderAssetUsages;
use bevy::color::Hsla;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crossbeam_channel::Receiver;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_arch = "wasm32")]
mod web;
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

/// Fallback for platforms without a media-session backend. The web build is not
/// one of them — it has no *thread* to poll from, but the page pushes metadata
/// in through [`web`] instead.
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_arch = "wasm32"
)))]
fn now_playing_run(_tx: crossbeam_channel::Sender<NowPlayingMsg>) {
    warn!("bava: now-playing unsupported on this platform");
}

/// Current track metadata from the active media session.
#[derive(Resource, Default, Debug, Clone)]
pub struct NowPlaying {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Raw `mpris:artUrl`, retained for debugging; read only on Linux.
    #[allow(dead_code)]
    pub art_url: Option<String>,
}

/// Handle to the decoded album-art texture, if any.
#[derive(Resource, Default)]
pub struct AlbumArt {
    pub image: Option<Handle<Image>>,
    /// Pixel dimensions of the current art (width, height).
    pub size: Option<(u32, u32)>,
    /// Accent colors extracted from the art (most vibrant first, up to
    /// [`MAX_DYNAMIC_COLORS`]), for the dynamic color profile. `None` when there
    /// is no art or extraction failed.
    pub colors: Option<Vec<Color>>,
    /// Downscaled, opaque copy of the art retained on the CPU so the HUD can
    /// re-blur the backdrop when the blur slider moves, without re-decoding.
    /// See [`downscale_for_blur`].
    pub small: Option<image::RgbaImage>,
}

/// Decoded RGBA8 album art produced off-thread.
struct DecodedArt {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    /// Accent colors extracted from the art on the decode thread.
    colors: Option<Vec<Color>>,
    /// Downscaled copy for the blurred backdrop, resized on the decode thread.
    small: image::RgbaImage,
}

/// Messages from the now-playing thread to the Bevy world.
enum NowPlayingMsg {
    Track(NowPlaying),
    Art(Option<DecodedArt>),
}

/// Bevy-side receiver. crossbeam's `Receiver` is `Send + Sync`.
#[derive(Resource)]
struct NowPlayingRx(Receiver<NowPlayingMsg>);

/// Keeps a sender alive for the app's lifetime in offline mode, where the
/// one-shot metadata thread exits immediately — without this the drained
/// channel reads as *disconnected* and logs a spurious "backend stopped".
#[derive(Resource)]
#[allow(dead_code)]
struct NowPlayingTxKeepAlive(crossbeam_channel::Sender<NowPlayingMsg>);

/// A fixed track for offline rendering (`--input`): metadata and embedded cover
/// art read from the audio file's tags, served once instead of polling the OS
/// media session.
#[derive(Clone, Default)]
pub struct OfflineTrack {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Encoded image bytes (e.g. an ID3 APIC frame), decoded off-thread like
    /// the live backends' art.
    pub art: Option<Vec<u8>>,
}

/// Polls the platform media session and serves now-playing + album art — or,
/// with [`offline`](Self::offline) set, serves a fixed track's tags instead.
#[derive(Default)]
pub struct NowPlayingPlugin {
    pub offline: Option<OfflineTrack>,
}

impl Plugin for NowPlayingPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = crossbeam_channel::unbounded();
        app.init_resource::<NowPlaying>()
            .init_resource::<AlbumArt>()
            .insert_resource(NowPlayingRx(rx))
            // PreUpdate, so metadata/art land before any Update system reads
            // them — in offline rendering that makes the HUD and dynamic
            // palette appear at a deterministic video frame (the first one).
            .add_systems(PreUpdate, apply_now_playing_updates);

        if let Some(track) = &self.offline {
            // Offline rendering: decode the cover and queue everything *now*,
            // synchronously. A background thread (like the live backends use)
            // would race the render loop — on a fast GPU the first frames of
            // every run would miss the art at a different, wall-clock-dependent
            // video timestamp. The tens-of-ms decode happens once, before any
            // frame renders.
            let track = track.clone();
            let _ = tx.send(NowPlayingMsg::Track(NowPlaying {
                title: track.title,
                artist: track.artist,
                album: track.album,
                art_url: None,
            }));
            let art = track.art.as_deref().and_then(decode_art_bytes);
            let _ = tx.send(NowPlayingMsg::Art(art));
            app.insert_resource(NowPlayingTxKeepAlive(tx));
            return;
        }

        // The web build has no thread to poll from: the page pushes metadata,
        // and a PreUpdate system forwards it down the same channel. The sender
        // has to be kept alive for the app's lifetime, or the drain would see a
        // disconnected channel on the very first frame.
        #[cfg(target_arch = "wasm32")]
        {
            app.insert_resource(NowPlayingTxKeepAlive(tx)).add_systems(
                PreUpdate,
                pump_web_now_playing.before(apply_now_playing_updates),
            );
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        thread::Builder::new()
            .name("bava-now-playing".into())
            .spawn(move || now_playing_run(tx))
            .expect("failed to spawn now-playing thread");
    }
}

/// Forward metadata the page pushed since last frame into the channel.
#[cfg(target_arch = "wasm32")]
fn pump_web_now_playing(keep_alive: Res<NowPlayingTxKeepAlive>) {
    web::pump(&keep_alive.0);
}

/// Drain now-playing messages and update resources / create art textures.
fn apply_now_playing_updates(
    rx: Res<NowPlayingRx>,
    mut warned: Local<bool>,
    mut now_playing: ResMut<NowPlaying>,
    mut album_art: ResMut<AlbumArt>,
    mut images: ResMut<Assets<Image>>,
) {
    loop {
        let msg = match rx.0.try_recv() {
            Ok(msg) => msg,
            Err(crossbeam_channel::TryRecvError::Empty) => break,
            // The backend thread ended (panic or a clean stop). Warn once so the
            // freeze isn't silent, then stop polling a dead channel.
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                if !*warned {
                    warn!(
                        "bava: now-playing backend stopped; metadata and album art will no longer update"
                    );
                    *warned = true;
                }
                break;
            }
        };
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
                if let Some(colors) = &art.colors {
                    debug!("bava: album colors — {colors:?}");
                }
                album_art.colors = art.colors;
                album_art.small = Some(art.small);
            }
            NowPlayingMsg::Art(None) => {
                album_art.image = None;
                album_art.size = None;
                album_art.colors = None;
                album_art.small = None;
            }
        }
    }
}

/// Decode encoded image bytes (JPEG/PNG, from any source) to RGBA8. Shared by
/// all platform backends. Also extracts the `(primary, secondary)` accent colors
/// here, on the (background) decode thread, so the render loop never pays for it.
fn decode_art_bytes(bytes: &[u8]) -> Option<DecodedArt> {
    // Check the header dimensions before decoding: the fetch paths cap the
    // *encoded* size, but a small PNG can declare enormous dimensions (a
    // 16 MiB file claiming 40 000×40 000 would allocate ~6.4 GB as RGBA8) —
    // and art URLs are fed by arbitrary MPRIS players. 8192² (256 MiB RGBA)
    // is far beyond any real cover.
    const MAX_ART_DIM: u32 = 8192;
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if w == 0 || h == 0 || w > MAX_ART_DIM || h > MAX_ART_DIM {
        warn!("bava: ignoring album art with implausible dimensions {w}x{h}");
        return None;
    }
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    // A zero-area / empty decode would build a degenerate texture and can panic
    // color_thief's quantizer — treat it as "no art".
    if width == 0 || height == 0 || rgba.is_empty() {
        return None;
    }
    let small = downscale_for_blur(&rgba);
    let pixels = rgba.into_raw();
    let colors = extract_palette(&pixels);
    Some(DecodedArt {
        rgba: pixels,
        width,
        height,
        colors,
        small,
    })
}

/// Longest side of the downscaled copy kept for the blurred backdrop.
///
/// The backdrop is a heavy blur stretched over the whole window, so any detail
/// finer than this is destroyed by the blur regardless — and blurring 256px
/// instead of a 3000px cover is what makes a *live* blur slider affordable
/// (the GPU's bilinear filtering does the upscale for free).
const ART_BLUR_SOURCE_MAX: u32 = 256;

/// Downscale album art to at most [`ART_BLUR_SOURCE_MAX`] on its longest side,
/// preserving aspect, and force it opaque. Runs on the decode thread.
fn downscale_for_blur(rgba: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = rgba.dimensions();
    let scale = (ART_BLUR_SOURCE_MAX as f32 / w.max(h) as f32).min(1.0);
    let sw = ((w as f32 * scale).round() as u32).max(1);
    let sh = ((h as f32 * scale).round() as u32).max(1);
    let mut small = if (sw, sh) == (w, h) {
        rgba.clone()
    } else {
        image::imageops::resize(rgba, sw, sh, image::imageops::FilterType::Triangle)
    };
    // `fast_blur` assumes premultiplied alpha, and a full-window backdrop wants
    // to be opaque anyway — so flatten transparency here rather than let it
    // bleed dark halos into the blur.
    for px in small.pixels_mut() {
        px.0[3] = u8::MAX;
    }
    small
}

/// Maximum number of accent colors extracted from album art (the dynamic-color
/// count slider clamps to this).
pub const MAX_DYNAMIC_COLORS: usize = 5;

/// Pick up to [`MAX_DYNAMIC_COLORS`] accent colors from album-art RGBA8 pixels,
/// most vibrant first and each visibly distinct in hue.
///
/// This follows the recipe common to media players (Android `Palette`,
/// Vibrant.js): quantize to a small palette via modified median-cut
/// (`color_thief`), then score candidates in HSL — favouring saturation,
/// mid-lightness, and dominance — and greedily pick the next color that balances
/// a high score with hue-distance from the ones already chosen. Falls back to
/// lightness-shifted variants of the primary for near-monochrome covers.
fn extract_palette(rgba: &[u8]) -> Option<Vec<Color>> {
    // quality 10 = sample every 10th pixel (fast, plenty for accent picking);
    // ask for up to 8 representative colors.
    let palette = color_thief::get_palette(rgba, color_thief::ColorFormat::Rgba, 10, 8).ok()?;
    if palette.is_empty() {
        return None;
    }
    let cands: Vec<(Color, Hsla)> = palette
        .iter()
        .map(|c| {
            let col = Color::srgb_u8(c.r, c.g, c.b);
            (col, Hsla::from(col))
        })
        .collect();
    let n = cands.len() as f32;

    // Vibrancy score: saturated, mid-lightness, and dominant colors rank highest.
    let score = |h: &Hsla, rank: usize| -> f32 {
        let vib = h.saturation;
        let lum = 1.0 - (h.lightness - 0.5).abs() * 2.0; // peaks at L = 0.5
        let pop = 1.0 - rank as f32 / n; // color_thief returns dominant colors first
        0.5 * vib + 0.3 * lum + 0.2 * pop
    };
    // Normalized 0..1 angular hue distance.
    let hue_dist = |a: f32, b: f32| {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d) / 180.0
    };

    // Primary: the most vibrant candidate.
    let (pi, (primary, ph)) = cands
        .iter()
        .enumerate()
        .map(|(i, c)| (i, *c))
        .max_by(|(i, (_, a)), (j, (_, b))| score(a, *i).total_cmp(&score(b, *j)))?;

    let mut chosen = vec![primary];
    let mut chosen_hues = vec![ph.hue];
    let mut used = vec![pi];

    // Greedily add the next color that best balances vibrancy with being a
    // different hue from everything chosen so far.
    while chosen.len() < MAX_DYNAMIC_COLORS && used.len() < cands.len() {
        let next = cands
            .iter()
            .enumerate()
            .filter(|(i, _)| !used.contains(i))
            .map(|(i, (col, h))| {
                let nearest = chosen_hues
                    .iter()
                    .map(|&ch| hue_dist(h.hue, ch))
                    .fold(1.0_f32, f32::min);
                (i, score(h, i) + 0.4 * nearest, *col, h.hue)
            })
            .max_by(|(_, a, _, _), (_, b, _, _)| a.total_cmp(b));
        let Some((i, _, col, hue)) = next else { break };
        chosen.push(col);
        chosen_hues.push(hue);
        used.push(i);
    }

    // Monochrome cover: only one candidate survived — derive a lightness-shifted
    // secondary so dynamic colors still has two stops to gradient between.
    if chosen.len() == 1 {
        let mut h = ph;
        h.lightness = if ph.lightness < 0.5 {
            (ph.lightness + 0.35).min(0.9)
        } else {
            (ph.lightness - 0.35).max(0.1)
        };
        chosen.push(Color::Hsla(h));
    }

    Some(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` cover with a fully transparent pixel, to check both the
    /// downscale bounds and the opacity flattening.
    fn art(w: u32, h: u32) -> image::RgbaImage {
        let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 90, 255]));
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        img
    }

    #[test]
    fn downscale_caps_longest_side_and_keeps_aspect() {
        let small = downscale_for_blur(&art(3000, 1500));
        assert_eq!(small.dimensions(), (ART_BLUR_SOURCE_MAX, 128));
    }

    #[test]
    fn downscale_leaves_small_art_alone() {
        // Already under the cap: upscaling would only cost memory and blur time.
        let small = downscale_for_blur(&art(64, 64));
        assert_eq!(small.dimensions(), (64, 64));
    }

    #[test]
    fn downscale_never_collapses_a_thin_cover_to_zero() {
        // A 4000×1 strip rounds its short side to 0 without the `.max(1)`, and a
        // zero-area texture is a GPU validation error.
        let small = downscale_for_blur(&art(4000, 1));
        assert_eq!(small.dimensions(), (ART_BLUR_SOURCE_MAX, 1));
    }

    #[test]
    fn downscale_forces_opaque() {
        // `fast_blur` assumes premultiplied alpha; transparent pixels left in
        // place bleed dark halos through the backdrop.
        for small in [
            downscale_for_blur(&art(512, 512)),
            downscale_for_blur(&art(8, 8)),
        ] {
            assert!(small.pixels().all(|p| p.0[3] == u8::MAX));
        }
    }
}
