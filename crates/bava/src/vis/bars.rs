//! 2D bars / monstercat-style visualizer.
//!
//! One sprite per bar, resized every frame from the [`Cava`] resource. The
//! monstercat neighbour-spreading pass turns spikes into smooth waves; bars are
//! filled with an amplitude gradient and can grow from the bottom or mirror from
//! the center. Hidden while the circle style is active.

use bevy::prelude::*;

use crate::cava::{Cava, CavaSettings};
use crate::vis::{gradient_color, spread_monstercat, VisSettings, VisStyle};

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

/// Resize each bar from the latest analyzed audio (with monstercat spreading).
fn update_bars(
    style: Res<VisStyle>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut bars: Query<(&Bar, &mut Sprite, &mut Transform, &mut Visibility)>,
) {
    let show = *style == VisStyle::Bars;
    if !show {
        for (_, _, _, mut vis_) in &mut bars {
            *vis_ = Visibility::Hidden;
        }
        return;
    }

    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());

    let mut values = cava.mono();
    let n = values.len();
    if n == 0 {
        return;
    }
    spread_monstercat(&mut values, vis.monstercat);

    let slot_w = w / n as f32;
    let bar_w = (slot_w - BAR_GAP).max(1.0);
    // Mirrored bars span both up and down from the center, so each half uses
    // about half the height budget.
    let max_h = h * MAX_HEIGHT_FRAC * if vis.mirror { 0.5 } else { 1.0 };
    let floor = -h / 2.0;
    let left = -w / 2.0;

    for (bar, mut sprite, mut transform, mut visibility) in &mut bars {
        *visibility = Visibility::Visible;

        let v = values.get(bar.0).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        let x = left + slot_w * (bar.0 as f32 + 0.5);

        let bar_h = (v * max_h).max(1.0);
        transform.translation.x = x;
        if vis.mirror {
            // Centered: extends ±bar_h from the middle → total height 2·bar_h.
            transform.translation.y = 0.0;
            sprite.custom_size = Some(Vec2::new(bar_w, bar_h * 2.0));
        } else {
            transform.translation.y = floor + bar_h * 0.5;
            sprite.custom_size = Some(Vec2::new(bar_w, bar_h));
        }

        sprite.color = gradient_color(vis.color_lo, vis.color_hi, v.min(1.0));
    }
}
