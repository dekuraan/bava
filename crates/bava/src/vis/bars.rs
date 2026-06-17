//! 2D bars / monstercat-style visualizer.
//!
//! Spawns one sprite per bar and, every frame, resizes them from the [`Cava`]
//! resource. The signature "monstercat" look comes from a neighbour-spreading
//! pass: each bar lifts the bars around it with exponential falloff, so a single
//! loud frequency becomes a smooth wave rather than an isolated spike. Bars are
//! rendered as near-white over the album-art backdrop.

use bevy::prelude::*;

use crate::cava::{Cava, CavaSettings};
use crate::vis::VisSettings;

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

/// Monstercat neighbour spreading: each bar raises the others to at least
/// `value / factor^distance`, taking the max. Sources are the unsmoothed heights
/// (`src`) so the spread is order-independent. `factor <= 1` is a no-op.
fn spread_monstercat(values: &mut [f32], factor: f32) {
    if factor <= 1.0 {
        return;
    }
    let n = values.len();
    let src: Vec<f32> = values.to_vec();
    for z in 0..n {
        let peak = src[z];
        if peak <= 0.0 {
            continue;
        }
        for (m, out) in values.iter_mut().enumerate() {
            if m == z {
                continue;
            }
            let dist = (z as i32 - m as i32).unsigned_abs() as f32;
            let spread = peak / factor.powf(dist);
            if spread > *out {
                *out = spread;
            }
        }
    }
}

/// Resize each bar from the latest analyzed audio (with monstercat spreading).
fn update_bars(
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut bars: Query<(&Bar, &mut Sprite, &mut Transform)>,
) {
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

    // Gradient endpoints, resolved once per frame.
    let lo = vis.color_lo.to_srgba();
    let hi = vis.color_hi.to_srgba();

    for (bar, mut sprite, mut transform) in &mut bars {
        let v = values.get(bar.0).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        let t = v.min(1.0);
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

        // Foreground gradient: lerp lo→hi by amplitude.
        sprite.color = Color::srgba(
            lo.red + (hi.red - lo.red) * t,
            lo.green + (hi.green - lo.green) * t,
            lo.blue + (hi.blue - lo.blue) * t,
            0.95,
        );
    }
}
