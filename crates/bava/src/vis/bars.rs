//! 2D bars / monstercat-style visualizer.
//!
//! One sprite per bar, resized every frame from the [`Cava`] resource. The
//! monstercat neighbour-spreading pass turns spikes into smooth waves; bars are
//! filled with an amplitude gradient and can grow from the bottom or mirror from
//! the center. Stands in for every linear ([`VisFamily::Box`]) drawing mode for
//! now; hidden while a circle mode is active.

use bevy::prelude::*;

use crate::cava::{Cava, CavaSettings};
use crate::vis::{gradient_color, spread_monstercat, DrawingMode, MirrorMode, VisFamily, VisSettings};

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
        app.add_systems(Startup, setup)
            // Reconcile the sprite pool first so a live bar-count change (editor
            // "Apply" / profile load) is reflected the same frame it converges.
            .add_systems(Update, (reconcile_bars, update_bars).chain());
    }
}

/// Spawn the 2D camera and one sprite per bar.
fn setup(mut commands: Commands, settings: Res<CavaSettings>) {
    commands.spawn(Camera2d);

    let n = settings.bars_per_channel.max(1);
    for i in 0..n {
        spawn_bar(&mut commands, i);
    }
}

/// Spawn a single bar sprite carrying its Cava bar index.
fn spawn_bar(commands: &mut Commands, i: usize) {
    commands.spawn((
        Sprite::from_color(Color::WHITE, Vec2::new(4.0, 4.0)),
        Transform::from_xyz(0.0, 0.0, 0.0),
        Bar(i),
    ));
}

/// Grow or shrink the bar-sprite pool to match the live [`Cava::bars_per_channel`],
/// which the settings editor can change at runtime (the startup pool is fixed).
/// Indices stay contiguous `0..target`, so [`update_bars`] addresses them safely.
fn reconcile_bars(mut commands: Commands, cava: Res<Cava>, bars: Query<(Entity, &Bar)>) {
    let target = cava.bars_per_channel.max(1);
    let current = bars.iter().count();
    if current < target {
        for i in current..target {
            spawn_bar(&mut commands, i);
        }
    } else if current > target {
        // Drop the highest indices, keeping a contiguous 0..target range.
        for (entity, bar) in &bars {
            if bar.0 >= target {
                commands.entity(entity).despawn();
            }
        }
    }
}

/// Resize each bar from the latest analyzed audio (with monstercat spreading).
fn update_bars(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut bars: Query<(&Bar, &mut Sprite, &mut Transform, &mut Visibility)>,
) {
    // The bars renderer stands in for every linear (box) mode for now.
    let show = mode.family() == VisFamily::Box;
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

    let mirror = vis.mirror != MirrorMode::Off;
    // Gradient endpoints, resolved once per frame (each call clones the profile).
    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    let slot_w = w / n as f32;
    let bar_w = (slot_w - BAR_GAP).max(1.0);
    // Mirrored bars span both up and down from the center, so each half uses
    // about half the height budget.
    let max_h = h * MAX_HEIGHT_FRAC * if mirror { 0.5 } else { 1.0 };
    let floor = -h / 2.0;
    let left = -w / 2.0;

    for (bar, mut sprite, mut transform, mut visibility) in &mut bars {
        *visibility = Visibility::Visible;

        let v = values.get(bar.0).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        let x = left + slot_w * (bar.0 as f32 + 0.5);

        let bar_h = (v * max_h).max(1.0);
        transform.translation.x = x;
        if mirror {
            // Centered: extends ±bar_h from the middle → total height 2·bar_h.
            transform.translation.y = 0.0;
            sprite.custom_size = Some(Vec2::new(bar_w, bar_h * 2.0));
        } else {
            transform.translation.y = floor + bar_h * 0.5;
            sprite.custom_size = Some(Vec2::new(bar_w, bar_h));
        }

        sprite.color = gradient_color(lo, hi, v.min(1.0));
    }
}
