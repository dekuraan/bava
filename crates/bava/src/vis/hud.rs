// SPDX-License-Identifier: GPL-3.0-or-later
//! On-screen HUD: a dimmed album-art backdrop, user-configured background /
//! foreground image overlays, and a centered now-playing label.

use bevy::prelude::*;

use crate::now_playing::{AlbumArt, NowPlaying};
use crate::vis::{ImageLayer, VisSettings};

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

/// Album-art backdrop, user image overlays, and now-playing label.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_hud)
            .add_systems(Update, (update_background, update_user_images, update_label));
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
        Sprite { color: Color::NONE, ..default() },
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
        Sprite { color: Color::NONE, ..default() },
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

/// Cover-fit the album art to the window (preserving aspect), dimmed so the bars
/// stay readable. Hidden when no art is available.
fn update_background(
    art: Res<AlbumArt>,
    windows: Query<&Window>,
    mut q: Query<&mut Sprite, With<ArtBackground>>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (ww, wh) = (window.width(), window.height());

    for mut sprite in &mut q {
        match (&art.image, art.size) {
            (Some(handle), Some((aw, ah))) if aw > 0 && ah > 0 => {
                sprite.image = handle.clone();
                // Scale so the image fully covers the window without distortion;
                // overflow simply extends past the screen edges.
                let (aw, ah) = (aw as f32, ah as f32);
                let scale = (ww / aw).max(wh / ah);
                sprite.custom_size = Some(Vec2::new(aw * scale, ah * scale));
                sprite.color = Color::srgb(0.4, 0.4, 0.42);
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
fn apply_image_layer(
    layer: &ImageLayer,
    ww: f32,
    wh: f32,
    asset_server: &AssetServer,
    images: &Assets<Image>,
    sprite: &mut Sprite,
    visibility: &mut Visibility,
) {
    let Some(path) = &layer.path else {
        sprite.color = Color::NONE;
        *visibility = Visibility::Hidden;
        return;
    };
    let handle: Handle<Image> = asset_server.load(path.clone());
    // Cover-fit to window when the image's dimensions are known.
    if let Some(img) = images.get(&handle) {
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        if iw > 0.0 && ih > 0.0 {
            let scale = (ww / iw).max(wh / ih) * layer.scale;
            sprite.custom_size = Some(Vec2::new(iw * scale, ih * scale));
        }
    } else {
        // Image not yet loaded; fill window at the configured scale.
        sprite.custom_size = Some(Vec2::new(ww, wh) * layer.scale);
    }
    sprite.image = handle;
    sprite.color = Color::srgba(1.0, 1.0, 1.0, layer.alpha);
    *visibility = Visibility::Visible;
}

/// Load and display user-configured background / foreground images from
/// [`VisSettings::background`] and [`VisSettings::foreground`].
fn update_user_images(
    vis: Res<VisSettings>,
    asset_server: Res<AssetServer>,
    images: Res<Assets<Image>>,
    windows: Query<&Window>,
    mut bg: Query<(&mut Sprite, &mut Visibility), (With<UserBackground>, Without<UserForeground>)>,
    mut fg: Query<(&mut Sprite, &mut Visibility), (With<UserForeground>, Without<UserBackground>)>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (ww, wh) = (window.width(), window.height());

    for (mut sprite, mut visibility) in &mut bg {
        apply_image_layer(&vis.background, ww, wh, &asset_server, &images, &mut sprite, &mut visibility);
    }
    for (mut sprite, mut visibility) in &mut fg {
        apply_image_layer(&vis.foreground, ww, wh, &asset_server, &images, &mut sprite, &mut visibility);
    }
}
