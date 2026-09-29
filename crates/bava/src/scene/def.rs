// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `scene.toml` file model.
//!
//! A scene is a directory: a `scene.toml` plus whatever it references —
//! shaders, textures, glTF models, sounds — by paths relative to that
//! directory. Everything here is plain serde data with `#[serde(default)]`, so a
//! scene only states what it changes; [`SceneDef::validate`] catches the
//! mistakes serde can't (dangling names, parent cycles, empty rings) with a
//! message that says where.
//!
//! See `docs/SCENES.md` for the authoring guide.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level `scene.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SceneDef {
    pub scene: SceneMeta,
    /// Overrides merged over the regular config while the scene is active
    /// (same tables and keys as `config.toml`: `[config.vis]`,
    /// `[config.physics]`, `[config.fx]`, …).
    pub config: Option<toml::Table>,
    /// Named WGSL shaders: `name = "shaders/x.wgsl"`.
    pub shaders: BTreeMap<String, String>,
    /// Named sounds.
    pub sounds: BTreeMap<String, SoundDef>,
    /// Replacement shaders for the built-in 2D blob layers.
    pub blob: BlobDef,
    /// 3D camera.
    pub camera: CameraDef,
    /// 3D lighting / sky / fog.
    pub environment: EnvironmentDef,
    #[serde(rename = "light")]
    pub lights: Vec<LightDef>,
    #[serde(rename = "object")]
    pub objects: Vec<ObjectDef>,
    #[serde(rename = "ring")]
    pub rings: Vec<RingDef>,
    /// 3D voxel terrain driven by the spectrum.
    pub terrain: Option<TerrainDef>,
}

/// `[scene]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SceneMeta {
    pub name: String,
    pub description: String,
    /// `"2d"` or `"3d"`.
    pub dimension: Dimension,
    /// Keep drawing bava's own 2D visualizer (blob, bars, balls) over a 3D
    /// scene. Always true for 2D scenes.
    pub overlay_2d: bool,
    /// Background clear color (hex).
    pub clear_color: Option<String>,
    /// Show the now-playing text.
    pub hud: bool,
    /// 2D: author in a virtual canvas this many units across the window's
    /// *shorter* side, so the scene scales with the window (0 = raw pixels).
    pub canvas: f32,
}

impl Default for SceneMeta {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            dimension: Dimension::TwoD,
            overlay_2d: false,
            clear_color: None,
            hud: true,
            canvas: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Dimension {
    #[default]
    #[serde(rename = "2d")]
    TwoD,
    #[serde(rename = "3d")]
    ThreeD,
}

/// `[sounds.<name>]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoundDef {
    pub path: String,
    /// Linear volume, clamped to 0..2.
    pub volume: f32,
    pub trigger: SoundTrigger,
    /// For `trigger = "beat"`: play on every Nth beat.
    pub every: u32,
    /// For `trigger = "key"`: the key name (same names as `[gui] toggle_key`).
    pub key: Option<String>,
    /// Minimum seconds between two plays.
    pub min_interval: f32,
    /// Random playback-speed variation (± fraction, clamped to 0..0.9), so
    /// repeats don't sound mechanical.
    pub jitter: f32,
}

impl Default for SoundDef {
    fn default() -> Self {
        Self {
            path: String::new(),
            volume: 0.5,
            trigger: SoundTrigger::Click,
            every: 1,
            key: None,
            min_interval: 0.05,
            jitter: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundTrigger {
    /// Once, when the scene starts.
    Start,
    /// Continuously, looped, while the scene is active.
    Loop,
    /// On detected beats (see `every`).
    Beat,
    /// When a physics ball is spawned.
    Spawn,
    /// When a ball is struck hard.
    Impact,
    /// On a left click.
    #[default]
    Click,
    /// On a key press (see `key`).
    Key,
}

/// `[blob]` — swap the shaders of the built-in 2D layers. Each names an entry
/// of `[shaders]`; `*_params` fill that material's `params[1..]` (`params[0]`
/// is kept up to date by bava: see `docs/SCENES.md`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BlobDef {
    pub fill_shader: Option<String>,
    pub fill_params: Vec<[f32; 4]>,
    pub halo_shader: Option<String>,
    pub halo_params: Vec<[f32; 4]>,
    pub backdrop_shader: Option<String>,
    pub backdrop_params: Vec<[f32; 4]>,
}

/// `[camera]` (3D).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CameraDef {
    pub position: [f32; 3],
    pub look_at: [f32; 3],
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// Auto-orbit around `look_at`, radians per second (negative = clockwise).
    pub orbit: f32,
    /// Vertical bob on the bass, world units.
    pub bob: f32,
    /// Field-of-view punch on the beat, as a fraction.
    pub punch: f32,
}

impl Default for CameraDef {
    fn default() -> Self {
        Self {
            position: [0.0, 6.0, 18.0],
            look_at: [0.0, 0.0, 0.0],
            fov: 50.0,
            orbit: 0.0,
            bob: 0.0,
            punch: 0.03,
        }
    }
}

/// `[environment]` (3D).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnvironmentDef {
    pub ambient_color: String,
    pub ambient_brightness: f32,
    /// Distance fog: color, start, end (world units). No fog when `None`.
    pub fog_color: Option<String>,
    pub fog_start: f32,
    pub fog_end: f32,
    /// A sky dome drawn with this shader (a `[shaders]` name).
    pub sky_shader: Option<String>,
    pub sky_params: Vec<[f32; 4]>,
}

impl Default for EnvironmentDef {
    fn default() -> Self {
        Self {
            ambient_color: "#ffffff".into(),
            ambient_brightness: 200.0,
            fog_color: None,
            fog_start: 30.0,
            fog_end: 90.0,
            sky_shader: None,
            sky_params: Vec::new(),
        }
    }
}

/// `[[light]]` (3D).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LightDef {
    pub kind: LightKind,
    pub color: String,
    /// Lux for directional lights, lumens for point lights.
    pub intensity: f32,
    pub position: [f32; 3],
    pub look_at: [f32; 3],
    pub shadows: bool,
    /// Point-light range.
    pub range: f32,
    /// Intensity reactions (`property` is ignored; the light's intensity is
    /// scaled by `1 + amount · level`).
    pub react: Vec<ReactDef>,
}

impl Default for LightDef {
    fn default() -> Self {
        Self {
            kind: LightKind::Directional,
            color: "#ffffff".into(),
            intensity: 8000.0,
            position: [10.0, 20.0, 10.0],
            look_at: [0.0, 0.0, 0.0],
            shadows: false,
            range: 30.0,
            react: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightKind {
    #[default]
    Directional,
    Point,
}

/// `[[object]]` — one thing in the scene: a primitive mesh or a glTF model,
/// with a material, a rest transform and optional motion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObjectDef {
    /// Referenced by other objects' `orbit.parent`.
    pub name: Option<String>,
    /// A primitive: 2D `circle`, `rect`, `ring`; 3D `sphere`, `cube`,
    /// `plane`, `cylinder`, `torus`, `capsule`, `cone`.
    pub mesh: Option<String>,
    /// Primitive dimensions; meaning depends on `mesh` (see the guide).
    pub size: Vec<f32>,
    /// Where the primitive's origin sits: its center, or the middle of its
    /// bottom edge (so a `scale.y` reaction grows it upward only).
    pub anchor: Anchor,
    /// A glTF / GLB model (3D), spawned from its first scene.
    pub model: Option<String>,
    /// Loop this glTF animation clip (by index) on the model; it plays faster
    /// the louder the music is.
    pub animation: Option<usize>,
    pub material: MaterialDef,
    pub position: [f32; 3],
    /// Euler angles in degrees (X, Y, Z).
    pub rotation: [f32; 3],
    /// Uniform scale, or per-axis `[x, y, z]`.
    pub scale: ScaleDef,
    pub orbit: Option<OrbitDef>,
    pub spin: Option<SpinDef>,
    pub react: Vec<ReactDef>,
    /// 2D only: balls bounce off this object.
    pub collider: Option<ColliderDef>,
}

impl Default for ObjectDef {
    fn default() -> Self {
        Self {
            name: None,
            mesh: None,
            size: Vec::new(),
            anchor: Anchor::Center,
            model: None,
            animation: None,
            material: MaterialDef::default(),
            position: [0.0; 3],
            rotation: [0.0; 3],
            scale: ScaleDef::Uniform(1.0),
            orbit: None,
            spin: None,
            react: Vec::new(),
            collider: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    #[default]
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScaleDef {
    Uniform(f32),
    Axes([f32; 3]),
}

impl ScaleDef {
    pub fn to_array(self) -> [f32; 3] {
        match self {
            ScaleDef::Uniform(s) => [s; 3],
            ScaleDef::Axes(a) => a,
        }
    }
}

/// An object's material. With `shader` it is an effect material (the shader
/// gets the live audio uniform); otherwise a standard color / texture material
/// (lit PBR in 3D unless `unlit`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaterialDef {
    pub color: String,
    pub texture: Option<String>,
    /// Sample the texture with nearest-neighbour filtering (pixel art).
    pub pixelated: bool,
    /// Texture atlas tile for cubes: every face shows tile N (row-major) of an
    /// `atlas_columns × atlas_rows` grid.
    pub atlas_tile: Option<u32>,
    pub atlas_columns: u32,
    pub atlas_rows: u32,
    /// 3D: emissive color (hex) — bloom it with an HDR `emissive_strength`.
    pub emissive: Option<String>,
    pub emissive_strength: f32,
    /// 3D: use the texture itself as the emissive map.
    pub emissive_texture: bool,
    pub unlit: bool,
    pub roughness: f32,
    pub metallic: f32,
    /// 0..1 opacity.
    pub alpha: f32,
    /// A `[shaders]` name.
    pub shader: Option<String>,
    /// The shader's `params` (up to four vec4s).
    pub params: Vec<[f32; 4]>,
    pub blend: BlendDef,
    /// 3D effect material: draw both faces (sky domes, rings seen edge-on).
    pub double_sided: bool,
}

impl Default for MaterialDef {
    fn default() -> Self {
        Self {
            color: "#ffffff".into(),
            texture: None,
            pixelated: false,
            atlas_tile: None,
            atlas_columns: 8,
            atlas_rows: 8,
            emissive: None,
            emissive_strength: 1.0,
            emissive_texture: false,
            unlit: false,
            roughness: 0.8,
            metallic: 0.0,
            alpha: 1.0,
            shader: None,
            params: Vec::new(),
            blend: BlendDef::Alpha,
            double_sided: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendDef {
    #[default]
    Alpha,
    Additive,
    Opaque,
    /// Alpha-tested cutout (pixel-art glass, leaves).
    Mask,
}

/// `orbit = { ... }` — circle a point, or another object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrbitDef {
    /// Name of an object to orbit (its live position is the center).
    pub parent: Option<String>,
    pub center: [f32; 3],
    /// Radius, or `[rx, ry]` for an ellipse.
    pub radius: RadiusDef,
    /// Seconds per revolution; negative runs clockwise.
    pub period: f32,
    /// Starting angle as a fraction of a turn.
    pub phase: f32,
    /// 3D: tilt of the orbit plane (degrees about X, then Z). 2D orbits are in
    /// the screen plane.
    pub tilt: [f32; 2],
    /// Speed up the orbit with a band: angular speed × (1 + react · level).
    pub speed_band: Option<String>,
    pub speed_react: f32,
    /// 2D fake depth: while on the far half of the orbit (above its center)
    /// the object moves to this z — e.g. behind the blob at z = -5.
    pub behind_z: Option<f32>,
    /// 2D fake depth: shrink by up to this fraction at the far point of the
    /// orbit (and grow as much at the near point).
    pub perspective: f32,
    /// Draw the orbit path (2D): line width in px (0 = hidden).
    pub path_width: f32,
    /// Orbit path color (hex, alpha allowed).
    pub path_color: String,
}

impl Default for OrbitDef {
    fn default() -> Self {
        Self {
            parent: None,
            center: [0.0; 3],
            radius: RadiusDef::Circle(100.0),
            period: 10.0,
            phase: 0.0,
            tilt: [0.0; 2],
            speed_band: None,
            speed_react: 0.0,
            behind_z: None,
            perspective: 0.0,
            path_width: 0.0,
            path_color: "#ffffff22".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RadiusDef {
    Circle(f32),
    Ellipse([f32; 2]),
}

impl RadiusDef {
    pub fn axes(self) -> [f32; 2] {
        match self {
            RadiusDef::Circle(r) => [r, r],
            RadiusDef::Ellipse(e) => e,
        }
    }
}

/// `spin = { ... }` — constant rotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpinDef {
    /// Degrees per second.
    pub speed: f32,
    /// Rotation axis (3D; 2D always spins about Z).
    pub axis: [f32; 3],
}

impl Default for SpinDef {
    fn default() -> Self {
        Self {
            speed: 30.0,
            axis: [0.0, 1.0, 0.0],
        }
    }
}

/// `react = [{ ... }]` — bind a property to an audio band.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReactDef {
    /// `scale`, `scale.x|y|z`, `position.x|y|z`, `rotation.x|y|z`
    /// (degrees), `brightness`, or `param.N.x|y|z|w`.
    pub property: String,
    /// `bass`, `mid`, `treble`, `energy`, `beat`, `bar:N`, or (inside a
    /// `[[ring]]`) `bar` for the instance's own bar.
    pub band: String,
    pub amount: f32,
    /// Release time constant, seconds (attack is instant).
    pub smooth: f32,
}

impl Default for ReactDef {
    fn default() -> Self {
        Self {
            property: "scale".into(),
            band: "bass".into(),
            amount: 0.2,
            smooth: 0.1,
        }
    }
}

/// 2D collider for balls to bounce off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColliderDef {
    /// Circle radius in px (defaults to the mesh size).
    pub radius: Option<f32>,
    pub restitution: f32,
}

impl Default for ColliderDef {
    fn default() -> Self {
        Self {
            radius: None,
            restitution: 0.9,
        }
    }
}

/// `[[ring]]` — `count` copies of an object template laid around a circle.
/// Instance *i* can bind `band = "bar"` to its own spectrum bar, so a ring of
/// 48 cubes becomes a 48-bar radial spectrum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RingDef {
    pub count: u32,
    /// Radius, or `[rx, ry]` for an elliptical ring.
    pub radius: RadiusDef,
    pub center: [f32; 3],
    /// Face each instance outward (rotate by its angle about the ring axis).
    pub face_out: bool,
    /// 3D: the ring lies in the XZ plane (true) or faces the camera in XY.
    pub horizontal: bool,
    /// Rotate the whole ring, degrees per second.
    pub spin: f32,
    /// Starting angle as a fraction of a turn.
    pub phase: f32,
    /// Map instance *i* to spectrum bars across the whole ring, mirrored so
    /// both halves meet at the bass.
    pub mirror_bars: bool,
    /// Cycle instances through these atlas tiles (cubes with `atlas_tile`).
    pub tiles: Vec<u32>,
    /// 2D fake depth, as for orbits: z of instances on the far half.
    pub behind_z: Option<f32>,
    /// 2D fake depth: scale swing between the near and far points.
    pub perspective: f32,
    pub template: ObjectDef,
}

impl Default for RingDef {
    fn default() -> Self {
        Self {
            count: 12,
            radius: RadiusDef::Circle(100.0),
            center: [0.0; 3],
            face_out: true,
            horizontal: true,
            spin: 0.0,
            phase: 0.0,
            mirror_bars: true,
            tiles: Vec::new(),
            behind_z: None,
            perspective: 0.0,
            template: ObjectDef::default(),
        }
    }
}

/// `[terrain]` — a Minecraft-style voxel field whose column heights follow the
/// spectrum, meshed every frame with hidden faces culled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerrainDef {
    /// Texture atlas image, a grid of `atlas_columns × atlas_rows` tiles
    /// (each 1..=[`MAX_ATLAS_GRID`]).
    pub atlas: String,
    pub atlas_columns: u32,
    pub atlas_rows: u32,
    /// Tile name → atlas index.
    pub tiles: BTreeMap<String, u32>,
    /// Columns per side.
    pub size: u32,
    /// World size of one block (positive).
    pub block: f32,
    /// Resting height in blocks.
    pub base_height: u32,
    /// Height of a full-scale column in blocks.
    pub max_height: u32,
    /// How bars map onto the grid.
    pub mapping: TerrainMapping,
    /// Static per-column height noise, in blocks (makes it read as land).
    pub roughness: f32,
    /// Scale the spectrum's lift down toward the edge (0 = flat field, 1 =
    /// none at the rim), so the field reads as an island rather than a bowl.
    pub edge_falloff: f32,
    /// Release time constant for falling columns, seconds.
    pub fall: f32,
    /// Seed for the static noise and ore placement.
    pub seed: u32,
    /// Tile names, top layer first.
    pub top: String,
    pub side: String,
    pub under: String,
    pub under_depth: u32,
    pub deep: String,
    /// Above this height (blocks) the top block becomes `peak`.
    pub peak_height: u32,
    pub peak: String,
    /// Blocks at or below this height get `shore` tops (0 = none). No water
    /// is drawn: add a `plane` object at `water_level × block` for that.
    pub water_level: u32,
    pub shore: String,
    /// Unused; still accepted so older scenes that set it keep parsing.
    pub water: String,
    /// Chance a deep block is an ore, and which glowing tiles ores use.
    pub ore_chance: f32,
    pub ores: Vec<String>,
    /// Ores' emissive strength, and how much the beat adds to it.
    pub ore_glow: f32,
    pub ore_pulse: f32,
}

impl Default for TerrainDef {
    fn default() -> Self {
        Self {
            atlas: String::new(),
            atlas_columns: 8,
            atlas_rows: 8,
            tiles: BTreeMap::new(),
            size: 24,
            block: 1.0,
            base_height: 2,
            max_height: 12,
            mapping: TerrainMapping::Radial,
            roughness: 1.5,
            edge_falloff: 0.0,
            fall: 0.25,
            seed: 7,
            top: "grass_top".into(),
            side: "grass_side".into(),
            under: "dirt".into(),
            under_depth: 3,
            deep: "stone".into(),
            peak_height: 0,
            peak: "snow".into(),
            water_level: 0,
            shore: "sand".into(),
            water: "water".into(),
            ore_chance: 0.0,
            ores: Vec::new(),
            ore_glow: 2.0,
            ore_pulse: 4.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerrainMapping {
    /// Distance from the center picks the bar: bass in the middle.
    #[default]
    Radial,
    /// Bass on the rim, treble in the middle.
    RadialInverted,
    /// A scrolling spectrogram: X is frequency, Z is time.
    Waterfall,
}

/// Largest `[terrain] atlas_columns` / `atlas_rows`.
pub const MAX_ATLAS_GRID: u32 = 1024;

/// The named keys `[sounds.x] key` accepts besides letters, digits and F-keys
/// (a subset of `config::parse_key`'s names, listed in the error for a typo).
const NAMED_KEYS: &[&str] = &[
    "space",
    "tab",
    "enter",
    "escape",
    "insert",
    "backquote",
    "backslash",
    "minus",
    "equal",
    "comma",
    "period",
    "slash",
    "semicolon",
];

impl SceneDef {
    /// Parse a `scene.toml`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let def: SceneDef = toml::from_str(text).map_err(|e| e.to_string())?;
        def.validate()?;
        Ok(def)
    }

    /// Check the cross-references serde can't: every shader / sound / parent
    /// name used exists, parents are declared before children, rings aren't
    /// empty, terrain tiles resolve, key names are known, and every file path
    /// is one the asset server can load.
    pub fn validate(&self) -> Result<(), String> {
        let shader = |name: &Option<String>, what: &str| -> Result<(), String> {
            match name {
                Some(n) if !self.shaders.contains_key(n) => Err(format!(
                    "{what}: unknown shader {n:?} (declare it under [shaders])"
                )),
                _ => Ok(()),
            }
        };
        shader(&self.blob.fill_shader, "[blob] fill_shader")?;
        for (what, params) in [
            ("fill_params", &self.blob.fill_params),
            ("halo_params", &self.blob.halo_params),
            ("backdrop_params", &self.blob.backdrop_params),
        ] {
            if params.len() > 3 {
                return Err(format!(
                    "[blob] {what}: at most 3 vec4s (params[0] is filled by bava)"
                ));
            }
        }
        if self.environment.sky_params.len() > 4 {
            return Err("[environment] sky_params: at most 4 vec4s".into());
        }
        shader(&self.blob.halo_shader, "[blob] halo_shader")?;
        shader(&self.blob.backdrop_shader, "[blob] backdrop_shader")?;
        shader(&self.environment.sky_shader, "[environment] sky_shader")?;

        for (name, sound) in &self.sounds {
            if sound.path.trim().is_empty() {
                return Err(format!("[sounds.{name}]: missing path"));
            }
            if sound.trigger == SoundTrigger::Key {
                match sound.key.as_deref() {
                    None => {
                        return Err(format!(
                            "[sounds.{name}]: trigger = \"key\" needs key = \"...\""
                        ));
                    }
                    Some(key) if crate::config::parse_key(key).is_none() => {
                        return Err(format!(
                            "[sounds.{name}]: unknown key {key:?} (use a-z, 0-9, f1-f12, {})",
                            NAMED_KEYS.join(", ")
                        ));
                    }
                    Some(_) => {}
                }
            }
        }

        let mut seen: Vec<&str> = Vec::new();
        for (i, obj) in self.objects.iter().enumerate() {
            let label = obj
                .name
                .clone()
                .unwrap_or_else(|| format!("[[object]] #{}", i + 1));
            self.check_object(obj, &label)?;
            if let Some(parent) = obj.orbit.as_ref().and_then(|o| o.parent.as_deref())
                && !seen.contains(&parent)
            {
                return Err(format!(
                    "{label}: orbit parent {parent:?} must be an object declared earlier"
                ));
            }
            if let Some(name) = &obj.name {
                seen.push(name);
            }
        }
        for (i, ring) in self.rings.iter().enumerate() {
            let label = format!("[[ring]] #{}", i + 1);
            if ring.count == 0 {
                return Err(format!("{label}: count must be at least 1"));
            }
            if ring.count > 4096 {
                return Err(format!(
                    "{label}: count {} is over the 4096 limit",
                    ring.count
                ));
            }
            if ring.template.orbit.is_some() {
                return Err(format!("{label}: a ring template cannot orbit"));
            }
            self.check_object(&ring.template, &label)?;
        }
        if let Some(t) = &self.terrain {
            if t.atlas.trim().is_empty() {
                return Err("[terrain]: missing atlas".into());
            }
            if t.size == 0 || t.size > 128 {
                return Err(format!("[terrain]: size {} must be 1..=128", t.size));
            }
            if t.max_height > 256 {
                return Err(format!(
                    "[terrain]: max_height {} is over 256",
                    t.max_height
                ));
            }
            // Negative mirrors the field inside out; zero or NaN draws nothing.
            if !(t.block.is_finite() && t.block > 0.0) {
                return Err(format!(
                    "[terrain]: block {} must be a positive number",
                    t.block
                ));
            }
            let grid = 1..=MAX_ATLAS_GRID;
            if !grid.contains(&t.atlas_columns) || !grid.contains(&t.atlas_rows) {
                return Err(format!(
                    "[terrain]: atlas_columns {} and atlas_rows {} must each be 1..={MAX_ATLAS_GRID}",
                    t.atlas_columns, t.atlas_rows
                ));
            }
            // Both bounded above, so this can't overflow.
            let tiles = t.atlas_columns * t.atlas_rows;
            let mut names = vec![&t.top, &t.side, &t.under, &t.deep];
            if t.peak_height > 0 {
                names.push(&t.peak);
            }
            if t.water_level > 0 {
                names.push(&t.shore);
            }
            names.extend(t.ores.iter());
            for name in names {
                match t.tiles.get(name) {
                    None => {
                        return Err(format!(
                            "[terrain]: tile {name:?} is not in [terrain.tiles]"
                        ));
                    }
                    Some(&i) if i >= tiles => {
                        return Err(format!(
                            "[terrain]: tile {name:?} = {i} is outside the atlas"
                        ));
                    }
                    Some(_) => {}
                }
            }
        }
        // Checked here too (not only when the files are looked up) so a path
        // the asset server can't load, such as one with a '#', fails to parse.
        for path in crate::scene::referenced_files(self) {
            crate::scene::files::normalize(&path).map_err(|e| format!("bad file path: {e}"))?;
        }
        Ok(())
    }

    fn check_object(&self, obj: &ObjectDef, label: &str) -> Result<(), String> {
        if obj.mesh.is_none() && obj.model.is_none() {
            return Err(format!("{label}: needs a mesh or a model"));
        }
        if obj.mesh.is_some() && obj.model.is_some() {
            return Err(format!("{label}: has both a mesh and a model"));
        }
        if obj.model.is_some() && self.scene.dimension == Dimension::TwoD {
            return Err(format!("{label}: models need a 3d scene"));
        }
        if obj.animation.is_some() && obj.model.is_none() {
            return Err(format!("{label}: animation needs a model"));
        }
        if let Some(mesh) = &obj.mesh {
            let ok = match self.scene.dimension {
                Dimension::TwoD => matches!(mesh.as_str(), "circle" | "rect" | "ring"),
                Dimension::ThreeD => matches!(
                    mesh.as_str(),
                    "sphere" | "cube" | "plane" | "cylinder" | "torus" | "capsule" | "cone"
                ),
            };
            if !ok {
                return Err(format!(
                    "{label}: mesh {mesh:?} is not a {} primitive",
                    match self.scene.dimension {
                        Dimension::TwoD => "2d (circle, rect, ring)",
                        Dimension::ThreeD =>
                            "3d (sphere, cube, plane, cylinder, torus, capsule, cone)",
                    }
                ));
            }
        }
        if let Some(shader) = &obj.material.shader
            && !self.shaders.contains_key(shader)
        {
            return Err(format!("{label}: unknown shader {shader:?}"));
        }
        if obj.material.params.len() > 4 {
            return Err(format!("{label}: at most 4 params vec4s"));
        }
        for r in &obj.react {
            crate::scene::animate::Property::parse(&r.property)
                .map_err(|e| format!("{label}: {e}"))?;
        }
        if obj.collider.is_some() && self.scene.dimension == Dimension::ThreeD {
            return Err(format!("{label}: colliders are 2d only"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scene_parses_to_defaults() {
        let def = SceneDef::parse("").unwrap();
        assert_eq!(def.scene.dimension, Dimension::TwoD);
        assert!(def.objects.is_empty());
        assert!(def.scene.hud);
    }

    #[test]
    fn unknown_keys_are_rejected_with_their_name() {
        let err = SceneDef::parse("[scene]\nnmae = \"typo\"\n").unwrap_err();
        assert!(err.contains("nmae"), "{err}");
    }

    #[test]
    fn scale_and_radius_accept_scalar_or_vector() {
        let def = SceneDef::parse(
            r#"
            [[object]]
            mesh = "circle"
            scale = 2.0
            orbit = { radius = [100, 50] }
            [[object]]
            mesh = "rect"
            scale = [1, 2, 3]
            orbit = { radius = 80 }
            "#,
        )
        .unwrap();
        assert_eq!(def.objects[0].scale.to_array(), [2.0; 3]);
        assert_eq!(def.objects[1].scale.to_array(), [1.0, 2.0, 3.0]);
        assert_eq!(
            def.objects[0].orbit.as_ref().unwrap().radius.axes(),
            [100.0, 50.0]
        );
        assert_eq!(
            def.objects[1].orbit.as_ref().unwrap().radius.axes(),
            [80.0, 80.0]
        );
    }

    #[test]
    fn dangling_names_are_reported() {
        let err =
            SceneDef::parse("[[object]]\nmesh = \"circle\"\nmaterial = { shader = \"nope\" }\n")
                .unwrap_err();
        assert!(err.contains("nope"), "{err}");
        let err = SceneDef::parse("[blob]\nfill_shader = \"sun\"\n").unwrap_err();
        assert!(err.contains("sun"), "{err}");
    }

    #[test]
    fn parents_must_come_first() {
        let err = SceneDef::parse(
            r#"
            [[object]]
            name = "moon"
            mesh = "circle"
            orbit = { parent = "earth" }
            [[object]]
            name = "earth"
            mesh = "circle"
            "#,
        )
        .unwrap_err();
        assert!(err.contains("earth"), "{err}");
    }

    #[test]
    fn meshes_must_match_the_dimension() {
        assert!(SceneDef::parse("[[object]]\nmesh = \"cube\"\n").is_err());
        assert!(
            SceneDef::parse("[scene]\ndimension = \"3d\"\n[[object]]\nmesh = \"cube\"\n").is_ok()
        );
        assert!(SceneDef::parse("[[object]]\nmodel = \"x.glb\"\n").is_err());
    }

    #[test]
    fn bad_react_property_is_rejected() {
        let err =
            SceneDef::parse("[[object]]\nmesh = \"circle\"\nreact = [{ property = \"wobble\" }]\n")
                .unwrap_err();
        assert!(err.contains("wobble"), "{err}");
    }

    #[test]
    fn terrain_tiles_must_resolve() {
        let base = r#"
            [scene]
            dimension = "3d"
            [terrain]
            atlas = "a.png"
            tiles = { grass_top = 0, grass_side = 1, dirt = 2 }
        "#;
        let err = SceneDef::parse(base).unwrap_err();
        assert!(err.contains("stone"), "{err}");
        let ok = format!("{base}\n[terrain.tiles]\n");
        // Re-declaring the table is a TOML error; add the tile inline instead.
        assert!(SceneDef::parse(&ok).is_err());
        let ok = base.replace("dirt = 2 }", "dirt = 2, stone = 3 }");
        SceneDef::parse(&ok).unwrap();
    }

    /// A valid terrain scene with `extra` keys added to `[terrain]`.
    fn terrain_with(extra: &str) -> Result<SceneDef, String> {
        SceneDef::parse(&format!(
            "[scene]\ndimension = \"3d\"\n[terrain]\natlas = \"a.png\"\n\
             tiles = {{ grass_top = 0, grass_side = 1, dirt = 2, stone = 3, sand = 5 }}\n{extra}\n"
        ))
    }

    #[test]
    fn terrain_block_must_be_a_positive_number() {
        terrain_with("").unwrap();
        terrain_with("block = 0.25").unwrap();
        for block in ["0.0", "-1.0", "nan", "inf"] {
            let err = terrain_with(&format!("block = {block}")).unwrap_err();
            assert!(err.contains("block"), "{block}: {err}");
        }
    }

    #[test]
    fn terrain_atlas_grid_is_bounded_instead_of_overflowing() {
        // 65536² overflows u32: this must be an error, not a panic.
        let err = terrain_with("atlas_columns = 65536\natlas_rows = 65536").unwrap_err();
        assert!(err.contains("atlas_columns"), "{err}");
        assert!(terrain_with("atlas_rows = 0").is_err());
        terrain_with("atlas_columns = 1024\natlas_rows = 1024").unwrap();
        // A tile past the end of a small atlas is still caught.
        let err = terrain_with("atlas_columns = 2\natlas_rows = 1").unwrap_err();
        assert!(err.contains("outside the atlas"), "{err}");
    }

    #[test]
    fn water_level_needs_a_shore_tile_but_no_water_tile() {
        // No `water` tile is declared: nothing draws one, so none is required.
        terrain_with("water_level = 3").unwrap();
        // The field still parses, for scenes written against the old docs.
        terrain_with("water_level = 3\nwater = \"lake\"").unwrap();
        let err = terrain_with("water_level = 3\nshore = \"beach\"").unwrap_err();
        assert!(err.contains("beach"), "{err}");
    }

    #[test]
    fn key_sounds_need_a_key_name_that_exists() {
        let sound = |key: &str| {
            SceneDef::parse(&format!(
                "[sounds.jump]\npath = \"j.ogg\"\ntrigger = \"key\"\n{key}\n"
            ))
        };
        sound("key = \"j\"").unwrap();
        sound("key = \"F5\"").unwrap();
        assert!(sound("").unwrap_err().contains("needs key"));
        for bad in ["up", "shift", ""] {
            let err = sound(&format!("key = {bad:?}")).unwrap_err();
            assert!(err.contains("unknown key"), "{bad}: {err}");
            assert!(err.contains("semicolon"), "lists the names: {err}");
        }
        // Other triggers ignore `key`, so it isn't checked there.
        SceneDef::parse("[sounds.x]\npath = \"x.ogg\"\nkey = \"up\"\n").unwrap();
    }

    #[test]
    fn every_key_name_in_the_error_parses() {
        let letters = ('a'..='z').map(String::from);
        let digits = ('0'..='9').map(String::from);
        let fkeys = (1..=12).map(|n| format!("f{n}"));
        let named = NAMED_KEYS.iter().map(|n| n.to_string());
        for name in letters.chain(digits).chain(fkeys).chain(named) {
            assert!(crate::config::parse_key(&name).is_some(), "{name}");
        }
    }

    #[test]
    fn file_paths_with_a_hash_fail_to_parse() {
        let err = SceneDef::parse("[sounds.note]\npath = \"sounds/C#4.ogg\"\n").unwrap_err();
        assert!(err.contains('#'), "{err}");
        let err = SceneDef::parse(
            "[[object]]\nmesh = \"circle\"\nmaterial = { texture = \"textures/a.png#\" }\n",
        )
        .unwrap_err();
        assert!(err.contains("a.png#"), "{err}");
        assert!(SceneDef::parse("[shaders]\nsun = \"../sun.wgsl\"\n").is_err());
    }

    #[test]
    fn config_overrides_are_kept_as_a_table() {
        let def = SceneDef::parse("[config.vis]\ninner_radius = 0.2\n").unwrap();
        let vis = def.config.unwrap()["vis"].as_table().unwrap().clone();
        assert_eq!(vis["inner_radius"].as_float(), Some(0.2));
    }
}
