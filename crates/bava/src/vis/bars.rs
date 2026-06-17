//! Linear ([`VisFamily::Box`]) visualizers.
//!
//! Renders every box-family [`DrawingMode`] as a *distinct* shape, switching on
//! [`DrawingMode::shape`]:
//!
//! - **Bars** — classic filled spectrum columns (mirror-aware).
//! - **Levels** — VU-meter columns whose height snaps to discrete steps.
//! - **Particles** — one small square per bar floating at its level.
//! - **Spine** — squares along the center line, growing with level.
//! - **Wave** — a smooth gradient waveform line across the width.
//! - **Splitter** — a zig-zag line alternating above/below the axis.
//!
//! The first four reuse a one-sprite-per-bar pool (kept in sync with the live
//! bar count by [`reconcile_bars`]); the last two are immediate-mode gizmo
//! linestrips, so the sprite pool is hidden while they're active. All shapes
//! share the [`Cava`] resource and the monstercat neighbour-spreading pass.

use bevy::prelude::*;

use crate::cava::{Cava, CavaSettings};
use crate::vis::{
    gradient_color, spread_monstercat, DrawingMode, MirrorMode, VisFamily, VisSettings, VisShape,
};

/// Fraction of window height a full-scale bar occupies.
const MAX_HEIGHT_FRAC: f32 = 0.9;
/// Gap between bars, in pixels.
const BAR_GAP: f32 = 2.0;
/// Discrete steps a Levels column snaps to.
const LEVEL_STEPS: f32 = 14.0;
/// Smooth segments used to draw a Wave line.
const WAVE_SEGMENTS: usize = 192;

/// Marks a bar sprite and records which Cava bar index it renders.
#[derive(Component)]
struct Bar(usize);

/// 2D linear visualizer plugin.
pub struct BarsPlugin;

impl Plugin for BarsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            // Reconcile the sprite pool first so a live bar-count change (editor
            // "Apply" / profile load) is reflected the same frame it converges.
            .add_systems(Update, (reconcile_bars, update_bars, draw_box_lines).chain());
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

/// Layout constants shared by the per-shape sprite/line code, derived once per
/// frame from the window.
struct Layout {
    /// Window half-width offset (left edge x).
    left: f32,
    /// Window floor (bottom edge y).
    floor: f32,
    /// Per-bar horizontal slot width.
    slot_w: f32,
    /// Drawn bar width (slot minus the gap).
    bar_w: f32,
    /// Pixels a full-scale (value 1.0) bar spans.
    max_h: f32,
}

impl Layout {
    fn new(w: f32, h: f32, n: usize, half_height: bool) -> Self {
        let slot_w = w / n.max(1) as f32;
        Self {
            left: -w / 2.0,
            floor: -h / 2.0,
            slot_w,
            bar_w: (slot_w - BAR_GAP).max(1.0),
            max_h: h * MAX_HEIGHT_FRAC * if half_height { 0.5 } else { 1.0 },
        }
    }

    /// Center x of bar `i`.
    fn bar_x(&self, i: usize) -> f32 {
        self.left + self.slot_w * (i as f32 + 0.5)
    }
}

/// Position the bar sprites for the sprite-based shapes (Bars/Levels/Particles/
/// Spine), or hide them when a line shape (Wave/Splitter) or a circle mode is
/// active.
fn update_bars(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut bars: Query<(&Bar, &mut Sprite, &mut Transform, &mut Visibility)>,
) {
    let shape = (mode.family() == VisFamily::Box).then(|| mode.shape());
    // Line shapes and circle modes don't use the sprite pool.
    let sprite_shape = match shape {
        Some(s @ (VisShape::Bars | VisShape::Levels | VisShape::Particles | VisShape::Spine)) => s,
        _ => {
            for (_, _, _, mut v) in &mut bars {
                *v = Visibility::Hidden;
            }
            return;
        }
    };

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
    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    // Only the column shapes use the mirrored (centered, half-height) layout.
    let centered = mirror && matches!(sprite_shape, VisShape::Bars | VisShape::Levels);
    let lyt = Layout::new(w, h, n, centered);

    for (bar, mut sprite, mut transform, mut visibility) in &mut bars {
        *visibility = Visibility::Visible;
        let v = values.get(bar.0).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        let x = lyt.bar_x(bar.0);
        transform.translation.x = x;

        match sprite_shape {
            VisShape::Bars => {
                let bar_h = (v * lyt.max_h).max(1.0);
                place_column(&mut sprite, &mut transform, &lyt, bar_h, centered);
            }
            VisShape::Levels => {
                // Snap the height to discrete VU-style steps.
                let stepped = (v * LEVEL_STEPS).round() / LEVEL_STEPS;
                let bar_h = (stepped * lyt.max_h).max(1.0);
                place_column(&mut sprite, &mut transform, &lyt, bar_h, centered);
            }
            VisShape::Particles => {
                // A small square that floats at the bar's level.
                let dot = lyt.bar_w.min(lyt.slot_w).max(2.0);
                transform.translation.y = lyt.floor + v * lyt.max_h;
                sprite.custom_size = Some(Vec2::splat(dot));
            }
            VisShape::Spine => {
                // A square on the center line, side growing with the level.
                let side = (lyt.bar_w * (0.35 + v)).clamp(2.0, lyt.max_h);
                transform.translation.y = 0.0;
                sprite.custom_size = Some(Vec2::splat(side));
            }
            _ => unreachable!("sprite_shape is constrained above"),
        }

        sprite.color = gradient_color(lo, hi, v.min(1.0));
    }
}

/// Place a column sprite either growing from the floor or centered (mirrored).
fn place_column(sprite: &mut Sprite, transform: &mut Transform, lyt: &Layout, bar_h: f32, centered: bool) {
    if centered {
        transform.translation.y = 0.0;
        sprite.custom_size = Some(Vec2::new(lyt.bar_w, bar_h * 2.0));
    } else {
        transform.translation.y = lyt.floor + bar_h * 0.5;
        sprite.custom_size = Some(Vec2::new(lyt.bar_w, bar_h));
    }
}

/// Draw the line-based box shapes (Wave, Splitter) with gizmos. No-op for every
/// other mode (the sprite pool or the circle renderer handles those).
fn draw_box_lines(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut gizmos: Gizmos,
) {
    if mode.family() != VisFamily::Box {
        return;
    }
    let shape = mode.shape();
    if !matches!(shape, VisShape::Wave | VisShape::Splitter) {
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
    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    let lyt = Layout::new(w, h, n, false);

    match shape {
        VisShape::Wave => {
            // A smooth gradient waveform across the full width.
            let points = (0..=WAVE_SEGMENTS).map(|k| {
                let t = k as f32 / WAVE_SEGMENTS as f32;
                let v = sample_h(&values, t).clamp(0.0, 1.5);
                let x = lyt.left + t * w;
                let y = lyt.floor + v * lyt.max_h;
                (Vec2::new(x, y), gradient_color(lo, hi, v.min(1.0)))
            });
            gizmos.linestrip_gradient_2d(points);
        }
        VisShape::Splitter => {
            // Zig-zag: each bar alternates above/below the center line.
            let points = (0..n).map(|i| {
                let v = values[i].clamp(0.0, 1.5);
                let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
                let y = dir * v * lyt.max_h * 0.5;
                (Vec2::new(lyt.bar_x(i), y), gradient_color(lo, hi, v.min(1.0)))
            });
            gizmos.linestrip_gradient_2d(points);
        }
        _ => {}
    }
}

/// Smoothstep-interpolated value at `t` (0..1) across the spectrum, for the
/// continuous Wave line.
fn sample_h(values: &[f32], t: f32) -> f32 {
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return values[0];
    }
    let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
    let i0 = pos.floor() as usize;
    let i1 = (i0 + 1).min(n - 1);
    let frac = pos - i0 as f32;
    let s = frac * frac * (3.0 - 2.0 * frac); // smoothstep
    values[i0] + (values[i1] - values[i0]) * s
}
