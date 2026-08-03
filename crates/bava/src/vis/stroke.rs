// SPDX-License-Identifier: MIT OR Apache-2.0
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

/// Accumulates many shapes into one triangle list, so a whole pool of items —
/// every spectrum bar, every ball trail — becomes a **single** `Mesh`.
///
/// This is a deliberate performance shape, not just a convenience. Bevy's mesh
/// slab allocator frees and re-allocates *every modified mesh* each frame,
/// whether or not its size changed (`free_meshes` in
/// `bevy_render::mesh::allocator`, whose own comment reads "TODO: Consider
/// explicitly reusing allocations for changed meshes of the same size"). A pool
/// of N one-mesh-per-item entities rewritten per frame therefore costs N frees,
/// N allocations, N uploads and N draw calls every frame; batching makes all of
/// those 1, and drops the per-item `Transform`/`Visibility`/`Aabb` bookkeeping
/// with it. Measured at 256 bars: `allocate_and_free_meshes` alone went from
/// 1.14 ms/frame to ~0.
///
/// Positions are baked in world space (each shape is placed and rotated as it is
/// pushed), because the batch has a single identity `Transform`.
///
/// Reuse one instance across frames — [`clear`](Self::clear) keeps the
/// allocations, so a steady-state frame does no buffer growth at all.
#[derive(Default)]
pub(crate) struct MeshBatch {
    positions: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
    /// Reused boundary scratch (point, outward normal). Building it into an owned
    /// `Vec` per shape would be one heap allocation per bar per frame.
    boundary: Vec<(Vec2, Vec2)>,
}

impl MeshBatch {
    /// Drop the accumulated geometry, keeping the buffer capacity.
    pub(crate) fn clear(&mut self) {
        self.positions.clear();
        self.colors.clear();
        self.indices.clear();
    }

    /// Whether nothing has been pushed since the last [`clear`](Self::clear).
    pub(crate) fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Overwrite `mesh` with the accumulated geometry.
    ///
    /// An empty batch writes the degenerate triangle rather than a zero-vertex
    /// mesh, for the slab-allocator reason in [`empty_stroke_mesh`].
    pub(crate) fn write(&self, mesh: &mut Mesh) {
        if self.is_empty() {
            write_degenerate_tri(mesh);
            return;
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors.clone());
        mesh.insert_indices(Indices::U32(self.indices.clone()));
    }

    /// Append a feather-antialiased rounded rectangle centered at `center`,
    /// rotated `rotation` radians about it: `half` extents, corner `radius`
    /// (clamped to the shorter half), a `feather`-px alpha ramp at the edge,
    /// filled with `color`.
    pub(crate) fn push_rounded_rect(
        &mut self,
        center: Vec2,
        half: Vec2,
        rotation: f32,
        radius: f32,
        feather: f32,
        color: Color,
    ) {
        rounded_rect_boundary(&mut self.boundary, half, radius);
        self.push_convex_fan(center, rotation, feather, color);
    }

    /// Append the convex shape currently in [`Self::boundary`] (CCW points with
    /// outward normals, in local space) as a triangle fan plus a feather ring,
    /// placed at `center` and rotated by `rotation`.
    ///
    /// Vertex 0 is the center; then the full-alpha boundary ring; then the
    /// zero-alpha ring pushed `feather` px out along each boundary normal.
    fn push_convex_fan(&mut self, center: Vec2, rotation: f32, feather: f32, color: Color) {
        let nb = self.boundary.len() as u32;
        if nb < 3 {
            return;
        }
        let (sin, cos) = rotation.sin_cos();
        // Rotate about the shape's own center, then translate. Normals rotate
        // but are not translated.
        let turn = |v: Vec2| Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos);

        let lin = color.to_linear();
        let (r, g, b, a) = (lin.red, lin.green, lin.blue, lin.alpha);
        let base = self.positions.len() as u32;

        self.positions.push([center.x, center.y, 0.0]);
        self.colors.push([r, g, b, a]);
        for i in 0..self.boundary.len() {
            let q = center + turn(self.boundary[i].0);
            self.positions.push([q.x, q.y, 0.0]);
            self.colors.push([r, g, b, a]);
        }
        for i in 0..self.boundary.len() {
            let (p, n) = self.boundary[i];
            let q = center + turn(p) + turn(n) * feather;
            self.positions.push([q.x, q.y, 0.0]);
            self.colors.push([r, g, b, 0.0]);
        }

        let b0 = base + 1;
        let o0 = base + 1 + nb;
        for i in 0..nb {
            let i1 = (i + 1) % nb;
            let (bi, bi1) = (b0 + i, b0 + i1);
            let (oi, oi1) = (o0 + i, o0 + i1);
            self.indices.extend_from_slice(&[base, bi, bi1]);
            self.indices.extend_from_slice(&[bi, bi1, oi1, bi, oi1, oi]);
        }
    }

    /// Append a feathered stroke through `pts`, tapering the core half-width
    /// from `hw_start` to `hw_end`. Same geometry as [`apply_stroke_tapered`].
    pub(crate) fn push_stroke(
        &mut self,
        pts: &[(Vec2, Color)],
        hw_start: f32,
        hw_end: f32,
        feather: f32,
        closed: bool,
    ) {
        let n = pts.len();
        if n < 2 {
            return; // can't form a stroke; contribute nothing to the batch
        }
        let base = self.positions.len() as u32;
        stroke_vertices(pts, hw_start, hw_end, feather, closed, |pos, color| {
            self.positions.push(pos);
            self.colors.push(color);
        });
        stroke_indices(n, closed, |i| self.indices.push(base + i));
    }
}

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


/// Fill `out` with the boundary of a rounded rect (CCW, centered on the origin)
/// paired with outward normals, walking the four corner arcs. The straight edges
/// fall out of connecting consecutive arc endpoints.
fn rounded_rect_boundary(out: &mut Vec<(Vec2, Vec2)>, half: Vec2, radius: f32) {
    out.clear();
    let hx = half.x.max(0.01);
    let hy = half.y.max(0.01);
    let r = radius.clamp(0.0, hx.min(hy));

    let centers = [
        (Vec2::new(hx - r, -(hy - r)), -FRAC_PI_2), // right-bottom: -90°..0°
        (Vec2::new(hx - r, hy - r), 0.0),           // right-top:     0°..90°
        (Vec2::new(-(hx - r), hy - r), FRAC_PI_2),  // left-top:     90°..180°
        (Vec2::new(-(hx - r), -(hy - r)), PI),      // left-bottom: 180°..270°
    ];

    for (center, a0) in centers {
        for s in 0..=CORNER_SEGS {
            let ang = a0 + (s as f32 / CORNER_SEGS as f32) * FRAC_PI_2;
            let dir = Vec2::new(ang.cos(), ang.sin());
            out.push((center + dir * r, dir));
        }
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
    if pts.len() < 2 {
        // <2 points can't form a stroke; emit an invisible degenerate triangle
        // rather than a zero-vertex mesh (which the slab allocator rejects).
        write_degenerate_tri(mesh);
        return;
    }
    let mut batch = MeshBatch::default();
    batch.push_stroke(pts, hw_start, hw_end, feather, closed);
    batch.write(mesh);
}

/// Emit the four lanes per centerline point that make up a tapered, feathered
/// stroke: outer+ (alpha 0), core+ (1), core- (1), outer- (0), each offset along
/// the point's normal. Shared by the single-mesh and batched paths so they can't
/// drift apart.
fn stroke_vertices(
    pts: &[(Vec2, Color)],
    hw_start: f32,
    hw_end: f32,
    feather: f32,
    closed: bool,
    mut emit: impl FnMut([f32; 3], [f32; 4]),
) {
    let n = pts.len();
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
        let lanes = [(hw + feather, 0.0), (hw, 1.0), (-hw, 1.0), (-(hw + feather), 0.0)];

        let lin = pts[i].1.to_linear();
        for (off, edge) in lanes {
            let q = p + nrm * off;
            emit([q.x, q.y, 0.0], [lin.red, lin.green, lin.blue, lin.alpha * edge]);
        }
    }
}

/// Emit the stroke's indices (relative to its first vertex): three quads — left
/// feather, core, right feather — per segment.
fn stroke_indices(n: usize, closed: bool, mut emit: impl FnMut(u32)) {
    let segs = if closed { n } else { n - 1 };
    for s in 0..segs {
        let i0 = (s * 4) as u32;
        let i1 = (((s + 1) % n) * 4) as u32;
        for lane in 0..3u32 {
            let (a0, a1, b0, b1) = (i0 + lane, i0 + lane + 1, i1 + lane, i1 + lane + 1);
            for i in [a0, a1, b1, a0, b1, b0] {
                emit(i);
            }
        }
    }
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
        let mut batch = MeshBatch::default();
        batch.push_rounded_rect(
            Vec2::ZERO,
            Vec2::new(20.0, 10.0),
            0.0,
            4.0,
            STROKE_FEATHER,
            Color::WHITE,
        );
        batch.write(&mut mesh);
        let nb = 4 * (CORNER_SEGS + 1);
        // center + boundary ring + feather ring.
        assert_eq!(mesh.count_vertices(), 1 + nb * 2);
        assert_eq!(mesh.indices().unwrap().len(), nb * 9);
    }

    #[test]
    fn batching_n_rects_concatenates_geometry_and_offsets_indices() {
        // The whole point of the batch: N shapes land in ONE mesh, with each
        // shape's indices rebased onto its own vertices.
        let per_rect_verts = 1 + 4 * (CORNER_SEGS + 1) * 2;
        let per_rect_indices = 4 * (CORNER_SEGS + 1) * 9;
        let mut batch = MeshBatch::default();
        for i in 0..5 {
            batch.push_rounded_rect(
                Vec2::new(i as f32 * 50.0, 0.0),
                Vec2::new(10.0, 20.0),
                0.0,
                2.0,
                STROKE_FEATHER,
                Color::WHITE,
            );
        }
        let mut mesh = empty_stroke_mesh();
        batch.write(&mut mesh);
        assert_eq!(mesh.count_vertices(), 5 * per_rect_verts);
        assert_eq!(mesh.indices().unwrap().len(), 5 * per_rect_indices);
        // Every index must address a real vertex — the rebasing is the part that
        // silently corrupts geometry if it regresses.
        let max = match mesh.indices().unwrap() {
            Indices::U32(v) => *v.iter().max().unwrap() as usize,
            Indices::U16(v) => *v.iter().max().unwrap() as usize,
        };
        assert_eq!(max, 5 * per_rect_verts - 1);
    }

    #[test]
    fn rect_rotation_is_baked_into_vertex_positions() {
        // Batched shapes carry no per-item Transform, so rotation has to land in
        // the vertices themselves.
        let mut flat = MeshBatch::default();
        flat.push_rounded_rect(Vec2::ZERO, Vec2::new(30.0, 5.0), 0.0, 0.0, 0.0, Color::WHITE);
        let mut turned = MeshBatch::default();
        turned.push_rounded_rect(
            Vec2::ZERO,
            Vec2::new(30.0, 5.0),
            std::f32::consts::FRAC_PI_2,
            0.0,
            0.0,
            Color::WHITE,
        );
        let extent = |b: &MeshBatch| {
            b.positions.iter().fold((0.0f32, 0.0f32), |(x, y), p| {
                (x.max(p[0].abs()), y.max(p[1].abs()))
            })
        };
        let (fx, fy) = extent(&flat);
        let (tx, ty) = extent(&turned);
        // A quarter turn swaps the long and short axes.
        assert!((fx - ty).abs() < 1e-3, "{fx} vs {ty}");
        assert!((fy - tx).abs() < 1e-3, "{fy} vs {tx}");
    }

    #[test]
    fn clear_keeps_capacity_so_steady_state_frames_do_not_grow_buffers() {
        let mut batch = MeshBatch::default();
        for _ in 0..8 {
            batch.push_rounded_rect(Vec2::ZERO, Vec2::splat(4.0), 0.0, 1.0, 1.0, Color::WHITE);
        }
        let cap = batch.positions.capacity();
        batch.clear();
        assert!(batch.is_empty());
        assert_eq!(batch.positions.capacity(), cap);
        // An empty batch still writes the degenerate triangle, never 0 vertices.
        let mut mesh = empty_stroke_mesh();
        batch.write(&mut mesh);
        assert_eq!(mesh.count_vertices(), 3);
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
