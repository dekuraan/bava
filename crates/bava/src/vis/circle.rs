// SPDX-License-Identifier: MIT OR Apache-2.0
//! Circular ([`VisFamily::Circle`]) visualizers.
//!
//! Renders every circle-family [`DrawingMode`] as a *distinct* shape, switching
//! on [`DrawingMode::shape`]:
//!
//! - **Wave** — a closed ring whose radius is modulated by the spectrum (the
//!   spectrum is folded by angle about the vertical axis so the ring is
//!   left/right symmetric and smoothstep-interpolated across many segments,
//!   giving a continuously deforming smooth blob). The outline is a
//!   feather-antialiased stroke [`Mesh2d`]; an optional translucent fill is a
//!   deforming triangle-fan [`Mesh2d`].
//! - **Bars** — radial spectrum spokes growing outward from a base ring.
//! - **Levels** — radial spokes whose length snaps to discrete VU steps.
//! - **Particles** — one small square per bar floating at its radial level.
//! - **Spine** — squares on the base ring, growing with level.
//!
//! Bars/Levels/Particles/Spine draw one rounded rect per bar, all accumulated
//! into a **single** batched [`Mesh2d`] (analogous to the box bars); Wave uses the
//! ring stroke + fill, hidden while a blocky shape is active. All are rebuilt from
//! the [`Cava`] resource each frame.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::cava::Cava;
use crate::vis::bars::{BAR_GAP, LEVEL_STEPS};
use crate::vis::stroke::{
    MeshBatch, STROKE_FEATHER, apply_stroke, empty_stroke_mesh, stroke_material,
};
use crate::vis::{
    DrawingMode, VisFamily, VisSettings, VisShape, gradient_color, spread_monstercat,
};

/// Segments around the ring. Higher = smoother curve.
const SEGMENTS: usize = 256;
/// Maximum visual radius of the circle visualizer as a fraction of the smaller
/// window dimension. The inner-ring position and peak amplitude both draw from
/// this budget, controlled by [`VisSettings::inner_radius`].
const MAX_RADIUS_FRAC: f32 = 0.42;

/// Resolve `base` (inner ring radius) and `amp` (peak outward reach) from the
/// `inner_radius` setting (0 = bars from center, 1 = no space for bars).
fn circle_radii(extent: f32, inner_radius: f32) -> (f32, f32) {
    let r = inner_radius.clamp(0.0, 0.95);
    let total = extent * MAX_RADIUS_FRAC;
    (total * r, total * (1.0 - r))
}

/// Handles for the fill mesh/material so they can be updated each frame.
#[derive(Resource)]
struct FillHandles {
    mesh: Handle<Mesh>,
    material: Handle<ColorMaterial>,
}

/// Handle for the ring-outline stroke mesh, rebuilt each frame.
#[derive(Resource)]
struct RingHandles {
    mesh: Handle<Mesh>,
}

/// Marks the fill-blob entity.
#[derive(Component)]
struct FillBlob;

/// Marks the ring-outline stroke entity.
#[derive(Component)]
struct RingStroke;

/// Marks the single entity holding **every** radial bar (Bars/Levels/Particles/
/// Spine circle shapes), mirroring the box bar batch. See [`MeshBatch`] for why
/// these are one mesh rather than one per bar.
#[derive(Component)]
struct CircleBarBatch;

/// Handle for the batched radial bar mesh, rebuilt each frame.
#[derive(Resource)]
struct CircleBarHandles {
    mesh: Handle<Mesh>,
}

/// Circular visualizer plugin.
pub struct CirclePlugin;

impl Plugin for CirclePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_circle).add_systems(
            Update,
            // The radial bars are one batched mesh rebuilt from scratch each
            // frame, so a live bar-count change needs no pool reconciliation.
            // `update_ring` draws both the Wave outline and its optional fill
            // from one geometry pass.
            (update_circle_bars, update_ring),
        );
    }
}

/// Spawn the (hidden) fill blob, ring-outline stroke, and the radial bar batch.
fn setup_circle(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    // Every radial bar lives in one mesh on one entity; per-vertex color carries
    // the gradient, so one blend material covers them all.
    let bar_mesh = meshes.add(empty_stroke_mesh());
    commands.spawn((
        Mesh2d(bar_mesh.clone()),
        MeshMaterial2d(materials.add(stroke_material())),
        Transform::from_xyz(0.0, 0.0, 0.5),
        Visibility::Hidden,
        CircleBarBatch,
    ));
    commands.insert_resource(CircleBarHandles { mesh: bar_mesh });

    let mesh = meshes.add(fan_mesh());
    let material = materials.add(ColorMaterial::from(Color::NONE));
    commands.spawn((
        Mesh2d(mesh.clone()),
        MeshMaterial2d(material.clone()),
        Transform::from_xyz(0.0, 0.0, -5.0),
        Visibility::Hidden,
        FillBlob,
    ));
    commands.insert_resource(FillHandles { mesh, material });

    // The ring outline is now a feathered (antialiased) stroke mesh rather than a
    // gizmo. White blend material; the per-vertex gradient supplies the color.
    let ring_mesh = meshes.add(empty_stroke_mesh());
    let ring_material = materials.add(stroke_material());
    commands.spawn((
        Mesh2d(ring_mesh.clone()),
        MeshMaterial2d(ring_material),
        Transform::from_xyz(0.0, 0.0, 1.0),
        Visibility::Hidden,
        RingStroke,
    ));
    commands.insert_resource(RingHandles { mesh: ring_mesh });
}

/// Draw the radial bars for the blocky circle shapes (Bars/Levels/Particles/
/// Spine) into one batched mesh, or hide it when Wave (the smooth blob) or a box
/// mode is active.
#[allow(clippy::too_many_arguments)]
fn update_circle_bars(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    handles: Res<CircleBarHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut batch: Local<MeshBatch>,
    mut bars: Query<&mut Visibility, With<CircleBarBatch>>,
) {
    let shape = (mode.family() == VisFamily::Circle).then(|| mode.shape());
    let bar_shape = match shape {
        Some(s @ (VisShape::Bars | VisShape::Levels | VisShape::Particles | VisShape::Spine)) => s,
        // Wave (handled by the ring/fill) or a box mode: hide the radial bars.
        _ => {
            for mut v in &mut bars {
                // set_if_neq: rewriting Hidden every frame would re-dirty
                // visibility propagation in every other mode.
                v.set_if_neq(Visibility::Hidden);
            }
            return;
        }
    };

    let Some(window) = windows.iter().next() else {
        return;
    };
    let extent = window.width().min(window.height());

    let mut values = cava.mono();
    let n = values.len();
    if n == 0 {
        for mut v in &mut bars {
            v.set_if_neq(Visibility::Hidden);
        }
        return;
    }
    for mut v in &mut bars {
        v.set_if_neq(Visibility::Visible);
    }
    spread_monstercat(&mut values, vis.monstercat);

    let (base, amp) = circle_radii(extent, vis.inner_radius);
    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    let glow = vis.glow_gain;
    // Tangential slot width from the base-ring circumference, minus the gap.
    let slot_w = (TAU * base / n as f32).max(1.0);
    let bar_w = (slot_w - BAR_GAP).max(1.0);

    crate::profile_scope!("circle_bar_meshes");
    batch.clear();
    for (index, &raw) in values.iter().enumerate() {
        let v = raw.clamp(0.0, 1.5);
        // Apply vis.rotation to the starting angle.
        let ang = index as f32 / n as f32 * TAU - FRAC_PI_2 + vis.rotation;
        let (cos, sin) = (ang.cos(), ang.sin());

        // Each shape resolves to (radius of the rect center, half extents, rotation).
        let (radius, half, rot) = match bar_shape {
            VisShape::Bars => {
                let len = (amp * v).max(1.0);
                (
                    base + len * 0.5,
                    Vec2::new(bar_w * 0.5, len * 0.5),
                    ang - FRAC_PI_2,
                )
            }
            VisShape::Levels => {
                let stepped = (v * LEVEL_STEPS).round() / LEVEL_STEPS;
                let len = (amp * stepped).max(1.0);
                (
                    base + len * 0.5,
                    Vec2::new(bar_w * 0.5, len * 0.5),
                    ang - FRAC_PI_2,
                )
            }
            VisShape::Particles => {
                let dot = bar_w.max(2.0);
                (base + amp * v, Vec2::splat(dot * 0.5), 0.0)
            }
            VisShape::Spine => {
                let side = (bar_w * (0.35 + v)).clamp(2.0, amp.max(2.0));
                (base, Vec2::splat(side * 0.5), 0.0)
            }
            _ => unreachable!("bar_shape is constrained above"),
        };

        let center = Vec2::new(cos * radius, sin * radius);
        let round = vis.items_roundness.clamp(0.0, 1.0) * half.x.min(half.y);
        let color = gradient_color(lo, hi, v.min(1.0), glow);
        batch.push_rounded_rect(center, half, rot, round, STROKE_FEATHER, color);
    }

    if let Some(mut mesh) = meshes.get_mut(&handles.mesh) {
        batch.write(&mut mesh);
    }
}

/// A triangle fan: vertex 0 is the center, 1..=SEGMENTS are the ring. Positions
/// are placeholders (overwritten each frame); indices/normals/uvs are fixed.
fn fan_mesh() -> Mesh {
    let verts = SEGMENTS + 1;
    let positions = vec![[0.0f32, 0.0, 0.0]; verts];
    let normals = vec![[0.0f32, 0.0, 1.0]; verts];
    let uvs = vec![[0.0f32, 0.0]; verts];
    let mut indices = Vec::with_capacity(SEGMENTS * 3);
    for k in 0..SEGMENTS {
        indices.push(0u32);
        indices.push(1 + k as u32);
        indices.push(1 + ((k + 1) % SEGMENTS) as u32);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Radius at angle parameter `t` (0..1 around the circle), folded about the
/// vertical axis so `sample(t) == sample(1 - t)` — perfectly left/right
/// symmetric, with low frequencies at the top and highs at the bottom.
fn sample(values: &[f32], t: f32) -> f32 {
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return values[0];
    }
    let a = if t <= 0.5 { t } else { 1.0 - t }; // 0..0.5, symmetric
    let pos = a * 2.0 * (n - 1) as f32; // 0..(n-1)
    let i0 = pos.floor() as usize;
    let i1 = (i0 + 1).min(n - 1);
    let frac = pos - i0 as f32;
    let s = frac * frac * (3.0 - 2.0 * frac); // smoothstep
    values[i0.min(n - 1)] + (values[i1] - values[i0.min(n - 1)]) * s
}

/// The blob ring vertices for `values` (already monstercat-spread), matching the
/// rendered outline/fill. `extent` is the smaller window dimension,
/// `inner_radius` and `rotation` mirror the live [`VisSettings`] so the physics
/// collider tracks the visual exactly.
pub(crate) fn blob_ring(
    values: &[f32],
    extent: f32,
    inner_radius: f32,
    rotation: f32,
) -> Vec<Vec2> {
    let (base, amp) = circle_radii(extent, inner_radius);
    (0..SEGMENTS)
        .map(|k| ring_point(values, k, base, amp, rotation).0)
        .collect()
}

/// Position of ring point `k` for a given spectrum, base radius, amplitude and
/// angular offset.
fn ring_point(values: &[f32], k: usize, base: f32, amp: f32, rotation: f32) -> (Vec2, f32) {
    let t = k as f32 / SEGMENTS as f32;
    let v = sample(values, t).clamp(0.0, 1.5);
    let r = base + amp * v;
    let ang = t * TAU - FRAC_PI_2 + rotation;
    (Vec2::new(ang.cos() * r, ang.sin() * r), v)
}

/// Rebuild the antialiased ring-outline stroke and, when enabled, the translucent
/// fill blob for the Wave circle mode — from a **single** ring-geometry pass. (The
/// fill previously recomputed the whole `mono` → monstercat → `ring_point`
/// pipeline a second time every frame.)
#[allow(clippy::too_many_arguments)]
fn update_ring(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    ring: Res<RingHandles>,
    fill: Res<FillHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut ring_q: Query<&mut Visibility, (With<RingStroke>, Without<FillBlob>)>,
    mut fill_q: Query<&mut Visibility, (With<FillBlob>, Without<RingStroke>)>,
) {
    // The ring outline / fill are the Wave circle shape only; the other circle
    // shapes use the radial bar pool.
    let ring_active = mode.family() == VisFamily::Circle && mode.shape() == VisShape::Wave;
    let fill_active = ring_active && vis.filling;
    for mut v in &mut ring_q {
        v.set_if_neq(if ring_active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        });
    }
    for mut v in &mut fill_q {
        v.set_if_neq(if fill_active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        });
    }
    if !ring_active {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let extent = window.width().min(window.height());

    let mut values = cava.mono();
    if values.is_empty() {
        return;
    }
    spread_monstercat(&mut values, vis.monstercat);
    let (base, amp) = circle_radii(extent, vis.inner_radius);
    let rot = vis.rotation;

    // Compute the ring geometry once; both the outline stroke and the fill fan
    // read from it.
    let ring_pts: Vec<(Vec2, f32)> = (0..SEGMENTS)
        .map(|k| ring_point(&values, k, base, amp, rot))
        .collect();

    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    let glow = vis.glow_gain;
    if let Some(mut mesh) = meshes.get_mut(&ring.mesh) {
        let pts: Vec<(Vec2, Color)> = ring_pts
            .iter()
            .map(|(pos, v)| (*pos, gradient_color(lo, hi, v.min(1.0), glow)))
            .collect();
        apply_stroke(
            &mut mesh,
            &pts,
            vis.line_thickness * 0.5,
            STROKE_FEATHER,
            true,
        );
    }

    if !fill_active {
        return;
    }
    if let Some(mut mesh) = meshes.get_mut(&fill.mesh) {
        let mut positions = Vec::with_capacity(SEGMENTS + 1);
        positions.push([0.0, 0.0, 0.0]); // center
        let mut peak = 0.0f32;
        for (pos, v) in &ring_pts {
            peak = peak.max(*v);
            positions.push([pos.x, pos.y, 0.0]);
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

        // Tint the fill by loudness; keep it translucent so art shows through.
        if let Some(mut mat) = materials.get_mut(&fill.material) {
            mat.color =
                gradient_color(vis.fg_lo(), vis.fg_hi(), peak, vis.glow_gain).with_alpha(0.28);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_radii_split_and_clamp() {
        let extent = 1000.0;
        let total = extent * MAX_RADIUS_FRAC;
        let (base, amp) = circle_radii(extent, 0.4);
        assert!((base - total * 0.4).abs() < 1e-3);
        assert!((amp - total * 0.6).abs() < 1e-3);
        // The base + full amplitude budget is the whole radius.
        assert!((base + amp - total).abs() < 1e-3);
        // inner_radius is clamped to ≤ 0.95 so there's always room for bars.
        let (base_hi, amp_hi) = circle_radii(extent, 5.0);
        assert!((base_hi - total * 0.95).abs() < 1e-3);
        assert!(amp_hi > 0.0);
    }

    #[test]
    fn sample_is_folded_symmetric() {
        let values = vec![1.0, 0.7, 0.3, 0.1];
        // sample(t) == sample(1 - t): the ring is left/right symmetric.
        for k in 0..=20 {
            let t = k as f32 / 20.0;
            assert!(
                (sample(&values, t) - sample(&values, 1.0 - t)).abs() < 1e-5,
                "fold broke at t={t}"
            );
        }
        // Single bar is flat.
        assert_eq!(sample(&[0.5], 0.3), 0.5);
        // t=0 hits the first (bass) bar.
        assert!((sample(&values, 0.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn blob_ring_tracks_amplitude_and_is_closed() {
        let extent = 1000.0;
        let (base, amp) = circle_radii(extent, 0.38);

        // Silence → a circle at the base radius.
        let silent = blob_ring(&[0.0; 16], extent, 0.38, 0.0);
        assert_eq!(silent.len(), SEGMENTS);
        for p in &silent {
            assert!(
                (p.length() - base).abs() < 1e-2,
                "silent rim should be the base radius"
            );
        }

        // Loud → radii grow with amplitude, bounded by base + amp·v.
        let loud = blob_ring(&[1.0; 16], extent, 0.38, 0.0);
        for p in &loud {
            let r = p.length();
            assert!(r > base + 1.0, "loud rim should expand past base");
            assert!(
                r <= base + amp * 1.5 + 1e-2,
                "rim within the clamped budget"
            );
        }
    }

    #[test]
    fn ring_point_places_first_segment_at_the_bottom() {
        // k=0 → t=0 → angle = -π/2 → straight down, at radius base (silent).
        let (p, v) = ring_point(&[0.0, 0.0, 0.0], 0, 100.0, 50.0, 0.0);
        assert!((v - 0.0).abs() < 1e-6);
        assert!(p.x.abs() < 1e-3, "x≈0 at the bottom");
        assert!((p.y + 100.0).abs() < 1e-3, "y≈-base at the bottom");
    }
}
