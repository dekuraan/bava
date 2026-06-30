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

/// A new stroke mesh holding a single degenerate, fully-transparent triangle
/// (filled with real geometry per frame by [`apply_stroke`]).
///
/// It is *not* truly empty on purpose: Bevy's mesh slab allocator logs a
/// "use-after-free: attempted to copy element data for an unallocated key" every
/// frame a `Mesh` asset is extracted with **zero** vertices — `allocate_meshes`
/// skips allocation for an empty vertex buffer but still attempts the data copy
/// (`bevy_render::mesh::allocator`). Stroke meshes are extracted before their
/// first `apply_stroke` and whenever they have <2 points, so we keep a zero-area,
/// zero-alpha triangle (which rasterizes nothing) instead of an empty buffer.
pub(crate) fn empty_stroke_mesh() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    write_degenerate_tri(&mut mesh);
    mesh
}

/// Overwrite `mesh` with a single zero-area, zero-alpha triangle — an invisible
/// stand-in for an empty mesh that keeps the vertex buffer non-empty so the mesh
/// slab allocator never sees a zero-vertex extract (see [`empty_stroke_mesh`]).
fn write_degenerate_tri(mesh: &mut Mesh) {
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32, 0.0, 0.0]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32, 0.0, 0.0, 0.0]; 3]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
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
        // <2 points can't form a stroke; emit an invisible degenerate triangle
        // rather than a zero-vertex mesh (which the slab allocator rejects).
        write_degenerate_tri(mesh);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(mesh: &Mesh) -> (usize, usize) {
        (mesh.count_vertices(), mesh.indices().map(|i| i.len()).unwrap_or(0))
    }

    #[test]
    fn stroke_too_short_yields_degenerate_tri() {
        // <2 points yields an invisible degenerate triangle, NOT a zero-vertex
        // mesh: Bevy's slab allocator errors every frame on a 0-vertex extract.
        let mut mesh = empty_stroke_mesh();
        assert_eq!(counts(&mesh), (3, 3), "empty stroke mesh is a degenerate tri");
        apply_stroke(&mut mesh, &[], 2.0, STROKE_FEATHER, false);
        assert_eq!(counts(&mesh), (3, 3));
        apply_stroke(&mut mesh, &[(Vec2::ZERO, Color::WHITE)], 2.0, STROKE_FEATHER, false);
        assert_eq!(counts(&mesh), (3, 3), "a single point can't form a stroke");
    }

    #[test]
    fn open_stroke_vertex_and_index_counts() {
        let pts = vec![(Vec2::new(0.0, 0.0), Color::WHITE), (Vec2::new(10.0, 0.0), Color::WHITE)];
        let mut mesh = empty_stroke_mesh();
        apply_stroke(&mut mesh, &pts, 2.0, STROKE_FEATHER, false);
        // 4 lanes per point; 3 quads (2 tris each) per segment; 1 segment open.
        assert_eq!(counts(&mesh), (2 * 4, 1 * 3 * 6));
    }

    #[test]
    fn closed_stroke_adds_a_wrap_segment() {
        let pts = vec![
            (Vec2::new(0.0, 0.0), Color::WHITE),
            (Vec2::new(10.0, 0.0), Color::WHITE),
            (Vec2::new(5.0, 8.0), Color::WHITE),
        ];
        let mut mesh = empty_stroke_mesh();
        apply_stroke(&mut mesh, &pts, 2.0, STROKE_FEATHER, true);
        // Closed → segs == n (the last point wraps to the first).
        assert_eq!(counts(&mesh), (3 * 4, 3 * 3 * 6));
    }

    #[test]
    fn rounded_rect_has_center_boundary_and_feather_rings() {
        let mut mesh = empty_stroke_mesh();
        apply_rounded_rect(&mut mesh, Vec2::new(20.0, 10.0), 4.0, STROKE_FEATHER, Color::WHITE);
        let nb = 4 * (CORNER_SEGS + 1);
        // center + boundary ring + feather ring.
        assert_eq!(mesh.count_vertices(), 1 + nb * 2);
        assert_eq!(mesh.indices().unwrap().len(), nb * 9);
    }

    #[test]
    fn tapered_stroke_builds_four_lanes_per_point() {
        let pts = vec![
            (Vec2::new(0.0, 0.0), Color::WHITE),
            (Vec2::new(10.0, 0.0), Color::WHITE),
            (Vec2::new(20.0, 0.0), Color::WHITE),
        ];
        let mut mesh = empty_stroke_mesh();
        apply_stroke_tapered(&mut mesh, &pts, 0.0, 6.0, STROKE_FEATHER, false);
        assert_eq!(mesh.count_vertices(), 3 * 4);
    }
}
