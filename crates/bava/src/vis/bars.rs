// SPDX-License-Identifier: GPL-3.0-or-later
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
//! The first four reuse a one-mesh-per-bar pool of rounded-rect [`Mesh2d`]s
//! (kept in sync with the live bar count by [`reconcile_bars`], rounded per
//! `items_roundness` with feather-antialiased edges); the last two are a single
//! antialiased stroke mesh, hidden while a blocky shape is active. All shapes
//! share the [`Cava`] resource and the monstercat neighbour-spreading pass.

use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::view::Hdr;

use crate::cava::{Cava, CavaSettings};
use crate::vis::stroke::{
    apply_rounded_rect, apply_stroke, empty_stroke_mesh, stroke_material, STROKE_FEATHER,
};
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

/// Marks a bar mesh and records which Cava bar index it renders.
#[derive(Component)]
struct Bar(usize);

/// Shared blend material for the bar meshes (per-vertex color supplies the hue).
#[derive(Resource)]
struct BarMaterial(Handle<ColorMaterial>);

/// Handle for the line-shape (Wave/Splitter) stroke mesh, rebuilt each frame.
#[derive(Resource)]
struct BoxLineHandles {
    mesh: Handle<Mesh>,
}

/// Marks the box-line stroke entity.
#[derive(Component)]
struct BoxLineStroke;

/// 2D linear visualizer plugin.
pub struct BarsPlugin;

impl Plugin for BarsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            // Reconcile the sprite pool first so a live bar-count change (editor
            // "Apply" / profile load) is reflected the same frame it converges.
            .add_systems(Update, (reconcile_bars, update_bars, update_box_lines).chain());
    }
}

/// Spawn the 2D camera and one sprite per bar. The camera is HDR with 8× MSAA
/// and bloom, so the amplitude-boosted (HDR-range) colors from
/// [`gradient_color`](crate::vis::gradient_color) glow at peaks and the gizmo /
/// mesh edges stay smooth.
fn setup(
    mut commands: Commands,
    settings: Res<CavaSettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    commands.spawn((
        Camera2d,
        Hdr,
        Msaa::Sample8,
        // NATURAL bloom, nudged up a touch for a punchier neon glow.
        Bloom {
            intensity: 0.25,
            ..Bloom::NATURAL
        },
    ));

    // One shared blend material; each bar mesh carries its own per-vertex color.
    let bar_material = materials.add(stroke_material());
    let n = settings.bars_per_channel.max(1);
    for i in 0..n {
        spawn_bar(&mut commands, &mut meshes, &bar_material, i);
    }
    commands.insert_resource(BarMaterial(bar_material));

    // Reusable antialiased stroke for the line shapes (Wave / Splitter); only one
    // is active at a time, so a single entity suffices.
    let line_mesh = meshes.add(empty_stroke_mesh());
    commands.spawn((
        Mesh2d(line_mesh.clone()),
        MeshMaterial2d(materials.add(stroke_material())),
        Transform::from_xyz(0.0, 0.0, 1.0),
        Visibility::Hidden,
        BoxLineStroke,
    ));
    commands.insert_resource(BoxLineHandles { mesh: line_mesh });
}

/// Spawn a single bar as its own (initially empty) rounded-rect mesh, carrying
/// its Cava bar index and sharing the blend material.
fn spawn_bar(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: &Handle<ColorMaterial>,
    i: usize,
) {
    commands.spawn((
        Mesh2d(meshes.add(empty_stroke_mesh())),
        MeshMaterial2d(material.clone()),
        Transform::from_xyz(0.0, 0.0, 0.0),
        Visibility::Hidden,
        Bar(i),
    ));
}

/// Grow or shrink the bar-mesh pool to match the live [`Cava::bars_per_channel`],
/// which the settings editor can change at runtime (the startup pool is fixed).
/// Indices stay contiguous `0..target`, so [`update_bars`] addresses them safely.
fn reconcile_bars(
    mut commands: Commands,
    cava: Res<Cava>,
    material: Res<BarMaterial>,
    mut meshes: ResMut<Assets<Mesh>>,
    bars: Query<(Entity, &Bar)>,
) {
    let target = cava.bars_per_channel.max(1);
    let current = bars.iter().count();
    if current < target {
        for i in current..target {
            spawn_bar(&mut commands, &mut meshes, &material.0, i);
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

/// Rebuild each bar mesh for the blocky shapes (Bars/Levels/Particles/Spine) as
/// a rounded rect, or hide the pool when a line shape (Wave/Splitter) or a circle
/// mode is active.
fn update_bars(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut bars: Query<(&Bar, &Mesh2d, &mut Transform, &mut Visibility)>,
) {
    let shape = (mode.family() == VisFamily::Box).then(|| mode.shape());
    // Line shapes and circle modes don't use the bar-mesh pool.
    let bar_shape = match shape {
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
    let centered = mirror && matches!(bar_shape, VisShape::Bars | VisShape::Levels);
    let lyt = Layout::new(w, h, n, centered);

    for (bar, mesh2d, mut transform, mut visibility) in &mut bars {
        *visibility = Visibility::Visible;
        let v = values.get(bar.0).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        let x = lyt.bar_x(bar.0);

        // Each shape resolves to a centered rounded rect: (center y, half extents).
        let (cy, half) = match bar_shape {
            VisShape::Bars => column_geom(&lyt, (v * lyt.max_h).max(1.0), centered),
            VisShape::Levels => {
                // Snap the height to discrete VU-style steps.
                let stepped = (v * LEVEL_STEPS).round() / LEVEL_STEPS;
                column_geom(&lyt, (stepped * lyt.max_h).max(1.0), centered)
            }
            VisShape::Particles => {
                // A small square that floats at the bar's level.
                let dot = lyt.bar_w.min(lyt.slot_w).max(2.0);
                (lyt.floor + v * lyt.max_h, Vec2::splat(dot * 0.5))
            }
            VisShape::Spine => {
                // A square on the center line, side growing with the level.
                let side = (lyt.bar_w * (0.35 + v)).clamp(2.0, lyt.max_h);
                (0.0, Vec2::splat(side * 0.5))
            }
            _ => unreachable!("bar_shape is constrained above"),
        };

        transform.translation.x = x;
        transform.translation.y = cy;

        if let Some(mesh) = meshes.get_mut(&mesh2d.0) {
            let radius = vis.items_roundness.clamp(0.0, 1.0) * half.x.min(half.y);
            let color = gradient_color(lo, hi, v.min(1.0));
            apply_rounded_rect(mesh, half, radius, STROKE_FEATHER, color);
        }
    }
}

/// Center-y and half-extents of a column of height `bar_h`, growing from the
/// floor or centered (mirrored).
fn column_geom(lyt: &Layout, bar_h: f32, centered: bool) -> (f32, Vec2) {
    if centered {
        (0.0, Vec2::new(lyt.bar_w * 0.5, bar_h))
    } else {
        (lyt.floor + bar_h * 0.5, Vec2::new(lyt.bar_w * 0.5, bar_h * 0.5))
    }
}

/// Rebuild the antialiased stroke for the line-based box shapes (Wave, Splitter),
/// or hide it for every other mode (handled by the sprite pool / circle renderer).
fn update_box_lines(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    line: Res<BoxLineHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut q: Query<&mut Visibility, With<BoxLineStroke>>,
) {
    let shape = (mode.family() == VisFamily::Box).then(|| mode.shape());
    let active = matches!(shape, Some(VisShape::Wave | VisShape::Splitter));
    for mut v in &mut q {
        *v = if active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if !active {
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

    let pts: Vec<(Vec2, Color)> = match shape {
        Some(VisShape::Wave) => {
            // A smooth gradient waveform across the full width.
            (0..=WAVE_SEGMENTS)
                .map(|k| {
                    let t = k as f32 / WAVE_SEGMENTS as f32;
                    let v = sample_h(&values, t).clamp(0.0, 1.5);
                    let x = lyt.left + t * w;
                    let y = lyt.floor + v * lyt.max_h;
                    (Vec2::new(x, y), gradient_color(lo, hi, v.min(1.0)))
                })
                .collect()
        }
        Some(VisShape::Splitter) => {
            // Zig-zag: each bar alternates above/below the center line.
            (0..n)
                .map(|i| {
                    let v = values[i].clamp(0.0, 1.5);
                    let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
                    let y = dir * v * lyt.max_h * 0.5;
                    (Vec2::new(lyt.bar_x(i), y), gradient_color(lo, hi, v.min(1.0)))
                })
                .collect()
        }
        _ => Vec::new(),
    };

    if let Some(mesh) = meshes.get_mut(&line.mesh) {
        apply_stroke(mesh, &pts, vis.line_thickness * 0.5, STROKE_FEATHER, false);
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
