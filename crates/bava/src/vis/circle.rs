//! Smooth circular visualizer.
//!
//! Draws a closed ring whose radius is modulated by the spectrum. The spectrum
//! is folded by angle about the vertical axis (so the ring is left/right
//! symmetric) and smoothstep-interpolated across many segments, giving a
//! continuously deforming smooth blob. The outline is drawn with gizmos; an
//! optional translucent fill is a deforming triangle-fan [`Mesh2d`]. Both are
//! rebuilt from the [`Cava`] resource every frame.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::cava::Cava;
use crate::vis::{gradient_color, spread_monstercat, DrawingMode, VisFamily, VisSettings};

/// Segments around the ring. Higher = smoother curve.
const SEGMENTS: usize = 256;
/// Resting radius as a fraction of the smaller window dimension.
const BASE_FRAC: f32 = 0.16;
/// How far peaks push outward, as a fraction of the smaller window dimension.
const AMP_FRAC: f32 = 0.26;

/// Handles for the fill mesh/material so they can be updated each frame.
#[derive(Resource)]
struct FillHandles {
    mesh: Handle<Mesh>,
    material: Handle<ColorMaterial>,
}

/// Marks the fill-blob entity.
#[derive(Component)]
struct FillBlob;

/// Smooth circular visualizer plugin.
pub struct CirclePlugin;

impl Plugin for CirclePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_circle)
            .add_systems(Update, (draw_ring, update_fill));
    }
}

/// Configure line width and spawn the (hidden) fill blob.
fn setup_circle(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut store: ResMut<GizmoConfigStore>,
    vis: Res<VisSettings>,
) {
    store.config_mut::<DefaultGizmoConfigGroup>().0.line.width = vis.line_thickness;

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

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
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
/// rendered outline/fill. `extent` is the smaller window dimension. Exposed so
/// the physics planet collider can track the visual shape exactly.
pub(crate) fn blob_ring(values: &[f32], extent: f32) -> Vec<Vec2> {
    let (base, amp) = (extent * BASE_FRAC, extent * AMP_FRAC);
    (0..SEGMENTS).map(|k| ring_point(values, k, base, amp).0).collect()
}

/// Position of ring point `k` for a given spectrum, base radius and amplitude.
fn ring_point(values: &[f32], k: usize, base: f32, amp: f32) -> (Vec2, f32) {
    let t = k as f32 / SEGMENTS as f32;
    let v = sample(values, t).clamp(0.0, 1.5);
    let r = base + amp * v;
    let ang = t * TAU - FRAC_PI_2; // start at the top
    (Vec2::new(ang.cos() * r, ang.sin() * r), v)
}

/// Draw the outline ring when the circle style is active.
fn draw_ring(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut gizmos: Gizmos,
) {
    // The circle renderer stands in for every radial (circle) mode for now.
    if mode.family() != VisFamily::Circle {
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
    let (base, amp) = (extent * BASE_FRAC, extent * AMP_FRAC);

    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());
    let points = (0..=SEGMENTS).map(|k| {
        let (pos, v) = ring_point(&values, k % SEGMENTS, base, amp);
        (pos, gradient_color(lo, hi, v))
    });
    gizmos.linestrip_gradient_2d(points);
}

/// Update / show the translucent fill blob when enabled.
fn update_fill(
    mode: Res<DrawingMode>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    fill: Res<FillHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut q: Query<&mut Visibility, With<FillBlob>>,
) {
    let active = mode.family() == VisFamily::Circle && vis.filling;
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
    let extent = window.width().min(window.height());

    let mut values = cava.mono();
    if values.is_empty() {
        return;
    }
    spread_monstercat(&mut values, vis.monstercat);
    let (base, amp) = (extent * BASE_FRAC, extent * AMP_FRAC);

    if let Some(mesh) = meshes.get_mut(&fill.mesh) {
        let mut positions = Vec::with_capacity(SEGMENTS + 1);
        positions.push([0.0, 0.0, 0.0]); // center
        let mut peak = 0.0f32;
        for k in 0..SEGMENTS {
            let (pos, v) = ring_point(&values, k, base, amp);
            peak = peak.max(v);
            positions.push([pos.x, pos.y, 0.0]);
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

        // Tint the fill by loudness; keep it translucent so art shows through.
        if let Some(mat) = materials.get_mut(&fill.material) {
            mat.color = gradient_color(vis.fg_lo(), vis.fg_hi(), peak).with_alpha(0.28);
        }
    }
}
