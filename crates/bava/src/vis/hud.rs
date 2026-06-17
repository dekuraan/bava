//! On-screen HUD: a dimmed album-art backdrop and a now-playing text label,
//! both driven by the [`mpris`](crate::mpris) resources.

use bevy::prelude::*;

use crate::mpris::{AlbumArt, NowPlaying};

/// Full-window album-art backdrop sprite.
#[derive(Component)]
struct ArtBackground;

/// Now-playing text label.
#[derive(Component)]
struct NowPlayingLabel;

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

    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: 20.0,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(12.0),
            left: Val::Px(12.0),
            ..default()
        },
        NowPlayingLabel,
    ));
}

/// Stretch the album art to fill the window, dimmed so bars stay readable.
fn update_background(
    art: Res<AlbumArt>,
    windows: Query<&Window>,
    mut q: Query<&mut Sprite, With<ArtBackground>>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    for mut sprite in &mut q {
        match &art.image {
            Some(handle) => {
                sprite.image = handle.clone();
                sprite.custom_size = Some(Vec2::new(window.width(), window.height()));
                // Tint darkens the texture so the visualizer reads clearly.
                sprite.color = Color::srgb(0.28, 0.28, 0.28);
            }
            None => {
                sprite.color = Color::NONE;
            }
        }
    }
}

/// Reflect the current track in the label.
fn update_label(
    now_playing: Res<NowPlaying>,
    mut q: Query<&mut Text, With<NowPlayingLabel>>,
) {
    if !now_playing.is_changed() {
        return;
    }
    let mut text = String::new();
    if let Some(title) = &now_playing.title {
        text.push_str(title);
    }
    if let Some(artist) = &now_playing.artist {
        text.push('\n');
        text.push_str(artist);
    }
    if let Some(album) = &now_playing.album {
        text.push_str("\n— ");
        text.push_str(album);
    }
    for mut label in &mut q {
        label.0 = text.clone();
    }
}
