// SPDX-License-Identifier: GPL-3.0-or-later
//! On-screen HUD: a dimmed album-art backdrop and a centered now-playing label,
//! both driven by the [`mpris`](crate::mpris) resources.

use bevy::prelude::*;

use crate::mpris::{AlbumArt, NowPlaying};

/// Full-window album-art backdrop sprite.
#[derive(Component)]
struct ArtBackground;

/// Now-playing text label (title line).
#[derive(Component)]
struct NowPlayingTitle;

/// Now-playing text label (artist / album line).
#[derive(Component)]
struct NowPlayingSub;

/// Album-art backdrop + now-playing label.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_hud)
            .add_systems(Update, (update_background, update_label));
    }
}

fn setup_hud(mut commands: Commands) {
    // Backdrop sits behind the bars (negative Z). Hidden until art arrives.
    commands.spawn((
        Sprite {
            color: Color::NONE,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -10.0),
        ArtBackground,
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
                    font_size: 30.0,
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::new_with_justify(Justify::Center),
                TextShadow {
                    offset: Vec2::splat(2.0),
                    color: Color::srgba(0.0, 0.0, 0.0, 0.85),
                },
                NowPlayingTitle,
            ));
            parent.spawn((
                Text::new(""),
                TextFont {
                    font_size: 18.0,
                    ..default()
                },
                TextColor(Color::srgba(0.85, 0.85, 0.9, 1.0)),
                TextLayout::new_with_justify(Justify::Center),
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
