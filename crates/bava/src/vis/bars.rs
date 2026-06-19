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

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::view::Hdr;

use crate::cava::{Cava, CavaSettings};
use crate::vis::stroke::{
    apply_rounded_rect, apply_stroke, empty_stroke_mesh, stroke_material, STROKE_FEATHER,
};
use crate::vis::{
    gradient_color, spread_monstercat, Direction, DrawingMode, MirrorMode, VisFamily, VisSettings,
    VisShape,
};

/// Fraction of window height a full-scale bar occupies.
pub(crate) const MAX_HEIGHT_FRAC: f32 = 0.9;
/// Gap between bars, in pixels.
pub(crate) const BAR_GAP: f32 = 2.0;
/// Discrete steps a Levels column snaps to.
pub(crate) const LEVEL_STEPS: f32 = 14.0;
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
            .add_systems(Update, (reconcile_bars, update_bars, update_box_lines).chain())
            // Keep camera post-process in sync with the live editor settings.
            .add_systems(Update, (apply_tonemapping, apply_bloom));
    }
}

/// Spawn the 2D camera and one sprite per bar. The camera is HDR with 8× MSAA
/// and bloom, so the amplitude-boosted (HDR-range) colors from
/// [`gradient_color`](crate::vis::gradient_color) glow at peaks and the gizmo /
/// mesh edges stay smooth.
fn setup(
    mut commands: Commands,
    settings: Res<CavaSettings>,
    vis: Res<VisSettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    commands.spawn((
        Camera2d,
        Hdr,
        Msaa::Sample8,
        // Map the HDR (amplitude-boosted) colors to the display per [`VisSettings::tonemapping`].
        Tonemapping::from(vis.tonemapping),
        Bloom {
            intensity: vis.bloom_intensity,
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

/// Push the live [`VisSettings::tonemapping`] choice onto the camera whenever it
/// changes (editor edit / profile load), so the tone mapper updates the same
/// frame without a restart.
fn apply_tonemapping(
    vis: Res<VisSettings>,
    mut cameras: Query<&mut Tonemapping, With<Camera2d>>,
) {
    if !vis.is_changed() {
        return;
    }
    let wanted = Tonemapping::from(vis.tonemapping);
    for mut tm in &mut cameras {
        if *tm != wanted {
            *tm = wanted;
        }
    }
}

/// Sync live [`VisSettings::bloom_intensity`] onto the camera's bloom component.
fn apply_bloom(vis: Res<VisSettings>, mut blooms: Query<&mut Bloom, With<Camera2d>>) {
    if !vis.is_changed() {
        return;
    }
    for mut bloom in &mut blooms {
        bloom.intensity = vis.bloom_intensity;
    }
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
/// frame from the window. Also consumed by `physics.rs` so the spectrum
/// colliders land exactly on the rendered bars.
pub(crate) struct Layout {
    /// Window half-width offset (left edge x).
    pub(crate) left: f32,
    /// Window floor (bottom edge y).
    pub(crate) floor: f32,
    /// Per-bar horizontal slot width.
    pub(crate) slot_w: f32,
    /// Drawn bar width (slot minus the gap).
    pub(crate) bar_w: f32,
    /// Pixels a full-scale (value 1.0) bar spans.
    pub(crate) max_h: f32,
}

impl Layout {
    pub(crate) fn new(w: f32, h: f32, n: usize, half_height: bool) -> Self {
        let slot_w = w / n.max(1) as f32;
        Self {
            left: -w / 2.0,
            floor: -h / 2.0,
            slot_w,
            bar_w: (slot_w - BAR_GAP).max(1.0),
            max_h: h * MAX_HEIGHT_FRAC * if half_height { 0.5 } else { 1.0 },
        }
    }

    /// Like [`new`](Self::new) but shrinks the drawing area by `margin` on each
    /// side and shifts its center by `offset * (w/2, h/2)`.
    #[allow(dead_code)]
    pub(crate) fn new_with_margin(
        w: f32,
        h: f32,
        n: usize,
        half_height: bool,
        margin: f32,
        offset: Vec2,
    ) -> Self {
        let eff_w = (w - 2.0 * margin).max(1.0);
        let eff_h = (h - 2.0 * margin).max(1.0);
        let slot_w = eff_w / n.max(1) as f32;
        let ox = offset.x * w * 0.5;
        let oy = offset.y * h * 0.5;
        Self {
            left: -eff_w / 2.0 + ox,
            floor: -eff_h / 2.0 + oy,
            slot_w,
            bar_w: (slot_w - BAR_GAP).max(1.0),
            max_h: eff_h * MAX_HEIGHT_FRAC * if half_height { 0.5 } else { 1.0 },
        }
    }

    /// Center x of bar `i`.
    pub(crate) fn bar_x(&self, i: usize) -> f32 {
        self.left + self.slot_w * (i as f32 + 0.5)
    }
}

/// The per-bar display values for the bar pool / line shapes, with the mirror
/// mode applied. `n` is the pool size (= live bar count). This is a purely visual
/// horizontal remap — physics reads [`Cava::mono`] directly and is unaffected.
///
/// - [`MirrorMode::Off`]: the spectrum as-is.
/// - [`MirrorMode::Full`]: a left/right-reflected copy of the spectrum (bass
///   meets at the center, treble at the edges; `reverse_mirror` flips that).
/// - [`MirrorMode::SplitChannels`]: the left channel on one side, the right
///   channel mirrored on the other (mono falls back to the same data on both).
fn mirror_values(cava: &Cava, vis: &VisSettings, n: usize) -> Vec<f32> {
    let spread = |mut v: Vec<f32>| {
        spread_monstercat(&mut v, vis.monstercat);
        v
    };
    let mut result = match vis.mirror {
        MirrorMode::Off => spread(cava.mono()),
        MirrorMode::Full => fold_symmetric(&spread(cava.mono()), n, vis.reverse_mirror),
        MirrorMode::SplitChannels => {
            let left = spread(cava.left().to_vec());
            let right = if cava.right().is_empty() {
                left.clone()
            } else {
                spread(cava.right().to_vec())
            };
            let (a, b) = if vis.reverse_mirror { (&right, &left) } else { (&left, &right) };
            let half = n.div_ceil(2);
            (0..n)
                .map(|i| {
                    if i < half {
                        resample(a, half - 1 - i, half)
                    } else {
                        resample(b, i - half, n - half)
                    }
                })
                .collect()
        }
    };
    if vis.reverse_order {
        result.reverse();
    }
    result
}

/// Map `n` slots to a left/right-symmetric copy of `src`: `pos = 0` (center) →
/// `src[0]`, the edges → `src[last]` (reversed if `reverse`).
fn fold_symmetric(src: &[f32], n: usize, reverse: bool) -> Vec<f32> {
    if src.is_empty() {
        return vec![0.0; n];
    }
    let m = src.len();
    let half = n.div_ceil(2);
    (0..n)
        .map(|i| {
            let dist = if i < half { half - 1 - i } else { i - half };
            let mut idx = dist * (m - 1) / half.saturating_sub(1).max(1);
            if reverse {
                idx = (m - 1) - idx.min(m - 1);
            }
            src[idx.min(m - 1)]
        })
        .collect()
}

/// Value of `src` at relative position `pos` of `slots` evenly spaced samples.
fn resample(src: &[f32], pos: usize, slots: usize) -> f32 {
    if src.is_empty() {
        return 0.0;
    }
    let m = src.len();
    src[(pos * (m - 1) / slots.saturating_sub(1).max(1)).min(m - 1)]
}

/// Rebuild each bar mesh for the blocky shapes (Bars/Levels/Particles/Spine) as
/// a rounded rect, or hide the pool when a line shape (Wave/Splitter) or a circle
/// mode is active. All four [`Direction`] variants are supported.
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
    let n = cava.bars_per_channel;
    if n == 0 {
        return;
    }
    let values = mirror_values(&cava, &vis, n);
    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    let m = vis.area_margin;
    let off = vis.area_offset;
    let eff_w = (w - 2.0 * m).max(1.0);
    let eff_h = (h - 2.0 * m).max(1.0);
    let ox = off.x * w * 0.5;
    let oy = off.y * h * 0.5;
    let glow = vis.glow_gain;

    for (bar, mesh2d, mut transform, mut visibility) in &mut bars {
        if bar.0 >= n {
            *visibility = Visibility::Hidden;
            continue;
        }
        *visibility = Visibility::Visible;
        let v = values.get(bar.0).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        let color = gradient_color(lo, hi, v.min(1.0), glow);

        // Compute center and half-extents based on direction.
        let (center, half) = match vis.direction {
            Direction::BottomTop | Direction::TopBottom => {
                let slot_w = eff_w / n as f32;
                let bar_w = (slot_w - BAR_GAP).max(1.0);
                let max_h = eff_h * MAX_HEIGHT_FRAC;
                let left = -eff_w * 0.5 + ox;
                let x = left + slot_w * (bar.0 as f32 + 0.5);
                let floor = -eff_h * 0.5 + oy;
                let ceil = eff_h * 0.5 + oy;
                let up = vis.direction == Direction::BottomTop;
                let (cy, half) = match bar_shape {
                    VisShape::Bars => {
                        let bh = (v * max_h).max(1.0);
                        let cy = if up { floor + bh * 0.5 } else { ceil - bh * 0.5 };
                        (cy, Vec2::new(bar_w * 0.5, bh * 0.5))
                    }
                    VisShape::Levels => {
                        let stepped = (v * LEVEL_STEPS).round() / LEVEL_STEPS;
                        let bh = (stepped * max_h).max(1.0);
                        let cy = if up { floor + bh * 0.5 } else { ceil - bh * 0.5 };
                        (cy, Vec2::new(bar_w * 0.5, bh * 0.5))
                    }
                    VisShape::Particles => {
                        let dot = bar_w.min(slot_w).max(2.0);
                        let cy = if up { floor + v * max_h } else { ceil - v * max_h };
                        (cy, Vec2::splat(dot * 0.5))
                    }
                    VisShape::Spine => {
                        let side = (bar_w * (0.35 + v)).clamp(2.0, max_h);
                        (oy, Vec2::splat(side * 0.5))
                    }
                    _ => unreachable!(),
                };
                (Vec2::new(x, cy), half)
            }
            Direction::LeftRight | Direction::RightLeft => {
                let slot_h = eff_h / n as f32;
                let bar_h = (slot_h - BAR_GAP).max(1.0);
                let max_w = eff_w * MAX_HEIGHT_FRAC;
                let top = eff_h * 0.5 + oy;
                let y = top - slot_h * (bar.0 as f32 + 0.5);
                let left = -eff_w * 0.5 + ox;
                let right = eff_w * 0.5 + ox;
                let ltr = vis.direction == Direction::LeftRight;
                let (cx, half) = match bar_shape {
                    VisShape::Bars => {
                        let bw = (v * max_w).max(1.0);
                        let cx = if ltr { left + bw * 0.5 } else { right - bw * 0.5 };
                        (cx, Vec2::new(bw * 0.5, bar_h * 0.5))
                    }
                    VisShape::Levels => {
                        let stepped = (v * LEVEL_STEPS).round() / LEVEL_STEPS;
                        let bw = (stepped * max_w).max(1.0);
                        let cx = if ltr { left + bw * 0.5 } else { right - bw * 0.5 };
                        (cx, Vec2::new(bw * 0.5, bar_h * 0.5))
                    }
                    VisShape::Particles => {
                        let dot = bar_h.max(2.0);
                        let cx = if ltr { left + v * max_w } else { right - v * max_w };
                        (cx, Vec2::splat(dot * 0.5))
                    }
                    VisShape::Spine => {
                        let side = (bar_h * (0.35 + v)).clamp(2.0, max_w);
                        (ox, Vec2::splat(side * 0.5))
                    }
                    _ => unreachable!(),
                };
                (Vec2::new(cx, y), half)
            }
        };

        transform.translation.x = center.x;
        transform.translation.y = center.y;

        if let Some(mesh) = meshes.get_mut(&mesh2d.0) {
            let radius = vis.items_roundness.clamp(0.0, 1.0) * half.x.min(half.y);
            apply_rounded_rect(mesh, half, radius, STROKE_FEATHER, color);
        }
    }
}

/// Center-y and half-extents of a column of height `bar_h`, growing from the
/// floor or centered (mirrored).
pub(crate) fn column_geom(lyt: &Layout, bar_h: f32, centered: bool) -> (f32, Vec2) {
    if centered {
        (0.0, Vec2::new(lyt.bar_w * 0.5, bar_h))
    } else {
        (lyt.floor + bar_h * 0.5, Vec2::new(lyt.bar_w * 0.5, bar_h * 0.5))
    }
}

/// Rebuild the antialiased stroke for the line-based box shapes (Wave, Splitter),
/// or hide it for every other mode (handled by the sprite pool / circle renderer).
/// All four [`Direction`] variants are supported.
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
        *v = if active { Visibility::Visible } else { Visibility::Hidden };
    }
    if !active {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());
    let n = cava.bars_per_channel;
    if n == 0 {
        return;
    }
    let values = mirror_values(&cava, &vis, n);
    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    let glow = vis.glow_gain;
    let m = vis.area_margin;
    let off = vis.area_offset;
    let eff_w = (w - 2.0 * m).max(1.0);
    let eff_h = (h - 2.0 * m).max(1.0);
    let ox = off.x * w * 0.5;
    let oy = off.y * h * 0.5;

    let pts: Vec<(Vec2, Color)> = match (shape, vis.direction) {
        // Wave — vertical (BottomTop / TopBottom)
        (Some(VisShape::Wave), Direction::BottomTop | Direction::TopBottom) => {
            let max_h = eff_h * MAX_HEIGHT_FRAC;
            let left = -eff_w * 0.5 + ox;
            let floor = -eff_h * 0.5 + oy;
            let ceil = eff_h * 0.5 + oy;
            let up = vis.direction == Direction::BottomTop;
            (0..=WAVE_SEGMENTS)
                .map(|k| {
                    let t = k as f32 / WAVE_SEGMENTS as f32;
                    let v = sample_h(&values, t).clamp(0.0, 1.5);
                    let x = left + t * eff_w;
                    let y = if up { floor + v * max_h } else { ceil - v * max_h };
                    (Vec2::new(x, y), gradient_color(lo, hi, v.min(1.0), glow))
                })
                .collect()
        }
        // Wave — horizontal (LeftRight / RightLeft)
        (Some(VisShape::Wave), Direction::LeftRight | Direction::RightLeft) => {
            let max_w = eff_w * MAX_HEIGHT_FRAC;
            let top = eff_h * 0.5 + oy;
            let left = -eff_w * 0.5 + ox;
            let right = eff_w * 0.5 + ox;
            let ltr = vis.direction == Direction::LeftRight;
            (0..=WAVE_SEGMENTS)
                .map(|k| {
                    let t = k as f32 / WAVE_SEGMENTS as f32;
                    let v = sample_h(&values, t).clamp(0.0, 1.5);
                    let y = top - t * eff_h;
                    let x = if ltr { left + v * max_w } else { right - v * max_w };
                    (Vec2::new(x, y), gradient_color(lo, hi, v.min(1.0), glow))
                })
                .collect()
        }
        // Splitter — vertical
        (Some(VisShape::Splitter), Direction::BottomTop | Direction::TopBottom) => {
            let max_h = eff_h * MAX_HEIGHT_FRAC;
            let left = -eff_w * 0.5 + ox;
            let slot_w = eff_w / n as f32;
            let up = vis.direction == Direction::BottomTop;
            let sign = if up { 1.0 } else { -1.0 };
            (0..n)
                .map(|i| {
                    let v = values[i].clamp(0.0, 1.5);
                    let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
                    let x = left + slot_w * (i as f32 + 0.5);
                    let y = sign * dir * v * max_h * 0.5 + oy;
                    (Vec2::new(x, y), gradient_color(lo, hi, v.min(1.0), glow))
                })
                .collect()
        }
        // Splitter — horizontal
        (Some(VisShape::Splitter), Direction::LeftRight | Direction::RightLeft) => {
            let max_w = eff_w * MAX_HEIGHT_FRAC;
            let top = eff_h * 0.5 + oy;
            let slot_h = eff_h / n as f32;
            let ltr = vis.direction == Direction::LeftRight;
            let sign = if ltr { 1.0 } else { -1.0 };
            (0..n)
                .map(|i| {
                    let v = values[i].clamp(0.0, 1.5);
                    let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
                    let y = top - slot_h * (i as f32 + 0.5);
                    let x = sign * dir * v * max_w * 0.5 + ox;
                    (Vec2::new(x, y), gradient_color(lo, hi, v.min(1.0), glow))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_basic_geometry() {
        let (w, h, n) = (1000.0, 600.0, 10);
        let lyt = Layout::new(w, h, n, false);
        assert_eq!(lyt.left, -500.0);
        assert_eq!(lyt.floor, -300.0);
        assert_eq!(lyt.slot_w, 100.0);
        assert_eq!(lyt.bar_w, 100.0 - BAR_GAP);
        assert!((lyt.max_h - h * MAX_HEIGHT_FRAC).abs() < 1e-3);
        // Bars are centered in their slots, evenly spaced.
        assert_eq!(lyt.bar_x(0), -500.0 + 50.0);
        assert_eq!(lyt.bar_x(9), 500.0 - 50.0);
        // half_height halves the full-scale span.
        let half = Layout::new(w, h, n, true);
        assert!((half.max_h - lyt.max_h * 0.5).abs() < 1e-3);
    }

    #[test]
    fn layout_with_margin_shrinks_and_offsets() {
        let lyt = Layout::new_with_margin(1000.0, 600.0, 10, false, 50.0, Vec2::ZERO);
        // Effective width 900 → slots 90 wide, left edge -450.
        assert_eq!(lyt.slot_w, 90.0);
        assert_eq!(lyt.left, -450.0);
        assert_eq!(lyt.floor, -250.0);
    }

    #[test]
    fn column_geom_floor_anchored_vs_centered() {
        let lyt = Layout::new(1000.0, 600.0, 10, false);
        let (cy, half) = column_geom(&lyt, 120.0, false);
        // Floor-anchored: center is half the height above the floor.
        assert_eq!(cy, lyt.floor + 60.0);
        assert_eq!(half.y, 60.0);
        assert_eq!(half.x, lyt.bar_w * 0.5);

        let (cy_c, half_c) = column_geom(&lyt, 120.0, true);
        assert_eq!(cy_c, 0.0);
        assert_eq!(half_c.y, 120.0); // centered uses full height as half-extent
    }

    #[test]
    fn sample_h_endpoints_and_degenerate() {
        assert_eq!(sample_h(&[], 0.5), 0.0);
        assert_eq!(sample_h(&[0.42], 0.5), 0.42);
        let v = vec![0.0, 0.25, 0.5, 0.75, 1.0];
        assert!((sample_h(&v, 0.0) - 0.0).abs() < 1e-5);
        assert!((sample_h(&v, 1.0) - 1.0).abs() < 1e-5);
        // Clamps out-of-range t.
        assert!((sample_h(&v, -1.0) - 0.0).abs() < 1e-5);
        assert!((sample_h(&v, 2.0) - 1.0).abs() < 1e-5);
        // Monotonic non-decreasing across a ramp.
        let mut prev = f32::NEG_INFINITY;
        for k in 0..=64 {
            let s = sample_h(&v, k as f32 / 64.0);
            assert!(s + 1e-5 >= prev);
            prev = s;
        }
    }

    #[test]
    fn fold_symmetric_is_mirror_symmetric() {
        assert_eq!(fold_symmetric(&[], 4, false), vec![0.0; 4]);
        let src = vec![1.0, 0.6, 0.2]; // bass→treble
        let out = fold_symmetric(&src, 8, false);
        assert_eq!(out.len(), 8);
        for i in 0..out.len() {
            assert!(
                (out[i] - out[out.len() - 1 - i]).abs() < 1e-6,
                "not symmetric at {i}: {out:?}"
            );
        }
    }

    #[test]
    fn resample_endpoints_and_empty() {
        assert_eq!(resample(&[], 0, 4), 0.0);
        let src = vec![10.0, 20.0, 30.0];
        assert_eq!(resample(&src, 0, 4), 10.0);
        // Last slot maps to the last source sample.
        assert_eq!(resample(&src, 3, 4), 30.0);
    }

    #[test]
    fn mirror_values_off_mode_matches_spread_mono() {
        let cava = Cava {
            bars: vec![0.0, 1.0, 0.0, 0.5],
            bars_per_channel: 4,
            channels: 1,
        };
        let vis = VisSettings {
            mirror: MirrorMode::Off,
            reverse_order: false,
            ..VisSettings::default()
        };
        let got = mirror_values(&cava, &vis, 4);
        let mut want = cava.mono();
        spread_monstercat(&mut want, vis.monstercat);
        assert_eq!(got, want);

        // reverse_order flips the result.
        let vis_rev = VisSettings { reverse_order: true, ..vis };
        let mut rev = mirror_values(&cava, &vis_rev, 4);
        rev.reverse();
        assert_eq!(rev, got);
    }

    #[test]
    fn mirror_values_full_is_symmetric() {
        let cava = Cava {
            bars: vec![1.0, 0.5, 0.1],
            bars_per_channel: 3,
            channels: 1,
        };
        let vis = VisSettings {
            mirror: MirrorMode::Full,
            reverse_order: false,
            monstercat: 1.0, // disable spreading for an exact symmetry check
            ..VisSettings::default()
        };
        let out = mirror_values(&cava, &vis, 8);
        for i in 0..out.len() {
            assert!((out[i] - out[out.len() - 1 - i]).abs() < 1e-6, "asymmetry at {i}");
        }
    }
}
