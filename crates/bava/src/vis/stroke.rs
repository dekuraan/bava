// SPDX-License-Identifier: GPL-3.0-or-later
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

use std::f32::consts::{FRAC_PI_2, PI};

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

/// Arc segments per rounded-rect corner.
const CORNER_SEGS: usize = 4;

/// Overwrite `mesh` with a filled, feather-antialiased rounded rectangle centered
/// at the origin: `half` extents, corner `radius` (clamped to the shorter half),
/// a `feather`-px alpha ramp at the edge, filled with `color`. Position it with
/// the entity's `Transform`. Same blend [`stroke_material`] applies.
pub(crate) fn apply_rounded_rect(
    mesh: &mut Mesh,
    half: Vec2,
    radius: f32,
    feather: f32,
    color: Color,
) {
    let hx = half.x.max(0.01);
    let hy = half.y.max(0.01);
    let r = radius.clamp(0.0, hx.min(hy));

    // Boundary points (CCW) with their outward normals, walking the four corner
    // arcs. Straight edges fall out of connecting consecutive arc endpoints.
    let centers = [
        (Vec2::new(hx - r, -(hy - r)), -FRAC_PI_2), // right-bottom: -90°..0°
        (Vec2::new(hx - r, hy - r), 0.0),           // right-top:     0°..90°
        (Vec2::new(-(hx - r), hy - r), FRAC_PI_2),  // left-top:     90°..180°
        (Vec2::new(-(hx - r), -(hy - r)), PI),      // left-bottom: 180°..270°
    ];
    let lin = color.to_linear();
    let rgb = [lin.red, lin.green, lin.blue];

    let mut boundary: Vec<(Vec2, Vec2)> = Vec::with_capacity(4 * (CORNER_SEGS + 1));
    for (center, a0) in centers {
        for s in 0..=CORNER_SEGS {
            let ang = a0 + (s as f32 / CORNER_SEGS as f32) * FRAC_PI_2;
            let dir = Vec2::new(ang.cos(), ang.sin());
            boundary.push((center + dir * r, dir));
        }
    }
    let nb = boundary.len();

    // Vertex 0 = center; 1..=nb = boundary (full alpha); nb+1..=2nb = feather (0).
    let mut positions = Vec::with_capacity(1 + nb * 2);
    let mut colors = Vec::with_capacity(1 + nb * 2);
    positions.push([0.0, 0.0, 0.0]);
    colors.push([rgb[0], rgb[1], rgb[2], lin.alpha]);
    for (p, _) in &boundary {
        positions.push([p.x, p.y, 0.0]);
        colors.push([rgb[0], rgb[1], rgb[2], lin.alpha]);
    }
    for (p, n) in &boundary {
        let q = *p + *n * feather;
        positions.push([q.x, q.y, 0.0]);
        colors.push([rgb[0], rgb[1], rgb[2], 0.0]);
    }

    let b0 = 1u32;
    let o0 = 1 + nb as u32;
    let mut indices = Vec::with_capacity(nb * 9);
    for i in 0..nb {
        let i1 = (i + 1) % nb;
        let (bi, bi1) = (b0 + i as u32, b0 + i1 as u32);
        let (oi, oi1) = (o0 + i as u32, o0 + i1 as u32);
        // Fan fill from the center.
        indices.extend_from_slice(&[0, bi, bi1]);
        // Feather ring quad (boundary → outer).
        indices.extend_from_slice(&[bi, bi1, oi1, bi, oi1, oi]);
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
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
    apply_stroke_tapered(mesh, pts, hw, hw, feather, closed);
}

/// Like [`apply_stroke`], but the core half-width ramps linearly from `hw_start`
/// at the first point to `hw_end` at the last, giving a tapered (triangular /
/// comet) stroke. Always treated as open. The `feather` ramp is added outside the
/// (varying) core, so the stroke stays antialiased even where it narrows to a point.
pub(crate) fn apply_stroke_tapered(
    mesh: &mut Mesh,
    pts: &[(Vec2, Color)],
    hw_start: f32,
    hw_end: f32,
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

        // Core half-width for this point, lerped start→end along the stroke.
        let t = i as f32 / (n - 1) as f32;
        let hw = hw_start + (hw_end - hw_start) * t;
        // Four lanes per centerline point: outer+ (a=0), core+ (a=1), core- (a=1),
        // outer- (a=0). Offsets are along the point's normal.
        let lanes = [(hw + feather, 0.0), (hw, 1.0), (-hw, 1.0), (-(hw + feather), 0.0)];

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
