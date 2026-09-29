// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`FxMaterial`]: the one 2D material every effect — and every scene shader —
//! is drawn with.
//!
//! A `Material2d`'s fragment shader is normally fixed per *type*, which would
//! mean one Rust type per shader and no way to load a shader a scene file names
//! at runtime. Instead the shader handle rides in the material's
//! `bind_group_data` key, and [`Material2d::specialize`] swaps it into the
//! pipeline descriptor — so each distinct shader just becomes one more
//! specialization of the same pipeline. The blend mode is keyed the same way:
//! glows and particles override the blend state to additive, which
//! [`AlphaMode2d`] has no variant for.
//!
//! Every material shares one uniform layout ([`FxUniform`], mirrored by
//! `struct FxUniform` in `shaders/fx.wgsl`), refreshed each frame by
//! [`sync_fx_materials`] with the live audio features and palette. That shared
//! contract is what lets a scene author's shader react to the bass without any
//! Rust on their side.

use bevy::asset::uuid_handle;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d,
    RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, TextureDimension,
    TextureFormat,
};
use bevy::shader::{Shader, ShaderRef};
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPlugin};

use crate::vis::VisSettings;
use crate::vis::features::{AudioFeatures, FeaturesSet};

/// `bava::fx` — the uniform struct and noise helpers.
pub const FX_LIB_SHADER: Handle<Shader> = uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a01");
/// `bava::fx_material` — the material bindings + palette / rim helpers.
pub const FX_MATERIAL_LIB_SHADER: Handle<Shader> =
    uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a02");
/// The blob's plasma interior.
pub const BLOB_SHADER: Handle<Shader> = uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a03");
/// The additive glow + corona around the blob.
pub const HALO_SHADER: Handle<Shader> = uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a04");
/// The starfield / nebula backdrop.
pub const BACKDROP_SHADER: Handle<Shader> = uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a05");
/// Lit glossy physics balls.
pub const BALL_SHADER: Handle<Shader> = uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a06");
/// Vertex-colored additive geometry (sparks, flares, shockwaves).
pub const ADDITIVE_SHADER: Handle<Shader> = uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a07");
const CACHED_NOISE_SHADER: Handle<Shader> = uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a08");
pub(crate) const NOISE_TEXTURE: Handle<Image> =
    uuid_handle!("5b0c7e0e-2c61-4f1f-9d1c-1f0a8f2b6a09");

// Pack the four value-noise corners into each texel. One unfiltered fetch
// replaces four PCG hashes per octave, without reducing noise precision.
fn noise_texture() -> Image {
    const SIZE: i32 = 512;
    fn hash(x: i32, y: i32) -> f32 {
        let wrap = |n: i32| (n + 256).rem_euclid(SIZE) - 256;
        let mut x = (wrap(x) as u32)
            .wrapping_mul(1664525)
            .wrapping_add(1013904223);
        let mut y = (wrap(y) as u32)
            .wrapping_mul(1664525)
            .wrapping_add(1013904223);
        x = x.wrapping_add(y.wrapping_mul(1664525));
        y = y.wrapping_add(x.wrapping_mul(1664525));
        x ^= x >> 16;
        y ^= y >> 16;
        x = x.wrapping_add(y.wrapping_mul(1664525));
        x ^= x >> 16;
        (x >> 8) as f32 * (1.0 / 16777216.0)
    }
    let mut data = Vec::with_capacity(SIZE as usize * SIZE as usize * 16);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let signed = |n| if n >= 256 { n - SIZE } else { n };
            let (x, y) = (signed(x), signed(y));
            for value in [
                hash(x, y),
                hash(x + 1, y),
                hash(x, y + 1),
                hash(x + 1, y + 1),
            ] {
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    Image::new(
        Extent3d {
            width: SIZE as u32,
            height: SIZE as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba32Float,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    )
}

/// Register the embedded WGSL sources under their fixed handles. Idempotent,
/// and safe to call from any plugin that needs them.
pub fn register_shaders(app: &mut App) {
    if let Some(mut images) = app.world_mut().get_resource_mut::<Assets<Image>>() {
        if !images.contains(NOISE_TEXTURE.id()) {
            let _ = images.insert(NOISE_TEXTURE.id(), noise_texture());
        }
    }
    let Some(mut shaders) = app.world_mut().get_resource_mut::<Assets<Shader>>() else {
        return; // no render stack (headless unit tests)
    };
    let sources: [(&Handle<Shader>, &'static str, &str); 8] = [
        (
            &CACHED_NOISE_SHADER,
            include_str!("shaders/fx_cached.wgsl"),
            "bava/fx_cached.wgsl",
        ),
        (
            &FX_LIB_SHADER,
            include_str!("shaders/fx.wgsl"),
            "bava/fx.wgsl",
        ),
        (
            &FX_MATERIAL_LIB_SHADER,
            include_str!("shaders/fx_material.wgsl"),
            "bava/fx_material.wgsl",
        ),
        (
            &BLOB_SHADER,
            include_str!("shaders/blob.wgsl"),
            "bava/blob.wgsl",
        ),
        (
            &HALO_SHADER,
            include_str!("shaders/halo.wgsl"),
            "bava/halo.wgsl",
        ),
        (
            &BACKDROP_SHADER,
            include_str!("shaders/backdrop.wgsl"),
            "bava/backdrop.wgsl",
        ),
        (
            &BALL_SHADER,
            include_str!("shaders/ball.wgsl"),
            "bava/ball.wgsl",
        ),
        (
            &ADDITIVE_SHADER,
            include_str!("shaders/additive.wgsl"),
            "bava/additive.wgsl",
        ),
    ];
    for (handle, source, path) in sources {
        if shaders.contains(handle.id()) {
            continue;
        }
        let _ = shaders.insert(handle.id(), Shader::from_wgsl(source, path));
    }
}

/// The uniform block shared by every effect and scene shader. Field order and
/// types must match `struct FxUniform` in `shaders/fx.wgsl`.
#[derive(ShaderType, Clone, Copy, Debug, PartialEq)]
pub struct FxUniform {
    /// Base tint, linear RGB (HDR allowed) + alpha.
    pub color: Vec4,
    /// Up to four live palette stops, linear RGB.
    pub palette: [Vec4; 4],
    /// (bass, mid, treble, energy).
    pub audio: Vec4,
    /// (beat pulse, flow clock, seconds, palette stop count).
    pub clock: Vec4,
    /// Free per-material parameters.
    pub params: [Vec4; 4],
    /// 64 blob rim radii in px, evenly spaced by angle.
    pub shape: [Vec4; 16],
    /// (viewport width, viewport height, blob base radius, rotation).
    pub info: Vec4,
}

impl Default for FxUniform {
    fn default() -> Self {
        Self {
            color: Vec4::ONE,
            palette: [Vec4::ONE; 4],
            audio: Vec4::ZERO,
            clock: Vec4::new(0.0, 0.0, 0.0, 1.0),
            params: [Vec4::ZERO; 4],
            shape: [Vec4::ZERO; 16],
            info: Vec4::ZERO,
        }
    }
}

impl FxUniform {
    /// Write the 64 rim radii into [`shape`](Self::shape).
    pub fn set_shape(&mut self, radii: &[f32; 64]) {
        for (slot, chunk) in self.shape.iter_mut().zip(radii.as_chunks::<4>().0) {
            *slot = Vec4::from_array(*chunk);
        }
    }
}

/// How a material's output is combined with what is behind it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum FxBlend {
    /// Standard alpha blending.
    #[default]
    Alpha,
    /// `dst + src.rgb` — for glows, particles and light.
    Additive,
    /// No blending (alpha ignored).
    Opaque,
}

/// A 2D material whose fragment shader and blend mode are chosen per instance.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
#[bind_group_data(FxMaterialKey)]
pub struct FxMaterial {
    #[uniform(0)]
    pub uniform: FxUniform,
    /// Optional image, bound as `fx_texture` (a white fallback when `None`).
    #[texture(1)]
    #[sampler(2)]
    pub texture: Option<Handle<Image>>,
    #[texture(3, filterable = false)]
    pub noise_texture: Handle<Image>,
    /// Fragment shader. Must import `bava::fx_material` for its bindings.
    pub shader: Handle<Shader>,
    pub blend: FxBlend,
    /// Refresh the audio / palette / clock fields every frame. Materials that
    /// never read them can opt out and stay unmodified (no re-upload).
    pub live: bool,
}

impl FxMaterial {
    /// A live, alpha-blended material drawn with `shader`.
    pub fn new(shader: Handle<Shader>) -> Self {
        Self {
            uniform: FxUniform::default(),
            texture: None,
            noise_texture: NOISE_TEXTURE,
            shader,
            blend: FxBlend::Alpha,
            live: true,
        }
    }

    pub fn with_blend(mut self, blend: FxBlend) -> Self {
        self.blend = blend;
        self
    }
}

/// Specialization key: which shader, and how to blend it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FxMaterialKey {
    shader: Handle<Shader>,
    blend: FxBlend,
}

impl From<&FxMaterial> for FxMaterialKey {
    fn from(m: &FxMaterial) -> Self {
        Self {
            shader: m.shader.clone(),
            blend: m.blend,
        }
    }
}

/// `src.rgb + dst.rgb`, leaving the destination alpha alone. Shared with the
/// 3D twin, whose `AlphaMode::Add` would otherwise be premultiplied-over.
pub(crate) const ADDITIVE_BLEND: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

impl Material2d for FxMaterial {
    fn fragment_shader() -> ShaderRef {
        // Replaced per instance in `specialize`; this only has to be a valid
        // fragment entry point for the default descriptor.
        ShaderRef::Handle(ADDITIVE_SHADER)
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        match self.blend {
            FxBlend::Opaque => AlphaMode2d::Opaque,
            // Additive rides the transparent (sorted) phase; the blend state
            // itself is overridden in `specialize`.
            FxBlend::Alpha | FxBlend::Additive => AlphaMode2d::Blend,
        }
    }

    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader = key.bind_group_data.shader.clone();
            if key.bind_group_data.blend == FxBlend::Additive {
                for target in fragment.targets.iter_mut().flatten() {
                    target.blend = Some(ADDITIVE_BLEND);
                }
            }
        }
        Ok(())
    }
}

/// The live palette, packed for the uniform: up to four linear stops plus the
/// stop count. More than four stops (a five-color album palette) are resampled
/// evenly so both ends and the overall sweep survive.
pub fn palette_uniform(stops: &[Color]) -> ([Vec4; 4], f32) {
    let lin: Vec<Vec4> = if stops.is_empty() {
        vec![Vec4::ONE]
    } else {
        stops.iter().map(|c| c.to_linear().to_vec4()).collect()
    };
    let mut out = [*lin.last().unwrap_or(&Vec4::ONE); 4];
    if lin.len() <= 4 {
        out[..lin.len()].copy_from_slice(&lin);
        return (out, lin.len() as f32);
    }
    let span = (lin.len() - 1) as f32;
    for (k, slot) in out.iter_mut().enumerate() {
        let x = k as f32 / 3.0 * span;
        let i = (x.floor() as usize).min(lin.len() - 2);
        *slot = lin[i].lerp(lin[i + 1], x - i as f32);
    }
    (out, 4.0)
}

/// Pack the live audio features into the (audio, clock) uniform pair.
pub fn audio_uniform(features: &AudioFeatures, palette_count: f32) -> (Vec4, Vec4) {
    (
        Vec4::new(
            features.bass,
            features.mid,
            features.treble,
            features.energy,
        ),
        Vec4::new(
            features.beat_pulse,
            features.flow,
            features.time,
            palette_count,
        ),
    )
}

/// This frame's shared uniform fields — palette, audio, clock and viewport —
/// packed once and stamped onto every live material.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LiveInputs {
    palette: [Vec4; 4],
    audio: Vec4,
    clock: Vec4,
    viewport: Vec2,
}

impl LiveInputs {
    pub(crate) fn new(
        features: &AudioFeatures,
        vis: &VisSettings,
        windows: &Query<&Window>,
    ) -> Self {
        let (palette, count) = palette_uniform(&vis.fg_stops());
        let (audio, clock) = audio_uniform(features, count);
        let viewport = windows.iter().next().map_or(Vec2::new(1280.0, 720.0), |w| {
            Vec2::new(w.width(), w.height())
        });
        Self {
            palette,
            audio,
            clock,
            viewport,
        }
    }

    /// `u` with this frame's live fields written in.
    pub(crate) fn applied(&self, mut u: FxUniform) -> FxUniform {
        u.palette = self.palette;
        u.audio = self.audio;
        u.clock = self.clock;
        u.info.x = self.viewport.x;
        u.info.y = self.viewport.y;
        u
    }
}

/// Refresh every live [`FxMaterial`] with this frame's audio features, palette
/// and viewport size.
///
/// Only materials whose uniform actually changes are borrowed mutably:
/// `Assets::iter_mut` queues `AssetEvent::Modified` for *every* asset it
/// yields, and a modified material is re-uploaded and every mesh drawn with it
/// re-specialized — so iterating mutably would churn the non-live ones (the
/// particle batch, the ball buckets) and all their entities each frame.
pub fn sync_fx_materials(
    features: Res<AudioFeatures>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut materials: ResMut<Assets<FxMaterial>>,
    mut stale: Local<Vec<AssetId<FxMaterial>>>,
) {
    let live = LiveInputs::new(&features, &vis, &windows);
    stale.clear();
    stale.extend(
        materials
            .iter()
            .filter(|(_, m)| m.live && live.applied(m.uniform) != m.uniform)
            .map(|(id, _)| id),
    );
    for id in stale.drain(..) {
        if let Some(mut m) = materials.get_mut(id) {
            m.uniform = live.applied(m.uniform);
        }
    }
}

/// Registers [`FxMaterial`], its embedded shaders and the per-frame sync.
pub struct FxMaterialPlugin;

impl Plugin for FxMaterialPlugin {
    fn build(&self, app: &mut App) {
        register_shaders(app);
        app.add_plugins(Material2dPlugin::<FxMaterial>::default())
            .add_systems(
                Update,
                sync_fx_materials.in_set(FxSyncSet).after(FeaturesSet),
            );
    }
}

/// [`sync_fx_materials`]. Systems that set per-material fields (shape, params)
/// run after it so their writes aren't clobbered.
#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FxSyncSet;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_noise_corners_join_across_every_cell_and_wrap() {
        let image = noise_texture();
        let data = image.data.as_ref().unwrap();
        let texel = |x: usize, y: usize| -> [f32; 4] {
            std::array::from_fn(|c| {
                let start = ((y * 512 + x) * 4 + c) * 4;
                f32::from_le_bytes(data[start..start + 4].try_into().unwrap())
            })
        };
        // Known PCG outputs from the procedural shader, before normalization.
        assert_eq!(
            texel(0, 0),
            [1631281.0, 10341361.0, 9035872.0, 5162297.0].map(|v| v / 16777216.0)
        );
        assert_eq!(texel(511, 511)[0], 1975946.0 / 16777216.0);
        for y in 0..512 {
            for x in 0..512 {
                let a = texel(x, y);
                let right = texel((x + 1) % 512, y);
                let above = texel(x, (y + 1) % 512);
                assert_eq!(a[1], right[0]);
                assert_eq!(a[3], right[2]);
                assert_eq!(a[2], above[0]);
                assert_eq!(a[3], above[1]);
                assert!(a.iter().all(|v| (0.0..1.0).contains(v)));
            }
        }
    }

    #[test]
    fn palette_uniform_keeps_short_palettes_verbatim() {
        let (p, n) = palette_uniform(&[Color::BLACK, Color::WHITE]);
        assert_eq!(n, 2.0);
        assert_eq!(p[0].truncate(), Vec3::ZERO);
        assert_eq!(p[1].truncate(), Vec3::ONE);
        let (p, n) = palette_uniform(&[]);
        assert_eq!(n, 1.0);
        assert_eq!(p[0], Vec4::ONE);
    }

    #[test]
    fn palette_uniform_resamples_five_stops_keeping_the_ends() {
        let stops: Vec<Color> = (0..5)
            .map(|i| Color::linear_rgb(i as f32 / 4.0, 0.0, 0.0))
            .collect();
        let (p, n) = palette_uniform(&stops);
        assert_eq!(n, 4.0);
        assert!((p[0].x - 0.0).abs() < 1e-6);
        assert!((p[3].x - 1.0).abs() < 1e-6);
        // Evenly resampled along the ramp.
        assert!((p[1].x - 1.0 / 3.0).abs() < 1e-5);
        assert!((p[2].x - 2.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn set_shape_packs_radii_in_order() {
        let mut radii = [0.0f32; 64];
        for (i, r) in radii.iter_mut().enumerate() {
            *r = i as f32;
        }
        let mut u = FxUniform::default();
        u.set_shape(&radii);
        assert_eq!(u.shape[0], Vec4::new(0.0, 1.0, 2.0, 3.0));
        assert_eq!(u.shape[15], Vec4::new(60.0, 61.0, 62.0, 63.0));
    }

    /// Every `AssetEvent::Modified` id seen since the last drain.
    #[derive(Resource, Default)]
    struct Modified(Vec<AssetId<FxMaterial>>);

    fn collect_modified(
        mut events: MessageReader<AssetEvent<FxMaterial>>,
        mut out: ResMut<Modified>,
    ) {
        for event in events.read() {
            if let AssetEvent::Modified { id } = event {
                out.0.push(*id);
            }
        }
    }

    fn drain_modified(app: &mut App) -> Vec<AssetId<FxMaterial>> {
        std::mem::take(&mut app.world_mut().resource_mut::<Modified>().0)
    }

    #[test]
    fn sync_modifies_only_live_materials_whose_uniform_changed() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<FxMaterial>()
            .init_resource::<AudioFeatures>()
            .init_resource::<VisSettings>()
            .init_resource::<Modified>()
            .add_systems(Update, sync_fx_materials)
            .add_systems(Last, collect_modified);
        // Hold the strong handles: dropping them would free both materials
        // before the first sync runs.
        let (live_handle, quiet_handle) = {
            let mut assets = app.world_mut().resource_mut::<Assets<FxMaterial>>();
            let live = assets.add(FxMaterial::new(BLOB_SHADER));
            let mut quiet = FxMaterial::new(ADDITIVE_SHADER);
            quiet.live = false;
            (live, assets.add(quiet))
        };
        let (live, quiet) = (live_handle.id(), quiet_handle.id());

        // The first sync stamps the live fields in; the opted-out material
        // must not even be flagged (a flag alone re-uploads it and
        // re-specializes every mesh drawn with it).
        app.update();
        let modified = drain_modified(&mut app);
        assert!(modified.contains(&live));
        assert!(!modified.contains(&quiet), "non-live material was modified");

        // Nothing moved: nothing is touched.
        app.update();
        assert!(drain_modified(&mut app).is_empty());

        // The music moves: only the live material follows it.
        app.world_mut().resource_mut::<AudioFeatures>().bass = 0.5;
        app.update();
        assert_eq!(drain_modified(&mut app), vec![live]);
        let assets = app.world().resource::<Assets<FxMaterial>>();
        assert_eq!(assets.get(live).unwrap().uniform.audio.x, 0.5);
        assert_eq!(assets.get(quiet).unwrap().uniform.audio.x, 0.0);
    }

    #[test]
    fn material_key_distinguishes_shader_and_blend() {
        let a = FxMaterialKey::from(&FxMaterial::new(BLOB_SHADER));
        let b = FxMaterialKey::from(&FxMaterial::new(HALO_SHADER));
        let c = FxMaterialKey::from(&FxMaterial::new(BLOB_SHADER).with_blend(FxBlend::Additive));
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, FxMaterialKey::from(&FxMaterial::new(BLOB_SHADER)));
    }
}
