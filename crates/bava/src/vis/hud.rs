// SPDX-License-Identifier: MIT OR Apache-2.0
//! On-screen HUD: a blurred, dimmed album-art backdrop, user-configured
//! background / foreground image overlays, and a centered now-playing label.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::now_playing::{AlbumArt, NowPlaying};
use crate::vis::{ImageLayer, VisSettings};

/// Blur radius, in pixels of the downscaled art, at `art_blur == 1.0`. The
/// source's longest side is `now_playing::ART_BLUR_SOURCE_MAX` (256), so the
/// default 0.4 is a ~10px sigma — around 4% of the image, the frosted-glass
/// look media players use.
const ART_BLUR_MAX_SIGMA: f32 = 24.0;

/// Extra scale on the blurred backdrop. The blur clamps at the source's edges,
/// which leaves a faint seam; pushing it a few percent past the window hides
/// that without noticeably changing the framing.
const ART_BLUR_OVERSCALE: f32 = 1.06;

/// Full-window album-art backdrop sprite (driven by now-playing metadata).
#[derive(Component)]
struct ArtBackground;

/// User-configured background image sprite (from `[vis.background]`).
#[derive(Component)]
struct UserBackground;

/// User-configured foreground image sprite (from `[vis.foreground]`).
#[derive(Component)]
struct UserForeground;

/// Now-playing text label (title line).
#[derive(Component)]
struct NowPlayingTitle;

/// Now-playing text label (artist / album line).
#[derive(Component)]
struct NowPlayingSub;

/// The blurred backdrop texture and the blur amount it was built at, so moving
/// the blur slider re-blurs the small retained copy of the art instead of
/// re-decoding the cover.
#[derive(Resource, Default)]
struct BlurredArt {
    /// `None` when there is no art, or when blur is off (the sharp art is used).
    image: Option<Handle<Image>>,
    /// The `art_blur` this texture was built at; `None` before the first build.
    blur: Option<f32>,
}

/// Album-art backdrop, user image overlays, and now-playing label.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BlurredArt>()
            .add_systems(Startup, setup_hud)
            .add_systems(
                Update,
                (
                    // Explicitly ordered so a new blur lands on the backdrop the
                    // same frame the slider moves, rather than one frame later.
                    update_art_blur.before(update_background),
                    update_background,
                    update_user_images,
                    update_label,
                ),
            );
    }
}

fn setup_hud(mut commands: Commands, mut fonts: ResMut<Assets<Font>>) {
    let font_regular = fonts.add(Font::from_bytes(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/FiraSans-Regular.ttf"
        ))
        .to_vec(),
    ));
    let font_medium = fonts.add(Font::from_bytes(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/FiraSans-Medium.ttf"
        ))
        .to_vec(),
    ));
    // User background image — behind album art and bars.
    commands.spawn((
        Sprite {
            color: Color::NONE,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -12.0),
        UserBackground,
    ));

    // Album-art backdrop sits behind the bars. Hidden until art arrives.
    commands.spawn((
        Sprite {
            color: Color::NONE,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -10.0),
        ArtBackground,
    ));

    // User foreground image — above bars, below HUD text.
    commands.spawn((
        Sprite {
            color: Color::NONE,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 2.0),
        UserForeground,
    ));

    // Centered now-playing block, pinned to the top of the screen. A full-width
    // column with centered content keeps title + subtitle stacked and centered.
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            top: Val::Px(28.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(4.0),
            ..default()
        })
        .with_children(|parent| {
            parent.spawn((
                Text::new(""),
                TextFont {
                    font: font_medium.into(),
                    font_size: 30.0.into(),
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::justify(Justify::Center),
                TextShadow {
                    offset: Vec2::splat(2.0),
                    color: Color::srgba(0.0, 0.0, 0.0, 0.85),
                },
                NowPlayingTitle,
            ));
            parent.spawn((
                Text::new(""),
                TextFont {
                    font: font_regular.into(),
                    font_size: 18.0.into(),
                    ..default()
                },
                TextColor(Color::srgba(0.85, 0.85, 0.9, 1.0)),
                TextLayout::justify(Justify::Center),
                TextShadow {
                    offset: Vec2::splat(1.5),
                    color: Color::srgba(0.0, 0.0, 0.0, 0.8),
                },
                NowPlayingSub,
            ));
        });
}

/// Tint applied to the backdrop sprite for a given brightness. The blue channel
/// keeps the slight cool lift the original fixed `(0.4, 0.4, 0.42)` dim had, so
/// the backdrop reads as a shadow behind the bars rather than a washed cover.
fn backdrop_tint(brightness: f32) -> Color {
    let b = brightness.max(0.0);
    Color::srgb(b, b, b * 1.05)
}

/// Blur the retained downscaled art whenever the art or the blur amount changes,
/// and publish it as [`BlurredArt`].
///
/// Blurring 256px with a 3-pass box approximation is well under a millisecond,
/// which is what makes this affordable to redo live while dragging the slider —
/// the alternative (a two-pass GPU gaussian) would need a `Material2d` and a
/// render target for something whose source only changes once per track.
fn update_art_blur(
    art: Res<AlbumArt>,
    vis: Res<VisSettings>,
    mut blurred: ResMut<BlurredArt>,
    mut images: ResMut<Assets<Image>>,
) {
    let blur = vis.art_blur.clamp(0.0, 1.0);
    // Deliberately *not* guarded on `vis.is_changed()`: `animate_album_colors`
    // writes `dynamic_fg` on every frame of a crossfade, which would re-blur ~24
    // times per song change for no visible difference.
    if !art.is_changed() && blurred.blur == Some(blur) {
        return;
    }
    // Dropping the old strong handle frees the previous texture.
    blurred.image = None;
    blurred.blur = Some(blur);

    let Some(small) = &art.small else {
        return;
    };
    if blur <= 0.0 {
        // Blur off: `update_background` falls back to the full-resolution art.
        return;
    }

    let out = image::imageops::fast_blur(small, blur * ART_BLUR_MAX_SIGMA);
    let (w, h) = out.dimensions();
    let mut image = Image::new(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        out.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // The whole approach leans on the GPU bilinearly stretching this small
    // texture over the window, so don't inherit a nearest-neighbour default.
    image.sampler = ImageSampler::linear();
    blurred.image = Some(images.add(image));
}

/// Cover-fit the album art to the window (preserving aspect), blurred and dimmed
/// so the bars stay readable. Hidden when no art is available.
fn update_background(
    art: Res<AlbumArt>,
    vis: Res<VisSettings>,
    blurred: Res<BlurredArt>,
    windows: Query<&Window>,
    mut q: Query<&mut Sprite, With<ArtBackground>>,
    mut last: Local<(Vec2, f32)>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (ww, wh) = (window.width(), window.height());
    // The sprite only depends on the art, the blur texture, the brightness and
    // the window size; rewriting it every frame would re-dirty the sprite (and
    // re-clone the handle) at 60 Hz.
    let size = Vec2::new(ww, wh);
    let brightness = vis.art_brightness;
    if !art.is_changed() && !blurred.is_changed() && *last == (size, brightness) {
        return;
    }
    *last = (size, brightness);

    // Prefer the blurred texture; it shares the art's aspect ratio, so the same
    // cover-fit math applies — only the overscale that hides its edges differs.
    let source = match (&blurred.image, &art.image) {
        (Some(handle), _) => Some((handle, ART_BLUR_OVERSCALE)),
        (None, Some(handle)) => Some((handle, 1.0)),
        (None, None) => None,
    };

    for mut sprite in &mut q {
        match (source, art.size) {
            (Some((handle, overscale)), Some((aw, ah))) if aw > 0 && ah > 0 => {
                sprite.image = handle.clone();
                // Scale so the image fully covers the window without distortion;
                // overflow simply extends past the screen edges.
                let (aw, ah) = (aw as f32, ah as f32);
                let scale = (ww / aw).max(wh / ah) * overscale;
                sprite.custom_size = Some(Vec2::new(aw * scale, ah * scale));
                sprite.color = backdrop_tint(brightness);
            }
            _ => sprite.color = Color::NONE,
        }
    }
}

/// Reflect the current track in the two-line label.
fn update_label(
    now_playing: Res<NowPlaying>,
    mut titles: Query<&mut Text, (With<NowPlayingTitle>, Without<NowPlayingSub>)>,
    mut subs: Query<&mut Text, (With<NowPlayingSub>, Without<NowPlayingTitle>)>,
) {
    if !now_playing.is_changed() {
        return;
    }

    let title = now_playing.title.clone().unwrap_or_default();
    let sub = match (&now_playing.artist, &now_playing.album) {
        (Some(a), Some(al)) => format!("{a} — {al}"),
        (Some(a), None) => a.clone(),
        (None, Some(al)) => al.clone(),
        (None, None) => String::new(),
    };

    for mut t in &mut titles {
        t.0 = title.clone();
    }
    for mut t in &mut subs {
        t.0 = sub.clone();
    }
}

/// Cover-fit, tint, and show one user image `layer` on its sprite (or hide the
/// sprite when the layer has no path). `asset_server.load` returns the cached
/// handle for an already-loaded path, so re-issuing it each frame is cheap.
/// Returns `true` once the layer is settled (no image configured, or the image
/// is loaded and the final cover-fit size has been applied).
fn apply_image_layer(
    layer: &ImageLayer,
    ww: f32,
    wh: f32,
    asset_server: &AssetServer,
    images: &Assets<Image>,
    sprite: &mut Sprite,
    visibility: &mut Visibility,
) -> bool {
    let Some(path) = &layer.path else {
        sprite.color = Color::NONE;
        *visibility = Visibility::Hidden;
        return true;
    };
    let handle: Handle<Image> = asset_server.load(path.clone());
    // Cover-fit to window when the image's dimensions are known.
    let loaded = if let Some(img) = images.get(&handle) {
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        if iw > 0.0 && ih > 0.0 {
            let scale = (ww / iw).max(wh / ih) * layer.scale;
            sprite.custom_size = Some(Vec2::new(iw * scale, ih * scale));
        }
        true
    } else {
        // Image not yet loaded; fill window at the configured scale.
        sprite.custom_size = Some(Vec2::new(ww, wh) * layer.scale);
        false
    };
    sprite.image = handle;
    sprite.color = Color::srgba(1.0, 1.0, 1.0, layer.alpha);
    *visibility = Visibility::Visible;
    loaded
}

/// Load and display user-configured background / foreground images from
/// [`VisSettings::background`] and [`VisSettings::foreground`].
// A Bevy system signature: one param per queried layer + the change-guard
// Locals; the disjoint With/Without filters are what keep the queries sound.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_user_images(
    vis: Res<VisSettings>,
    asset_server: Res<AssetServer>,
    images: Res<Assets<Image>>,
    windows: Query<&Window>,
    mut bg: Query<(&mut Sprite, &mut Visibility), (With<UserBackground>, Without<UserForeground>)>,
    mut fg: Query<(&mut Sprite, &mut Visibility), (With<UserForeground>, Without<UserBackground>)>,
    mut last_size: Local<Vec2>,
    mut settled: Local<bool>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (ww, wh) = (window.width(), window.height());
    // Only re-apply when the settings or window changed, or while a configured
    // image is still loading (its cover-fit size isn't final until then) —
    // otherwise this would hit the asset server and dirty both sprites at 60 Hz.
    let size = Vec2::new(ww, wh);
    if vis.is_changed() || *last_size != size {
        *settled = false;
        *last_size = size;
    }
    if *settled {
        return;
    }

    let mut all_loaded = true;
    for (mut sprite, mut visibility) in &mut bg {
        all_loaded &= apply_image_layer(
            &vis.background,
            ww,
            wh,
            &asset_server,
            &images,
            &mut sprite,
            &mut visibility,
        );
    }
    for (mut sprite, mut visibility) in &mut fg {
        all_loaded &= apply_image_layer(
            &vis.foreground,
            ww,
            wh,
            &asset_server,
            &images,
            &mut sprite,
            &mut visibility,
        );
    }
    *settled = all_loaded;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal app carrying just what `update_art_blur` touches.
    fn blur_app() -> App {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default())
            .init_asset::<Image>()
            .init_resource::<AlbumArt>()
            .init_resource::<BlurredArt>()
            .init_resource::<VisSettings>()
            .add_systems(Update, update_art_blur);
        app
    }

    fn set_art(app: &mut App, w: u32, h: u32) {
        let mut art = app.world_mut().resource_mut::<AlbumArt>();
        art.small = Some(image::RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([10, 200, 60, 255]),
        ));
        art.size = Some((w, h));
    }

    fn blur_handle(app: &App) -> Option<AssetId<Image>> {
        app.world()
            .resource::<BlurredArt>()
            .image
            .as_ref()
            .map(|h| h.id())
    }

    fn set_blur(app: &mut App, blur: f32) {
        app.world_mut().resource_mut::<VisSettings>().art_blur = blur;
    }

    #[test]
    fn l2_blur_texture_is_built_from_the_retained_art() {
        let mut app = blur_app();
        set_art(&mut app, 64, 48);
        app.update();

        let id = blur_handle(&app).expect("blur enabled by default, so a texture is built");
        let images = app.world().resource::<Assets<Image>>();
        let image = images.get(id).unwrap();
        // Blurred at the *downscaled* size, not the window size — the GPU does
        // the upscale.
        assert_eq!((image.width(), image.height()), (64, 48));
    }

    #[test]
    fn l2_blur_is_not_rebuilt_when_nothing_changed() {
        // The guard that matters: `animate_album_colors` dirties `VisSettings`
        // every frame of a crossfade, and re-blurring on that would burn ~24
        // pointless blurs per song change.
        let mut app = blur_app();
        set_art(&mut app, 64, 64);
        app.update();
        let first = blur_handle(&app).unwrap();

        app.world_mut().resource_mut::<VisSettings>().glow_gain = 3.0;
        app.update();
        assert_eq!(blur_handle(&app), Some(first));
    }

    #[test]
    fn l2_moving_the_blur_slider_rebuilds() {
        let mut app = blur_app();
        set_art(&mut app, 64, 64);
        app.update();
        let first = blur_handle(&app).unwrap();

        set_blur(&mut app, 0.9);
        app.update();
        assert_ne!(blur_handle(&app), Some(first));
    }

    #[test]
    fn l2_zero_blur_drops_the_texture() {
        // With no blurred texture, `update_background` falls back to the sharp
        // full-resolution art — that's how blur is turned off.
        let mut app = blur_app();
        set_art(&mut app, 64, 64);
        app.update();
        assert!(blur_handle(&app).is_some());

        set_blur(&mut app, 0.0);
        app.update();
        assert_eq!(blur_handle(&app), None);
    }

    #[test]
    fn l2_no_art_yields_no_blur_texture() {
        let mut app = blur_app();
        app.update();
        assert_eq!(blur_handle(&app), None);
    }

    #[test]
    fn backdrop_tint_keeps_the_cool_lift_and_clamps_negatives() {
        let Color::Srgba(c) = backdrop_tint(0.62) else {
            panic!("expected sRGB")
        };
        assert!((c.red - 0.62).abs() < 1e-6);
        assert!(c.blue > c.red, "backdrop keeps a slight blue lift");
        let Color::Srgba(c) = backdrop_tint(-1.0) else {
            panic!("expected sRGB")
        };
        assert_eq!(c.red, 0.0);
    }
}
