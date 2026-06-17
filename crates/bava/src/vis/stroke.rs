//! Feathered triangle-strip stroke meshes for smooth, glowing lines.
//!
//! Bevy gizmo lines have no analytic antialiasing and no soft edge. Instead we
//! build a triangle mesh for the polyline: a solid core spanning `±half_width`
//! plus a `feather`-wide ramp to alpha 0 on each side. Linear interpolation of
//! the per-vertex alpha across that ramp gives a smooth (resolution-independent)
//! antialiased edge, and — with the HDR camera + bloom — the bright core glows.
//!
//! The mesh carries `ATTRIBUTE_COLOR`, so a plain blend [`ColorMaterial`] tinted
//! white multiplies through the per-vertex gradient/alpha — no custom shader.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::sprite_render::AlphaMode2d;

/// Default antialiasing feather half-width, in pixels, for stroke edges.
pub(crate) const STROKE_FEATHER: f32 = 1.5;

/// A new, empty stroke mesh (filled per frame by [`apply_stroke`]).
pub(crate) fn empty_stroke_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
}

/// A white, alpha-blended material; the per-vertex colors supply the actual hue
/// and the feather alpha.
pub(crate) fn stroke_material() -> ColorMaterial {
    ColorMaterial {
        color: Color::WHITE,
        alpha_mode: AlphaMode2d::Blend,
        ..default()
    }
}

/// Overwrite `mesh` with a feathered stroke through `pts` (each a position +
/// HDR color). The core spans `±hw`; `feather` is the half-width of the alpha
/// ramp added outside the core. `closed` joins the last point back to the first.
///
/// Reuses the mesh's handle, so updating it every frame causes no asset churn.
pub(crate) fn apply_stroke(
    mesh: &mut Mesh,
    pts: &[(Vec2, Color)],
    hw: f32,
    feather: f32,
    closed: bool,
) {
    let n = pts.len();
    if n < 2 {
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::new());
        mesh.insert_indices(Indices::U32(Vec::new()));
        return;
    }

    // Four lanes per centerline point: outer+ (a=0), core+ (a=1), core- (a=1),
    // outer- (a=0). Offsets are along the point's normal.
    let lanes = [(hw + feather, 0.0), (hw, 1.0), (-hw, 1.0), (-(hw + feather), 0.0)];
    let mut positions = Vec::with_capacity(n * 4);
    let mut colors = Vec::with_capacity(n * 4);

    for i in 0..n {
        let p = pts[i].0;
        // Tangent from neighbours (wrapping when closed, clamped when open).
        let prev = if i == 0 {
            if closed { pts[n - 1].0 } else { p }
        } else {
            pts[i - 1].0
        };
        let next = if i == n - 1 {
            if closed { pts[0].0 } else { p }
        } else {
            pts[i + 1].0
        };
        let mut tan = next - prev;
        if tan.length_squared() < 1e-9 {
            tan = Vec2::X;
        }
        let tan = tan.normalize();
        let nrm = Vec2::new(-tan.y, tan.x);

        let lin = pts[i].1.to_linear();
        for (off, edge) in lanes {
            let q = p + nrm * off;
            positions.push([q.x, q.y, 0.0]);
            colors.push([lin.red, lin.green, lin.blue, lin.alpha * edge]);
        }
    }

    // Three quads (left feather, core, right feather) per segment.
    let segs = if closed { n } else { n - 1 };
    let mut indices = Vec::with_capacity(segs * 3 * 6);
    for s in 0..segs {
        let i0 = (s * 4) as u32;
        let i1 = (((s + 1) % n) * 4) as u32;
        for lane in 0..3u32 {
            let (a0, a1, b0, b1) = (i0 + lane, i0 + lane + 1, i1 + lane, i1 + lane + 1);
            indices.extend_from_slice(&[a0, a1, b1, a0, b1, b0]);
        }
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}
