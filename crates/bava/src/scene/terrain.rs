// SPDX-License-Identifier: MIT OR Apache-2.0
//! Spectrum-driven voxel terrain (`[terrain]` in a 3D scene).
//!
//! A `size × size` field of block columns whose heights follow the spectrum —
//! bass in the middle by default, or a scrolling spectrogram — meshed the way a
//! block game meshes a chunk: one textured quad per *visible* block face, with
//! every face that touches a neighbouring column culled. Heights are whole
//! blocks, so the landscape moves in crisp block steps.
//!
//! The geometry changes whenever a column steps up or down, which is most
//! frames with music playing, so it is rebuilt from scratch into two reused
//! meshes (the precondition for batching — see [`MeshBatch`](crate::vis::stroke::MeshBatch)):
//! ordinary blocks, and glowing ores whose emissive strength pulses on the
//! beat. Frames where no column changed skip the rebuild entirely.

use std::collections::VecDeque;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::scene::def::{TerrainDef, TerrainMapping};

/// Which face of a block a quad is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Top,
    Bottom,
    PosX,
    NegX,
    PosZ,
    NegZ,
}

impl Face {
    pub const SIDES: [Face; 4] = [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ];
    pub const ALL: [Face; 6] = [
        Face::Top,
        Face::Bottom,
        Face::PosX,
        Face::NegX,
        Face::PosZ,
        Face::NegZ,
    ];

    fn normal(self) -> [f32; 3] {
        match self {
            Face::Top => [0.0, 1.0, 0.0],
            Face::Bottom => [0.0, -1.0, 0.0],
            Face::PosX => [1.0, 0.0, 0.0],
            Face::NegX => [-1.0, 0.0, 0.0],
            Face::PosZ => [0.0, 0.0, 1.0],
            Face::NegZ => [0.0, 0.0, -1.0],
        }
    }

    /// Grid step toward the neighbour this face looks at.
    fn step(self) -> (i32, i32) {
        match self {
            Face::PosX => (1, 0),
            Face::NegX => (-1, 0),
            Face::PosZ => (0, 1),
            Face::NegZ => (0, -1),
            Face::Top | Face::Bottom => (0, 0),
        }
    }
}

/// Positions / normals / UVs / indices for one mesh, reused across frames.
#[derive(Default, Clone, Debug)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn clear(&mut self) {
        self.positions.clear();
        self.normals.clear();
        self.uvs.clear();
        self.indices.clear();
    }

    /// Append one face of the axis-aligned box `min..max`, textured with `uv`
    /// (`[u0, v0, u1, v1]`, v0 at the top of the image). Corners go
    /// counter-clockwise seen from outside, so back-face culling keeps them.
    pub fn push_face(&mut self, min: Vec3, max: Vec3, face: Face, uv: [f32; 4]) {
        let (x0, y0, z0) = (min.x, min.y, min.z);
        let (x1, y1, z1) = (max.x, max.y, max.z);
        let corners: [[f32; 3]; 4] = match face {
            Face::Top => [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
            Face::Bottom => [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
            Face::PosX => [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
            Face::NegX => [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
            Face::PosZ => [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
            Face::NegZ => [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
        };
        let [u0, v0, u1, v1] = uv;
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&corners);
        self.normals.extend_from_slice(&[face.normal(); 4]);
        self.uvs
            .extend_from_slice(&[[u0, v1], [u1, v1], [u1, v0], [u0, v0]]);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Overwrite `mesh`. An empty batch writes one degenerate triangle instead
    /// of a zero-vertex mesh (which Bevy's mesh allocator logs errors for).
    pub fn write(&self, mesh: &mut Mesh) {
        if self.positions.is_empty() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3]);
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3]);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3]);
            mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
            return;
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs.clone());
        mesh.insert_indices(Indices::U32(self.indices.clone()));
    }

    /// A fresh mesh holding this data.
    pub fn to_mesh(&self) -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        self.write(&mut mesh);
        mesh
    }
}

/// UV rect of atlas tile `index` in a `columns × rows` grid, inset by a sliver
/// so nearest-neighbour sampling never bleeds in the neighbouring tile.
pub fn tile_uv(index: u32, columns: u32, rows: u32) -> [f32; 4] {
    let columns = columns.max(1);
    let rows = rows.max(1);
    let (cx, cy) = ((index % columns) as f32, (index / columns) as f32);
    let (w, h) = (1.0 / columns as f32, 1.0 / rows as f32);
    let inset = 0.002;
    [
        cx * w + inset * w,
        cy * h + inset * h,
        (cx + 1.0) * w - inset * w,
        (cy + 1.0) * h - inset * h,
    ]
}

/// A unit cube (centered, side 1) with every face showing atlas tile `index`.
pub fn atlas_cube(index: u32, columns: u32, rows: u32) -> Mesh {
    let uv = tile_uv(index, columns, rows);
    let mut data = MeshData::default();
    for face in Face::ALL {
        data.push_face(Vec3::splat(-0.5), Vec3::splat(0.5), face, uv);
    }
    data.to_mesh()
}

/// Tile indices resolved from the terrain's names.
#[derive(Clone, Debug)]
struct Tiles {
    top: u32,
    side: u32,
    under: u32,
    deep: u32,
    peak: u32,
    shore: u32,
    ores: Vec<u32>,
}

impl Tiles {
    fn resolve(def: &TerrainDef) -> Self {
        let get = |name: &str| def.tiles.get(name).copied().unwrap_or(0);
        Self {
            top: get(&def.top),
            side: get(&def.side),
            under: get(&def.under),
            deep: get(&def.deep),
            peak: get(&def.peak),
            shore: get(&def.shore),
            ores: def.ores.iter().map(|n| get(n)).collect(),
        }
    }
}

/// Deterministic per-position hash in 0..1.
fn hash3(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ (z as u32).wrapping_mul(0xcb1a_b31f)
        ^ seed.wrapping_mul(0x2545_f491);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 0x0100_0000 as f32
}

/// Smooth static height noise in -1..1 for column (i, j).
fn column_noise(i: i32, j: i32, seed: u32) -> f32 {
    // Two octaves of bilinearly interpolated lattice noise.
    let lattice = |x: f32, z: f32, s: u32| {
        let (xi, zi) = (x.floor() as i32, z.floor() as i32);
        let (fx, fz) = (x - xi as f32, z - zi as f32);
        let (sx, sz) = (fx * fx * (3.0 - 2.0 * fx), fz * fz * (3.0 - 2.0 * fz));
        let a = hash3(xi, 0, zi, s);
        let b = hash3(xi + 1, 0, zi, s);
        let c = hash3(xi, 0, zi + 1, s);
        let d = hash3(xi + 1, 0, zi + 1, s);
        (a + (b - a) * sx) + ((c + (d - c) * sx) - (a + (b - a) * sx)) * sz
    };
    let n = 0.65 * lattice(i as f32 / 5.0, j as f32 / 5.0, seed)
        + 0.35 * lattice(i as f32 / 2.3, j as f32 / 2.3, seed ^ 0x9e37);
    n * 2.0 - 1.0
}

/// Smoothstep-interpolated level at `t` (0..1) across `values`.
fn sample(values: &[f32], t: f32) -> f32 {
    match values.len() {
        0 => 0.0,
        1 => values[0],
        n => {
            let x = t.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (x.floor() as usize).min(n - 2);
            let f = x - i as f32;
            let s = f * f * (3.0 - 2.0 * f);
            values[i] + (values[i + 1] - values[i]) * s
        }
    }
}

/// Live terrain state: the smoothed heights, the waterfall history, and the
/// last integer heights meshed.
#[derive(Component, Clone, Debug)]
pub struct Terrain {
    pub def: TerrainDef,
    tiles: Tiles,
    /// Smoothed height per column, in blocks.
    display: Vec<f32>,
    /// Integer heights of the last mesh.
    meshed: Vec<u32>,
    noise: Vec<f32>,
    history: VecDeque<Vec<f32>>,
    scroll: f32,
    pub solid_mesh: Handle<Mesh>,
    pub glow_mesh: Handle<Mesh>,
    pub glow_material: Handle<StandardMaterial>,
    solid: MeshData,
    glow: MeshData,
}

/// Waterfall rows added per second.
const WATERFALL_ROWS_PER_SEC: f32 = 14.0;

impl Terrain {
    pub fn new(
        def: TerrainDef,
        solid_mesh: Handle<Mesh>,
        glow_mesh: Handle<Mesh>,
        glow_material: Handle<StandardMaterial>,
    ) -> Self {
        let n = def.size as usize;
        let seed = def.seed;
        let noise = (0..n * n)
            .map(|k| column_noise((k % n) as i32, (k / n) as i32, seed))
            .collect();
        Self {
            tiles: Tiles::resolve(&def),
            display: vec![def.base_height as f32; n * n],
            meshed: Vec::new(),
            noise,
            history: VecDeque::new(),
            scroll: 0.0,
            solid_mesh,
            glow_mesh,
            glow_material,
            solid: MeshData::default(),
            glow: MeshData::default(),
            def,
        }
    }

    /// Column height targets (blocks, fractional) for the current spectrum.
    fn targets(&mut self, bars: &[f32], dt: f32) -> Vec<f32> {
        let n = self.def.size as usize;
        let span = self.def.max_height.saturating_sub(self.def.base_height) as f32;
        let base = self.def.base_height as f32;
        let mut out = vec![0.0; n * n];
        match self.def.mapping {
            TerrainMapping::Radial | TerrainMapping::RadialInverted => {
                let c = (n as f32 - 1.0) * 0.5;
                for j in 0..n {
                    for i in 0..n {
                        let d = ((i as f32 - c).powi(2) + (j as f32 - c).powi(2)).sqrt();
                        let mut t = (d / (c + 0.5).max(0.5)).min(1.0);
                        if self.def.mapping == TerrainMapping::RadialInverted {
                            t = 1.0 - t;
                        }
                        out[j * n + i] = base + sample(bars, t).clamp(0.0, 1.5) * span;
                    }
                }
            }
            TerrainMapping::Waterfall => {
                self.scroll += dt * WATERFALL_ROWS_PER_SEC;
                let row: Vec<f32> = (0..n)
                    .map(|i| sample(bars, i as f32 / (n.max(2) - 1) as f32))
                    .collect();
                if self.history.is_empty() {
                    self.history.push_front(row.clone());
                }
                while self.scroll >= 1.0 {
                    self.scroll -= 1.0;
                    self.history.push_front(row.clone());
                }
                // The newest row always shows the live spectrum.
                self.history[0] = row;
                self.history.truncate(n);
                for j in 0..n {
                    // The front row (largest z, nearest a default camera) is
                    // the newest.
                    let age = n - 1 - j;
                    let level = self
                        .history
                        .get(age)
                        .map(|r| r[..].to_vec())
                        .unwrap_or_else(|| vec![0.0; n]);
                    for i in 0..n {
                        out[j * n + i] = base + level[i].clamp(0.0, 1.5) * span;
                    }
                }
            }
        }
        let falloff = self.def.edge_falloff.clamp(0.0, 1.0);
        let c = (n as f32 - 1.0) * 0.5;
        for (k, (h, noise)) in out.iter_mut().zip(&self.noise).enumerate() {
            if falloff > 0.0 {
                let (i, j) = ((k % n) as f32, (k / n) as f32);
                let d = ((i - c).powi(2) + (j - c).powi(2)).sqrt() / (c + 0.5).max(0.5);
                let keep = 1.0 - falloff * d.min(1.0).powf(1.5);
                *h = base + (*h - base) * keep;
            }
            *h += noise * self.def.roughness;
        }
        out
    }

    /// Advance the heights one frame. Returns true when the integer heights
    /// changed (the mesh needs rebuilding).
    pub fn step(&mut self, bars: &[f32], dt: f32) -> bool {
        let targets = self.targets(bars, dt);
        let release = if self.def.fall <= 1e-4 {
            0.0
        } else if dt > 0.0 {
            (-dt / self.def.fall).exp()
        } else {
            1.0
        };
        for (shown, target) in self.display.iter_mut().zip(targets) {
            *shown = if target >= *shown {
                target
            } else {
                target + (*shown - target) * release
            };
        }
        let heights: Vec<u32> = self
            .display
            .iter()
            .map(|h| h.round().clamp(1.0, 255.0) as u32)
            .collect();
        if heights == self.meshed {
            return false;
        }
        self.meshed = heights;
        self.rebuild();
        true
    }

    /// Integer height of column (i, j), 0 outside the grid.
    fn height(&self, i: i32, j: i32) -> u32 {
        let n = self.def.size as i32;
        if i < 0 || j < 0 || i >= n || j >= n {
            return 0;
        }
        self.meshed.get((j * n + i) as usize).copied().unwrap_or(0)
    }

    /// Re-mesh every column from [`Self::meshed`].
    fn rebuild(&mut self) {
        let mut solid = std::mem::take(&mut self.solid);
        let mut glow = std::mem::take(&mut self.glow);
        solid.clear();
        glow.clear();
        let def = &self.def;
        let n = def.size as i32;
        let b = def.block;
        let half = n as f32 * b * 0.5;
        let (cols, rows) = (def.atlas_columns, def.atlas_rows);
        let uv = |tile: u32| tile_uv(tile, cols, rows);

        for j in 0..n {
            for i in 0..n {
                let h = self.height(i, j);
                if h == 0 {
                    continue;
                }
                let x0 = i as f32 * b - half;
                let z0 = j as f32 * b - half;
                let peak = def.peak_height > 0 && h > def.peak_height;
                let shore = def.water_level > 0 && h <= def.water_level;
                let (top_tile, cap_side) = if peak {
                    (self.tiles.peak, self.tiles.peak)
                } else if shore {
                    (self.tiles.shore, self.tiles.shore)
                } else {
                    (self.tiles.top, self.tiles.side)
                };

                let top = h as f32 * b;
                solid.push_face(
                    Vec3::new(x0, top - b, z0),
                    Vec3::new(x0 + b, top, z0 + b),
                    Face::Top,
                    uv(top_tile),
                );

                for face in Face::SIDES {
                    let (di, dj) = face.step();
                    let neighbour = self.height(i + di, j + dj);
                    for y in neighbour..h {
                        let depth = h - 1 - y;
                        let (tile, ore) = if depth == 0 {
                            (cap_side, false)
                        } else if depth <= def.under_depth {
                            (self.tiles.under, false)
                        } else if !self.tiles.ores.is_empty()
                            && hash3(i, y as i32, j, def.seed) < def.ore_chance
                        {
                            let pick = hash3(j, i, y as i32, def.seed ^ 0x51ed);
                            let k = ((pick * self.tiles.ores.len() as f32) as usize)
                                .min(self.tiles.ores.len() - 1);
                            (self.tiles.ores[k], true)
                        } else {
                            (self.tiles.deep, false)
                        };
                        let target = if ore { &mut glow } else { &mut solid };
                        target.push_face(
                            Vec3::new(x0, y as f32 * b, z0),
                            Vec3::new(x0 + b, (y + 1) as f32 * b, z0 + b),
                            face,
                            uv(tile),
                        );
                    }
                }
            }
        }
        self.solid = solid;
        self.glow = glow;
    }

    /// Write the current geometry into the two meshes.
    pub fn write(&self, meshes: &mut Assets<Mesh>) {
        if let Some(mut m) = meshes.get_mut(&self.solid_mesh) {
            self.solid.write(&mut m);
        }
        if let Some(mut m) = meshes.get_mut(&self.glow_mesh) {
            self.glow.write(&mut m);
        }
    }

    #[cfg(test)]
    fn faces(&self) -> (usize, usize) {
        (self.solid.indices.len() / 6, self.glow.indices.len() / 6)
    }
}

/// Step every terrain, re-mesh the ones whose heights moved, and pulse the ore
/// glow on the beat.
pub fn update_terrain(
    time: Res<Time>,
    cava: Res<crate::cava::Cava>,
    vis: Res<crate::vis::VisSettings>,
    features: Res<crate::vis::features::AudioFeatures>,
    mut terrains: Query<&mut Terrain>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut bars = cava.mono();
    crate::vis::spread_monstercat(&mut bars, vis.monstercat);
    for mut terrain in &mut terrains {
        crate::profile_scope!("terrain_mesh");
        if terrain.step(&bars, time.delta_secs()) {
            terrain.write(&mut meshes);
        }
        let glow = terrain.def.ore_glow + terrain.def.ore_pulse * features.beat_pulse;
        if let Some(mut m) = materials.get_mut(&terrain.glow_material) {
            m.emissive = LinearRgba::rgb(glow, glow, glow);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(size: u32) -> TerrainDef {
        TerrainDef {
            atlas: "a.png".into(),
            size,
            base_height: 1,
            max_height: 5,
            roughness: 0.0,
            fall: 0.0,
            ..TerrainDef::default()
        }
    }

    fn terrain(d: TerrainDef) -> Terrain {
        Terrain::new(d, Handle::default(), Handle::default(), Handle::default())
    }

    #[test]
    fn faces_wind_counter_clockwise_from_outside() {
        for face in Face::ALL {
            let mut m = MeshData::default();
            m.push_face(Vec3::ZERO, Vec3::ONE, face, [0.0, 0.0, 1.0, 1.0]);
            let p = |k: usize| Vec3::from(m.positions[m.indices[k] as usize]);
            let n = (p(1) - p(0)).cross(p(2) - p(0)).normalize();
            assert!(
                (n - Vec3::from(face.normal())).length() < 1e-5,
                "{face:?}: winding normal {n} vs declared {:?}",
                face.normal()
            );
        }
    }

    #[test]
    fn tile_uvs_stay_inside_their_cell() {
        let [u0, v0, u1, v1] = tile_uv(9, 8, 8); // row 1, column 1
        assert!(u0 > 0.125 && u1 < 0.25, "{u0}..{u1}");
        assert!(v0 > 0.125 && v1 < 0.25, "{v0}..{v1}");
    }

    #[test]
    fn a_single_column_has_a_top_and_four_sides_per_block() {
        let mut t = terrain(def(1));
        // Silence: base height 1 → one block: 1 top + 4 sides.
        assert!(t.step(&[0.0; 8], 1.0 / 60.0));
        assert_eq!(t.faces(), (5, 0));
        // Full scale: 5 blocks → 1 top + 4 × 5 sides.
        assert!(t.step(&[1.0; 8], 1.0 / 60.0));
        assert_eq!(t.faces(), (21, 0));
        // Unchanged spectrum → no rebuild.
        assert!(!t.step(&[1.0; 8], 1.0 / 60.0));
    }

    #[test]
    fn faces_between_equal_neighbours_are_culled() {
        let mut t = terrain(def(3));
        t.step(&[0.0; 8], 1.0 / 60.0); // flat at height 1
        // 9 tops + only the 12 outward-facing sides of the 3×3 slab.
        assert_eq!(t.faces(), (9 + 12, 0));
    }

    #[test]
    fn radial_mapping_puts_the_bass_in_the_middle() {
        let mut t = terrain(def(9));
        // Bass loud, treble silent.
        let bars = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        t.step(&bars, 1.0 / 60.0);
        assert!(
            t.height(4, 4) > t.height(0, 0),
            "center {} corner {}",
            t.height(4, 4),
            t.height(0, 0)
        );
        let mut inv = terrain(TerrainDef {
            mapping: TerrainMapping::RadialInverted,
            ..def(9)
        });
        inv.step(&bars, 1.0 / 60.0);
        assert!(inv.height(0, 0) > inv.height(4, 4));
    }

    #[test]
    fn edge_falloff_lowers_the_rim() {
        let mut flat = terrain(def(9));
        let mut island = terrain(TerrainDef {
            edge_falloff: 1.0,
            ..def(9)
        });
        flat.step(&[1.0; 8], 1.0 / 60.0);
        island.step(&[1.0; 8], 1.0 / 60.0);
        assert_eq!(flat.height(0, 4), 5);
        assert_eq!(island.height(4, 4), 5, "the middle keeps its height");
        assert!(island.height(0, 4) < 3, "the rim drops to the base");
    }

    #[test]
    fn falling_columns_release_smoothly() {
        let mut t = terrain(TerrainDef {
            fall: 0.3,
            ..def(1)
        });
        t.step(&[1.0; 4], 1.0 / 60.0);
        assert_eq!(t.height(0, 0), 5);
        t.step(&[0.0; 4], 1.0 / 60.0);
        assert!(t.height(0, 0) > 1, "does not snap down in one frame");
        for _ in 0..240 {
            t.step(&[0.0; 4], 1.0 / 60.0);
        }
        assert_eq!(t.height(0, 0), 1);
    }

    #[test]
    fn waterfall_scrolls_rows_back_in_time() {
        let mut t = terrain(TerrainDef {
            mapping: TerrainMapping::Waterfall,
            ..def(4)
        });
        // A loud frame, then silence for a while: the loud row moves back.
        t.step(&[1.0; 4], 0.1);
        let front = 3; // newest row
        assert_eq!(t.height(1, front), 5);
        for _ in 0..3 {
            t.step(&[0.0; 4], 1.0 / WATERFALL_ROWS_PER_SEC);
        }
        assert_eq!(t.height(1, front), 1, "front row shows the live silence");
        assert!(
            t.height(1, 0) > 1 || t.height(1, 1) > 1,
            "the loud row scrolled back"
        );
    }

    #[test]
    fn ores_go_to_the_glow_mesh() {
        let mut d = def(4);
        d.max_height = 12;
        d.under_depth = 0;
        d.ore_chance = 1.0;
        d.ores = vec!["diamond_ore".into()];
        d.tiles.insert("diamond_ore".into(), 16);
        let mut t = terrain(d);
        t.step(&[1.0; 4], 1.0 / 60.0);
        let (solid, glow) = t.faces();
        assert!(glow > 0, "deep blocks became ores");
        assert!(solid > 0, "tops and caps stay solid");
    }

    #[test]
    fn noise_is_deterministic_and_bounded() {
        for i in 0..20 {
            for j in 0..20 {
                let n = column_noise(i, j, 7);
                assert!((-1.0..=1.0).contains(&n));
                assert_eq!(n, column_noise(i, j, 7));
            }
        }
    }
}
