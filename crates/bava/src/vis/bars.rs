//! 2D bars / monstercat-style visualizer.
//!
//! Spawns one sprite per bar and, every frame, resizes and recolors them from
//! the [`Cava`] resource: bars grow from the bottom of the window with a
//! cyan→magenta hue gradient that brightens with amplitude.

use bevy::prelude::*;

use crate::cava::{Cava, CavaSettings};

/// Fraction of window height a full-scale bar occupies.
const MAX_HEIGHT_FRAC: f32 = 0.9;
/// Gap between bars, in pixels.
const BAR_GAP: f32 = 2.0;

/// Marks a bar sprite and records which Cava bar index it renders.
#[derive(Component)]
struct Bar(usize);

/// 2D bars visualizer plugin.
pub struct BarsPlugin;

impl Plugin for BarsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(Update, update_bars);
    }
}

/// Spawn the 2D camera and one sprite per bar.
fn setup(mut commands: Commands, settings: Res<CavaSettings>) {
    commands.spawn(Camera2d);

    let n = settings.bars_per_channel.max(1);
    for i in 0..n {
        commands.spawn((
            Sprite::from_color(Color::WHITE, Vec2::new(4.0, 4.0)),
            Transform::from_xyz(0.0, 0.0, 0.0),
            Bar(i),
        ));
    }
}

/// Resize and recolor each bar from the latest analyzed audio.
fn update_bars(
    cava: Res<Cava>,
    windows: Query<&Window>,
    mut bars: Query<(&Bar, &mut Sprite, &mut Transform)>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());

    let values = cava.mono();
    let n = values.len();
    if n == 0 {
        return;
    }

    let slot_w = w / n as f32;
    let bar_w = (slot_w - BAR_GAP).max(1.0);
    let max_h = h * MAX_HEIGHT_FRAC;
    let floor = -h / 2.0;
    let left = -w / 2.0;

    for (bar, mut sprite, mut transform) in &mut bars {
        let v = values.get(bar.0).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        let bar_h = (v * max_h).max(1.0);

        let x = left + slot_w * (bar.0 as f32 + 0.5);
        transform.translation.x = x;
        transform.translation.y = floor + bar_h * 0.5;

        sprite.custom_size = Some(Vec2::new(bar_w, bar_h));

        // Monstercat-ish gradient: hue sweeps across the spectrum, lightness
        // tracks amplitude so active bars glow.
        let hue = (195.0 + (bar.0 as f32 / n as f32) * 150.0) % 360.0;
        let lightness = 0.35 + 0.4 * v.min(1.0);
        sprite.color = Color::hsl(hue, 0.85, lightness);
    }
}
