// SPDX-License-Identifier: MIT OR Apache-2.0
//! Build a parsed [`SceneDef`] into the world.
//!
//! Everything spawned here carries [`SceneEntity`] so the scene can be torn
//! down wholesale, and every setting touched on a built-in entity (the blob's
//! shaders, the vis camera's clear / render layers, the HUD) is put back by
//! [`restore_builtin_layers`].

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};

use avian2d::prelude::{Collider, LinearVelocity, Restitution, RigidBody, Rotation};
use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
use bevy::animation::{AnimationClip, AnimationPlayer};
use bevy::asset::UntypedAssetId;
use bevy::camera::{ClearColorConfig, Hdr, RenderTarget, visibility::NoFrustumCulling};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::gltf::GltfAssetLabel;
use bevy::image::{ImageLoaderSettings, ImageSampler};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::sprite_render::AlphaMode2d;
use bevy::world_serialization::{WorldAssetRoot, WorldInstanceReady};

use super::animate::{
    BandRef, Binding, MaterialLink, Orbit, Property, Reactive, SceneCamera, SceneLight,
    SceneObject, Spin,
};
use super::def::{
    Anchor, BlendDef, ColliderDef, Dimension, LightKind, MaterialDef, ObjectDef, SceneDef,
};
use super::files::{self, SceneFiles};
use super::material3d::FxMaterial3d;
use super::ready::SceneLoads;
use super::sound::{SceneSound, SceneSounds};
use super::terrain::{MeshData, Terrain, atlas_cube};
use super::{SceneCamera3d, SceneEntity, SceneRuntime, set_2d_hidden};
use crate::config::hex_to_color;
use crate::vis::bars::VisCamera;
use crate::vis::fx::material::{
    BACKDROP_SHADER, BLOB_SHADER, FxBlend, FxMaterial, FxUniform, HALO_SHADER,
};

/// What a load hands the spawner.
pub(crate) struct SpawnContext {
    /// Embedded-registry slot the scene's files were registered under.
    pub slot: String,
    /// `[shaders]` name → (path, WGSL source).
    pub shader_sources: HashMap<String, (String, String)>,
    pub files: SceneFiles,
}

/// Compiled scene shaders by name.
type Shaders = HashMap<String, Handle<Shader>>;

/// Build the whole scene.
pub(crate) fn spawn_scene(
    world: &mut World,
    def: &SceneDef,
    ctx: &SpawnContext,
) -> Result<(), String> {
    let _ = &ctx.files;
    // Whatever the previous scene was waiting on is gone with it.
    if let Some(mut loads) = world.get_resource_mut::<SceneLoads>() {
        loads.clear();
    }
    let shaders = compile_shaders(world, ctx);

    world.insert_resource(super::animate::SceneLayout {
        canvas: if def.scene.dimension == Dimension::TwoD {
            def.scene.canvas.max(0.0)
        } else {
            0.0
        },
    });
    if let Some(c) = &def.scene.clear_color {
        world.resource_mut::<ClearColor>().0 = color(c)?;
    }
    set_hud_visible(world, def.scene.hud);

    match def.scene.dimension {
        Dimension::TwoD => apply_blob_overrides(world, def, &shaders)?,
        Dimension::ThreeD => {
            spawn_camera(world, def, &shaders)?;
            if !def.scene.overlay_2d {
                set_2d_hidden(world, true);
            }
            spawn_lights(world, def)?;
        }
    }

    let mut names: HashMap<String, Entity> = HashMap::new();
    let mut cache = MeshCache::default();
    for (i, obj) in def.objects.iter().enumerate() {
        let entity = spawn_object(
            world,
            def,
            obj,
            2 * i,
            &shaders,
            ctx,
            &names,
            &mut cache,
            Instance::default(),
        )
        .map_err(|e| format!("{}: {e}", label(obj, i)))?;
        if let Some(name) = &obj.name {
            names.insert(name.clone(), entity);
        }
        if def.scene.dimension == Dimension::TwoD
            && let Some(orbit) = &obj.orbit
            && orbit.path_width > 0.0
        {
            spawn_orbit_path(world, obj, orbit, 2 * i + 1, &names)?;
        }
    }

    let mut index = 2 * def.objects.len();
    for (r, ring) in def.rings.iter().enumerate() {
        let n = ring.count as usize;
        for k in 0..n {
            let t = k as f32 / n as f32;
            let fraction = if ring.mirror_bars {
                (t.min(1.0 - t) * 2.0).clamp(0.0, 1.0)
            } else if n > 1 {
                k as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let plane = match (def.scene.dimension, ring.horizontal) {
                (Dimension::ThreeD, false) => Quat::from_rotation_x(FRAC_PI_2),
                _ => Quat::IDENTITY,
            };
            let orbit = Orbit {
                parent: None,
                center: Vec3::from(ring.center),
                axes: Vec2::from(ring.radius.axes()),
                angular: ring.spin.to_radians(),
                angle: (ring.phase + t) * TAU,
                plane,
                three_d: def.scene.dimension == Dimension::ThreeD,
                speed_band: None,
                speed_react: 0.0,
                face_out: ring.face_out,
                behind_z: ring.behind_z,
                perspective: ring.perspective,
            };
            let tile = (!ring.tiles.is_empty()).then(|| ring.tiles[k % ring.tiles.len()]);
            spawn_object(
                world,
                def,
                &ring.template,
                index,
                &shaders,
                ctx,
                &names,
                &mut cache,
                Instance {
                    orbit: Some(orbit),
                    bar: Some(fraction),
                    tile,
                },
            )
            .map_err(|e| format!("[[ring]] #{}: {e}", r + 1))?;
            index += 1;
        }
    }

    if let Some(t) = &def.terrain {
        spawn_terrain(world, t, ctx)?;
    }
    load_sounds(world, def, ctx)?;
    Ok(())
}

fn label(obj: &ObjectDef, i: usize) -> String {
    obj.name
        .clone()
        .unwrap_or_else(|| format!("[[object]] #{}", i + 1))
}

/// Parse a hex color.
fn color(hex: &str) -> Result<Color, String> {
    hex_to_color(hex).ok_or_else(|| format!("bad color {hex:?} (use #rrggbb or #aarrggbb)"))
}

fn compile_shaders(world: &mut World, ctx: &SpawnContext) -> Shaders {
    // Make sure the `bava::fx` imports resolve even in an app that never
    // added the effects plugin's registration (it is idempotent).
    let mut out = HashMap::new();
    let mut assets = world.resource_mut::<Assets<Shader>>();
    for (name, (path, source)) in &ctx.shader_sources {
        let shader = Shader::from_wgsl(source.clone(), format!("bava-scene/{}/{path}", ctx.slot));
        out.insert(name.clone(), assets.add(shader));
    }
    out
}

/// Put the built-in layers back the way bava draws them without a scene.
pub(crate) fn restore_builtin_layers(world: &mut World) {
    swap_blob_shader(world, Layer::Fill, BLOB_SHADER, &[]);
    swap_blob_shader(world, Layer::Halo, HALO_SHADER, &[]);
    swap_blob_shader(world, Layer::Backdrop, BACKDROP_SHADER, &[]);
    set_2d_hidden(world, false);
    let mut cams = world.query_filtered::<&mut Camera, With<VisCamera>>();
    for mut cam in cams.iter_mut(world) {
        cam.clear_color = ClearColorConfig::Default;
    }
    set_hud_visible(world, true);
}

fn set_hud_visible(world: &mut World, visible: bool) {
    let mut q = world.query_filtered::<&mut Visibility, With<crate::vis::hud::HudRoot>>();
    for mut v in q.iter_mut(world) {
        v.set_if_neq(if visible {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

#[derive(Clone, Copy)]
enum Layer {
    Fill,
    Halo,
    Backdrop,
}

/// Point one built-in blob layer at `shader`, with `params` in `params[1..]`.
fn swap_blob_shader(world: &mut World, layer: Layer, shader: Handle<Shader>, params: &[[f32; 4]]) {
    let handle = match layer {
        Layer::Fill => crate::vis::circle::fill_material(world),
        Layer::Halo => crate::vis::fx::halo_material(world),
        Layer::Backdrop => crate::vis::fx::backdrop_material(world),
    };
    let Some(handle) = handle else {
        return;
    };
    let mut materials = world.resource_mut::<Assets<FxMaterial>>();
    if let Some(mut m) = materials.get_mut(&handle) {
        m.shader = shader;
        for slot in 1..4 {
            m.uniform.params[slot] = params
                .get(slot - 1)
                .map(|p| Vec4::from_array(*p))
                .unwrap_or(Vec4::ZERO);
        }
    }
}

fn apply_blob_overrides(
    world: &mut World,
    def: &SceneDef,
    shaders: &Shaders,
) -> Result<(), String> {
    let pick = |name: &Option<String>| name.as_ref().and_then(|n| shaders.get(n)).cloned();
    if let Some(s) = pick(&def.blob.fill_shader) {
        swap_blob_shader(world, Layer::Fill, s, &def.blob.fill_params);
    }
    if let Some(s) = pick(&def.blob.halo_shader) {
        swap_blob_shader(world, Layer::Halo, s, &def.blob.halo_params);
    }
    if let Some(s) = pick(&def.blob.backdrop_shader) {
        swap_blob_shader(world, Layer::Backdrop, s, &def.blob.backdrop_params);
    }
    Ok(())
}

fn load_image(
    world: &mut World,
    ctx: &SpawnContext,
    rel: &str,
    pixelated: bool,
) -> Result<Handle<Image>, String> {
    let path = files::asset_path(&ctx.slot, rel)?;
    let server = world.resource::<AssetServer>();
    let handle: Handle<Image> = if pixelated {
        server
            .load_builder()
            .with_settings(|s: &mut ImageLoaderSettings| {
                s.sampler = ImageSampler::nearest();
            })
            .load::<Image>(path)
    } else {
        server.load(path)
    };
    track(world, handle.id());
    Ok(handle)
}

/// Have an offline render wait for `id` to load before its first frame (see
/// [`super::ready`]).
fn track(world: &mut World, id: impl Into<UntypedAssetId>) {
    if let Some(mut loads) = world.get_resource_mut::<SceneLoads>() {
        loads.track(id);
    }
}

fn params(p: &[[f32; 4]]) -> [Vec4; 4] {
    let mut out = [Vec4::ZERO; 4];
    for (slot, v) in out.iter_mut().zip(p) {
        *slot = Vec4::from_array(*v);
    }
    out
}

fn fx_blend(b: BlendDef) -> FxBlend {
    match b {
        BlendDef::Alpha | BlendDef::Mask => FxBlend::Alpha,
        BlendDef::Additive => FxBlend::Additive,
        BlendDef::Opaque => FxBlend::Opaque,
    }
}

/// Per-instance overrides when spawning a ring member.
#[derive(Default, Clone)]
struct Instance {
    orbit: Option<Orbit>,
    /// Spectrum position a `band = "bar"` reaction reads.
    bar: Option<f32>,
    /// Atlas tile replacing the template's.
    tile: Option<u32>,
}

/// Meshes and materials shared between ring instances.
#[derive(Default)]
struct MeshCache {
    meshes: HashMap<String, Handle<Mesh>>,
    materials_2d: HashMap<String, (Mat2, MaterialLinkBase)>,
    materials_3d: HashMap<String, (Mat3, MaterialLinkBase)>,
}

/// The entry under `key`, made on first use. A `private` one — a material an
/// object's own reactions rewrite every frame — is made fresh and kept out of
/// the cache, or every later object sharing its definition would pulse along.
fn cached<T: Clone>(
    cache: &mut HashMap<String, T>,
    key: String,
    private: bool,
    make: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if private {
        return make();
    }
    if let Some(hit) = cache.get(&key) {
        return Ok(hit.clone());
    }
    let made = make()?;
    cache.insert(key, made.clone());
    Ok(made)
}

#[derive(Clone)]
enum Mat2 {
    Fx(Handle<FxMaterial>),
    Color(Handle<ColorMaterial>),
}

#[derive(Clone)]
enum Mat3 {
    Fx(Handle<FxMaterial3d>),
    Standard(Handle<StandardMaterial>),
}

/// The rest values a [`MaterialLink`] scales from.
#[derive(Clone, Copy)]
struct MaterialLinkBase {
    color: Vec4,
    emissive: LinearRgba,
    params: [Vec4; 4],
}

fn size(obj: &ObjectDef, i: usize, default: f32) -> f32 {
    obj.size
        .get(i)
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

fn build_mesh(obj: &ObjectDef, dim: Dimension, tile: Option<u32>) -> Result<Mesh, String> {
    let kind = obj.mesh.as_deref().unwrap_or("");
    let mesh = match (dim, kind) {
        (Dimension::TwoD, "circle") => {
            let r = size(obj, 0, 50.0);
            Circle::new(r).mesh().resolution(64).build()
        }
        (Dimension::TwoD, "rect") => {
            let (w, h) = (size(obj, 0, 100.0), size(obj, 1, size(obj, 0, 100.0)));
            Rectangle::new(w, h).into()
        }
        (Dimension::TwoD, "ring") => {
            let r = size(obj, 0, 100.0);
            let w = size(obj, 1, 2.0);
            Annulus::new((r - w * 0.5).max(0.0), r + w * 0.5)
                .mesh()
                .resolution(128)
                .build()
        }
        (Dimension::ThreeD, "sphere") => {
            let r = size(obj, 0, 1.0);
            Sphere::new(r).mesh().uv(48, 24)
        }
        (Dimension::ThreeD, "cube") => {
            let x = size(obj, 0, 1.0);
            let y = size(obj, 1, x);
            let z = size(obj, 2, x);
            match tile.or(obj.material.atlas_tile) {
                Some(t) => atlas_cube(t, obj.material.atlas_columns, obj.material.atlas_rows)
                    .scaled_by(Vec3::new(x, y, z)),
                None => Cuboid::new(x, y, z).into(),
            }
        }
        (Dimension::ThreeD, "plane") => {
            let x = size(obj, 0, 10.0);
            let z = size(obj, 1, x);
            Plane3d::default().mesh().size(x, z).build()
        }
        (Dimension::ThreeD, "cylinder") => {
            let r = size(obj, 0, 0.5);
            let h = size(obj, 1, 1.0);
            Cylinder::new(r, h).into()
        }
        (Dimension::ThreeD, "torus") => {
            let major = size(obj, 0, 1.0);
            let minor = size(obj, 1, 0.25);
            Torus::new(major - minor, major + minor).into()
        }
        (Dimension::ThreeD, "capsule") => {
            let r = size(obj, 0, 0.5);
            let len = size(obj, 1, 1.0);
            Capsule3d::new(r, len).into()
        }
        (Dimension::ThreeD, "cone") => {
            let r = size(obj, 0, 0.5);
            let h = size(obj, 1, 1.0);
            Cone {
                radius: r,
                height: h,
            }
            .into()
        }
        _ => return Err(format!("unknown mesh {kind:?}")),
    };
    let lift = anchor_lift(obj, dim);
    Ok(if lift != 0.0 {
        mesh.translated_by(Vec3::Y * lift)
    } else {
        mesh
    })
}

/// How far `anchor = "bottom"` raises a primitive: half its height, so its
/// base sits on the origin (0 for a centered one). The mesh and its collider
/// both move by exactly this.
fn anchor_lift(obj: &ObjectDef, dim: Dimension) -> f32 {
    if obj.anchor != Anchor::Bottom {
        return 0.0;
    }
    let height = match (dim, obj.mesh.as_deref().unwrap_or("")) {
        (Dimension::TwoD, "circle") => 2.0 * size(obj, 0, 50.0),
        (Dimension::TwoD, "rect") => size(obj, 1, size(obj, 0, 100.0)),
        (Dimension::TwoD, "ring") => 2.0 * size(obj, 0, 100.0),
        (Dimension::ThreeD, "sphere") => 2.0 * size(obj, 0, 1.0),
        (Dimension::ThreeD, "cube") => size(obj, 1, size(obj, 0, 1.0)),
        (Dimension::ThreeD, "cylinder" | "cone") => size(obj, 1, 1.0),
        (Dimension::ThreeD, "capsule") => size(obj, 1, 1.0) + 2.0 * size(obj, 0, 0.5),
        _ => 0.0,
    };
    height * 0.5
}

/// Everything [`build_mesh`] reads, so objects share a mesh only when theirs
/// would come out identical. Cubes bake their atlas tile into the UVs.
fn mesh_key(obj: &ObjectDef, tile: Option<u32>) -> String {
    let atlas = match obj.mesh.as_deref() {
        Some("cube") => tile
            .or(obj.material.atlas_tile)
            .map(|t| (t, obj.material.atlas_columns, obj.material.atlas_rows)),
        _ => None,
    };
    format!("{:?}/{:?}/{:?}/{atlas:?}", obj.mesh, obj.size, obj.anchor)
}

/// A 2D object's collider: the drawn shape, raised by the same anchor lift
/// as its mesh, so balls bounce off what is on screen.
fn collider_2d(obj: &ObjectDef, c: &ColliderDef) -> Collider {
    let shape = match obj.mesh.as_deref() {
        Some("rect") => Collider::rectangle(size(obj, 0, 100.0), size(obj, 1, size(obj, 0, 100.0))),
        _ => Collider::circle(c.radius.unwrap_or_else(|| size(obj, 0, 50.0))),
    };
    let lift = anchor_lift(obj, Dimension::TwoD);
    if lift != 0.0 {
        Collider::compound(vec![(Vec2::Y * lift, Rotation::IDENTITY, shape)])
    } else {
        shape
    }
}

fn material_key(m: &MaterialDef, tile: Option<u32>) -> String {
    format!("{m:?}/{tile:?}")
}

fn material_2d(
    world: &mut World,
    m: &MaterialDef,
    shaders: &Shaders,
    ctx: &SpawnContext,
) -> Result<(Mat2, MaterialLinkBase), String> {
    let base = color(&m.color)?.with_alpha(m.alpha.clamp(0.0, 1.0));
    let texture = m
        .texture
        .as_deref()
        .map(|t| load_image(world, ctx, t, m.pixelated))
        .transpose()?;
    let lin = base.to_linear().to_vec4();
    let link = MaterialLinkBase {
        color: lin,
        emissive: LinearRgba::BLACK,
        params: params(&m.params),
    };
    if let Some(name) = &m.shader {
        let shader = shaders
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown shader {name:?}"))?;
        let material = FxMaterial {
            uniform: FxUniform {
                color: lin,
                params: link.params,
                ..FxUniform::default()
            },
            texture,
            shader,
            blend: fx_blend(m.blend),
            live: true,
        };
        let handle = world.resource_mut::<Assets<FxMaterial>>().add(material);
        return Ok((Mat2::Fx(handle), link));
    }
    let material = ColorMaterial {
        color: base,
        texture,
        alpha_mode: match m.blend {
            BlendDef::Opaque => AlphaMode2d::Opaque,
            BlendDef::Mask => AlphaMode2d::Mask(0.5),
            _ => AlphaMode2d::Blend,
        },
        ..default()
    };
    let handle = world.resource_mut::<Assets<ColorMaterial>>().add(material);
    Ok((Mat2::Color(handle), link))
}

fn material_3d(
    world: &mut World,
    m: &MaterialDef,
    shaders: &Shaders,
    ctx: &SpawnContext,
) -> Result<(Mat3, MaterialLinkBase), String> {
    let alpha = m.alpha.clamp(0.0, 1.0);
    let base = color(&m.color)?.with_alpha(alpha);
    let texture = m
        .texture
        .as_deref()
        .map(|t| load_image(world, ctx, t, m.pixelated))
        .transpose()?;
    let emissive = match &m.emissive {
        Some(e) => color(e)?.to_linear() * m.emissive_strength,
        None if m.emissive_texture => LinearRgba::WHITE * m.emissive_strength,
        None => LinearRgba::BLACK,
    };
    let link = MaterialLinkBase {
        color: base.to_linear().to_vec4(),
        emissive,
        params: params(&m.params),
    };
    if let Some(name) = &m.shader {
        let shader = shaders
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown shader {name:?}"))?;
        let material = FxMaterial3d {
            uniform: FxUniform {
                color: link.color,
                params: link.params,
                ..FxUniform::default()
            },
            texture,
            shader,
            blend: fx_blend(m.blend),
            double_sided: m.double_sided,
        };
        let handle = world.resource_mut::<Assets<FxMaterial3d>>().add(material);
        return Ok((Mat3::Fx(handle), link));
    }
    let material = StandardMaterial {
        base_color: base,
        base_color_texture: texture.clone(),
        emissive,
        emissive_texture: if m.emissive_texture { texture } else { None },
        unlit: m.unlit,
        perceptual_roughness: m.roughness.clamp(0.0, 1.0),
        metallic: m.metallic.clamp(0.0, 1.0),
        double_sided: m.double_sided,
        cull_mode: if m.double_sided {
            None
        } else {
            Some(bevy::render::render_resource::Face::Back)
        },
        alpha_mode: match m.blend {
            BlendDef::Additive => AlphaMode::Add,
            BlendDef::Mask => AlphaMode::Mask(0.5),
            BlendDef::Opaque => AlphaMode::Opaque,
            BlendDef::Alpha if alpha < 1.0 => AlphaMode::Blend,
            BlendDef::Alpha => AlphaMode::Opaque,
        },
        ..default()
    };
    let handle = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(material);
    Ok((Mat3::Standard(handle), link))
}

/// Reactions from the def, with `band = "bar"` bound to this instance's bar.
fn bindings(obj: &ObjectDef, bar: Option<f32>) -> Result<Vec<Binding>, String> {
    obj.react
        .iter()
        .map(|r| {
            let band = match (r.band.trim(), bar) {
                ("bar", Some(f)) => BandRef::Fraction(f),
                ("bar", None) => return Err("band = \"bar\" only works inside a [[ring]]".into()),
                (name, _) => BandRef::Named(name.to_string()),
            };
            Ok(Binding {
                property: Property::parse(&r.property)?,
                band,
                amount: r.amount,
                smooth: r.smooth.max(0.0),
                level: 0.0,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn spawn_object(
    world: &mut World,
    def: &SceneDef,
    obj: &ObjectDef,
    index: usize,
    shaders: &Shaders,
    ctx: &SpawnContext,
    names: &HashMap<String, Entity>,
    cache: &mut MeshCache,
    instance: Instance,
) -> Result<Entity, String> {
    let dim = def.scene.dimension;
    let [rx, ry, rz] = obj.rotation.map(f32::to_radians);
    let base = Transform {
        translation: Vec3::from(obj.position),
        rotation: Quat::from_euler(EulerRot::XYZ, rx, ry, rz),
        scale: Vec3::from(obj.scale.to_array()),
    };
    let bindings = bindings(obj, instance.bar)?;
    let drives_material = bindings
        .iter()
        .any(|b| matches!(b.property, Property::Brightness | Property::Param(..)));

    let orbit = match (&instance.orbit, &obj.orbit) {
        (Some(o), _) => Some(o.clone()),
        (None, Some(o)) => {
            let parent = match &o.parent {
                Some(p) => Some(
                    *names
                        .get(p)
                        .ok_or_else(|| format!("unknown parent {p:?}"))?,
                ),
                None => None,
            };
            let [ax, ay] = o.radius.axes();
            let plane = if dim == Dimension::ThreeD {
                Quat::from_rotation_x(o.tilt[0].to_radians())
                    * Quat::from_rotation_z(o.tilt[1].to_radians())
            } else {
                Quat::IDENTITY
            };
            Some(Orbit {
                parent,
                center: Vec3::from(o.center),
                axes: Vec2::new(ax, ay),
                angular: if o.period.abs() > 1e-3 {
                    TAU / o.period
                } else {
                    0.0
                },
                angle: o.phase * TAU,
                plane,
                three_d: dim == Dimension::ThreeD,
                speed_band: o.speed_band.as_ref().map(|b| BandRef::Named(b.clone())),
                speed_react: o.speed_react,
                face_out: false,
                behind_z: o.behind_z,
                perspective: o.perspective,
            })
        }
        (None, None) => None,
    };
    let spin = obj.spin.as_ref().map(|s| Spin {
        axis: if dim == Dimension::TwoD {
            Vec3::Z
        } else {
            Vec3::from(s.axis).try_normalize().unwrap_or(Vec3::Y)
        },
        speed: s.speed.to_radians(),
        angle: 0.0,
    });

    let mut entity = world.spawn((SceneEntity, SceneObject { index, base }, base));
    if let Some(o) = orbit {
        entity.insert(o);
    }
    if let Some(s) = spin {
        entity.insert(s);
    }
    if !bindings.is_empty() {
        entity.insert(Reactive { bindings });
    }
    let id = entity.id();

    if let Some(model) = &obj.model {
        let path = files::asset_path(&ctx.slot, model)?;
        let server = world.resource::<AssetServer>().clone();
        let root = server.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
        track(world, root.id());
        world.entity_mut(id).insert(WorldAssetRoot(root));
        if let Some(clip) = obj.animation {
            let clip: Handle<AnimationClip> =
                server.load(GltfAssetLabel::Animation(clip).from_asset(path));
            track(world, clip.id());
            let (graph, node) = AnimationGraph::from_clip(clip);
            let graph = world.resource_mut::<Assets<AnimationGraph>>().add(graph);
            world.entity_mut(id).insert(ModelAnimation { graph, node });
        }
        return Ok(id);
    }

    let mesh = cached(
        &mut cache.meshes,
        mesh_key(obj, instance.tile),
        false,
        || {
            let mesh = build_mesh(obj, dim, instance.tile)?;
            Ok(world.resource_mut::<Assets<Mesh>>().add(mesh))
        },
    )?;

    // Instances whose own reactions write their material need their own copy;
    // everyone else shares.
    let mkey = material_key(&obj.material, instance.tile);
    match dim {
        Dimension::TwoD => {
            let (mat, link) = cached(&mut cache.materials_2d, mkey, drives_material, || {
                material_2d(world, &obj.material, shaders, ctx)
            })?;
            let mut e = world.entity_mut(id);
            e.insert(Mesh2d(mesh));
            match mat {
                Mat2::Fx(h) => {
                    if drives_material {
                        e.insert(MaterialLink::Fx2d {
                            handle: h.clone(),
                            color: link.color,
                            params: link.params,
                        });
                    }
                    e.insert(MeshMaterial2d(h));
                }
                Mat2::Color(h) => {
                    if drives_material {
                        e.insert(MaterialLink::Color2d {
                            handle: h.clone(),
                            color: LinearRgba::from_vec4(link.color),
                        });
                    }
                    e.insert(MeshMaterial2d(h));
                }
            }
            if let Some(c) = &obj.collider {
                e.insert((
                    RigidBody::Kinematic,
                    collider_2d(obj, c),
                    Restitution::new(c.restitution),
                    LinearVelocity::ZERO,
                ));
            }
        }
        Dimension::ThreeD => {
            let (mat, link) = cached(&mut cache.materials_3d, mkey, drives_material, || {
                material_3d(world, &obj.material, shaders, ctx)
            })?;
            let mut e = world.entity_mut(id);
            e.insert(Mesh3d(mesh));
            match mat {
                Mat3::Fx(h) => {
                    if drives_material {
                        e.insert(MaterialLink::Fx3d {
                            handle: h.clone(),
                            color: link.color,
                            params: link.params,
                        });
                    }
                    e.insert((MeshMaterial3d(h), NotShadowCaster));
                }
                Mat3::Standard(h) => {
                    if drives_material {
                        e.insert(MaterialLink::Standard {
                            handle: h.clone(),
                            color: LinearRgba::from_vec4(link.color),
                            emissive: link.emissive,
                        });
                    }
                    e.insert(MeshMaterial3d(h));
                }
            }
        }
    }
    Ok(id)
}

/// A faint ring tracing a 2D orbit (following its parent, if any).
fn spawn_orbit_path(
    world: &mut World,
    obj: &ObjectDef,
    orbit: &super::def::OrbitDef,
    index: usize,
    names: &HashMap<String, Entity>,
) -> Result<(), String> {
    let [ax, ay] = orbit.radius.axes();
    let r = ax.max(1.0);
    let w = orbit.path_width;
    let mesh = world.resource_mut::<Assets<Mesh>>().add(
        Annulus::new((r - w * 0.5).max(0.0), r + w * 0.5)
            .mesh()
            .resolution(192)
            .build(),
    );
    let material = world
        .resource_mut::<Assets<ColorMaterial>>()
        .add(ColorMaterial {
            color: color(&orbit.path_color)?,
            alpha_mode: AlphaMode2d::Blend,
            ..default()
        });
    let z = obj.position[2] - 0.5;
    let parent = orbit.parent.as_ref().and_then(|p| names.get(p).copied());
    let base = Transform {
        translation: if parent.is_some() {
            Vec3::new(0.0, 0.0, z)
        } else {
            Vec3::new(orbit.center[0], orbit.center[1], z)
        },
        scale: Vec3::new(1.0, ay / r, 1.0),
        ..default()
    };
    let mut e = world.spawn((
        SceneEntity,
        SceneObject { index, base },
        base,
        Mesh2d(mesh),
        MeshMaterial2d(material),
    ));
    if parent.is_some() {
        e.insert(Orbit {
            parent,
            center: Vec3::ZERO,
            axes: Vec2::ZERO,
            angular: 0.0,
            angle: 0.0,
            plane: Quat::IDENTITY,
            three_d: false,
            speed_band: None,
            speed_react: 0.0,
            face_out: false,
            behind_z: None,
            perspective: 0.0,
        });
    }
    Ok(())
}

fn spawn_camera(world: &mut World, def: &SceneDef, shaders: &Shaders) -> Result<(), String> {
    let c = &def.camera;
    let look_at = Vec3::from(c.look_at);
    let offset = Vec3::from(c.position) - look_at;
    let (msaa, target) = {
        let mut q = world.query_filtered::<(&Msaa, Option<&RenderTarget>), With<VisCamera>>();
        q.iter(world)
            .next()
            .map(|(m, t)| (*m, t.cloned()))
            .unwrap_or((Msaa::Sample4, None))
    };
    let env = &def.environment;
    let clear = world.resource::<ClearColor>().0;
    let ambient = AmbientLight {
        color: color(&env.ambient_color)?,
        brightness: env.ambient_brightness,
        ..default()
    };
    let mut cam = world.spawn((
        SceneEntity,
        SceneCamera3d,
        Camera3d::default(),
        Camera {
            // Under the vis camera, which composites the 2D layer and the HUD
            // on top and applies bloom + tone mapping once for both.
            order: -1,
            clear_color: ClearColorConfig::Custom(clear),
            ..default()
        },
        Hdr,
        msaa,
        // The vis camera tone-maps the shared HDR target; doing it here too
        // would tone-map the 3D view twice.
        Tonemapping::None,
        Projection::Perspective(PerspectiveProjection {
            fov: c.fov.to_radians(),
            far: 2000.0,
            ..default()
        }),
        Transform::from_translation(look_at + offset).looking_at(look_at, Vec3::Y),
        SceneCamera {
            look_at,
            offset,
            orbit: c.orbit,
            angle: 0.0,
            bob: c.bob,
            fov: c.fov,
            punch: c.punch,
        },
        ambient,
    ));
    if let Some(target) = target {
        cam.insert(target);
    }
    if let Some(fog) = &env.fog_color {
        cam.insert(DistanceFog {
            color: color(fog)?,
            falloff: FogFalloff::Linear {
                start: env.fog_start,
                end: env.fog_end.max(env.fog_start + 1.0),
            },
            ..default()
        });
    }

    // The vis camera now draws over the 3D view instead of clearing it.
    let mut q = world.query_filtered::<&mut Camera, With<VisCamera>>();
    for mut cam in q.iter_mut(world) {
        cam.clear_color = ClearColorConfig::None;
    }

    if let Some(sky) = &env.sky_shader {
        let shader = shaders
            .get(sky)
            .cloned()
            .ok_or_else(|| format!("unknown sky shader {sky:?}"))?;
        let radius = 900.0;
        let mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Sphere::new(radius).mesh().uv(64, 32));
        let material = world
            .resource_mut::<Assets<FxMaterial3d>>()
            .add(FxMaterial3d {
                uniform: FxUniform {
                    params: params(&env.sky_params),
                    ..FxUniform::default()
                },
                texture: None,
                shader,
                blend: FxBlend::Opaque,
                double_sided: true,
            });
        world.spawn((
            SceneEntity,
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::default(),
            NotShadowCaster,
            NotShadowReceiver,
            NoFrustumCulling,
        ));
    }
    Ok(())
}

fn spawn_lights(world: &mut World, def: &SceneDef) -> Result<(), String> {
    for (i, l) in def.lights.iter().enumerate() {
        let c = color(&l.color).map_err(|e| format!("[[light]] #{}: {e}", i + 1))?;
        let transform = Transform::from_translation(Vec3::from(l.position))
            .looking_at(Vec3::from(l.look_at), Vec3::Y);
        let bindings = l
            .react
            .iter()
            .map(|r| Binding {
                property: Property::Brightness,
                band: BandRef::Named(r.band.clone()),
                amount: r.amount,
                smooth: r.smooth.max(0.0),
                level: 0.0,
            })
            .collect();
        let light = SceneLight {
            base: l.intensity,
            bindings,
        };
        match l.kind {
            LightKind::Directional => {
                world.spawn((
                    SceneEntity,
                    DirectionalLight {
                        illuminance: l.intensity,
                        color: c,
                        shadow_maps_enabled: l.shadows,
                        ..default()
                    },
                    transform,
                    light,
                ));
            }
            LightKind::Point => {
                world.spawn((
                    SceneEntity,
                    PointLight {
                        intensity: l.intensity,
                        range: l.range,
                        color: c,
                        shadow_maps_enabled: l.shadows,
                        ..default()
                    },
                    transform,
                    light,
                ));
            }
        }
    }
    Ok(())
}

fn spawn_terrain(
    world: &mut World,
    t: &super::def::TerrainDef,
    ctx: &SpawnContext,
) -> Result<(), String> {
    let atlas = load_image(world, ctx, &t.atlas, true)?;
    let (solid_mesh, glow_mesh) = {
        let mut meshes = world.resource_mut::<Assets<Mesh>>();
        (
            meshes.add(MeshData::default().to_mesh()),
            meshes.add(MeshData::default().to_mesh()),
        )
    };
    let (solid_mat, glow_mat) = {
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        let solid = materials.add(StandardMaterial {
            base_color_texture: Some(atlas.clone()),
            perceptual_roughness: 0.95,
            reflectance: 0.1,
            alpha_mode: AlphaMode::Mask(0.5),
            ..default()
        });
        let glow = materials.add(StandardMaterial {
            base_color_texture: Some(atlas.clone()),
            emissive_texture: Some(atlas),
            emissive: LinearRgba::rgb(t.ore_glow, t.ore_glow, t.ore_glow),
            perceptual_roughness: 0.6,
            ..default()
        });
        (solid, glow)
    };
    // The geometry is rewritten in place every frame, so its bounds are never
    // recomputed: skip frustum culling rather than cull against stale bounds.
    world.spawn((
        SceneEntity,
        Mesh3d(solid_mesh.clone()),
        MeshMaterial3d(solid_mat),
        Transform::default(),
        NoFrustumCulling,
    ));
    world.spawn((
        SceneEntity,
        Mesh3d(glow_mesh.clone()),
        MeshMaterial3d(glow_mat.clone()),
        Transform::default(),
        NoFrustumCulling,
    ));
    world.spawn((
        SceneEntity,
        Terrain::new(t.clone(), solid_mesh, glow_mesh, glow_mat),
    ));
    Ok(())
}

fn load_sounds(world: &mut World, def: &SceneDef, ctx: &SpawnContext) -> Result<(), String> {
    let offline = world.resource::<SceneRuntime>().offline;
    let server = world.resource::<AssetServer>().clone();
    let mut sounds = Vec::new();
    for (name, s) in &def.sounds {
        let path =
            files::asset_path(&ctx.slot, &s.path).map_err(|e| format!("[sounds.{name}]: {e}"))?;
        sounds.push(SceneSound::new(s.clone(), server.load(path)));
    }
    let mut res = world.resource_mut::<SceneSounds>();
    res.sounds = sounds;
    res.enabled = !offline;
    Ok(())
}

/// A glTF model's looping animation, started once its scene instance exists.
#[derive(Component, Clone)]
pub(crate) struct ModelAnimation {
    graph: Handle<AnimationGraph>,
    node: AnimationNodeIndex,
}

/// Marks an animation player driven by a scene model (its speed follows the
/// music).
#[derive(Component)]
pub(crate) struct ModelPlayer;

/// Start a model's animation on every player in its freshly spawned instance.
pub(crate) fn start_model_animation(
    ready: On<WorldInstanceReady>,
    roots: Query<&ModelAnimation>,
    children: Query<&Children>,
    mut players: Query<&mut AnimationPlayer>,
    mut commands: Commands,
) {
    let Ok(anim) = roots.get(ready.entity) else {
        return;
    };
    for e in children.iter_descendants(ready.entity) {
        if let Ok(mut player) = players.get_mut(e) {
            player.play(anim.node).repeat();
            commands
                .entity(e)
                .insert((AnimationGraphHandle(anim.graph.clone()), ModelPlayer));
        }
    }
}

/// Dance faster when it's louder.
pub(crate) fn drive_model_animation(
    features: Res<crate::vis::features::AudioFeatures>,
    mut players: Query<&mut AnimationPlayer, With<ModelPlayer>>,
) {
    let speed = 0.6 + 1.4 * features.energy.min(1.2) + 0.6 * features.beat_pulse;
    for mut player in &mut players {
        for (_, active) in player.playing_animations_mut() {
            active.set_speed(speed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meshes_build_for_every_primitive() {
        for (dim, kinds) in [
            (Dimension::TwoD, &["circle", "rect", "ring"][..]),
            (
                Dimension::ThreeD,
                &[
                    "sphere", "cube", "plane", "cylinder", "torus", "capsule", "cone",
                ][..],
            ),
        ] {
            for kind in kinds {
                let obj = ObjectDef {
                    mesh: Some(kind.to_string()),
                    ..ObjectDef::default()
                };
                let mesh = build_mesh(&obj, dim, None).unwrap_or_else(|e| panic!("{kind}: {e}"));
                assert!(mesh.count_vertices() > 0, "{kind}");
            }
        }
    }

    #[test]
    fn bottom_anchor_puts_the_base_at_the_origin() {
        let obj = ObjectDef {
            mesh: Some("rect".into()),
            size: vec![10.0, 40.0],
            anchor: Anchor::Bottom,
            ..ObjectDef::default()
        };
        let mesh = build_mesh(&obj, Dimension::TwoD, None).unwrap();
        let ys: Vec<f32> = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| a.as_float3())
            .unwrap()
            .iter()
            .map(|p| p[1])
            .collect();
        let min = ys.iter().copied().fold(f32::INFINITY, f32::min);
        let max = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            min.abs() < 1e-4 && (max - 40.0).abs() < 1e-4,
            "{min}..{max}"
        );
    }

    #[test]
    fn bar_bands_need_a_ring() {
        let obj = ObjectDef {
            mesh: Some("rect".into()),
            react: vec![super::super::def::ReactDef {
                band: "bar".into(),
                ..Default::default()
            }],
            ..ObjectDef::default()
        };
        assert!(bindings(&obj, None).is_err());
        let b = bindings(&obj, Some(0.25)).unwrap();
        assert_eq!(b[0].band, BandRef::Fraction(0.25));
    }

    fn cube(tile: Option<u32>) -> ObjectDef {
        ObjectDef {
            mesh: Some("cube".into()),
            material: MaterialDef {
                atlas_tile: tile,
                ..MaterialDef::default()
            },
            ..ObjectDef::default()
        }
    }

    #[test]
    fn cubes_on_different_atlas_tiles_do_not_share_a_mesh() {
        // The Minecraft scene's planks (11) and glowstone lamps (14).
        assert_ne!(
            mesh_key(&cube(Some(11)), None),
            mesh_key(&cube(Some(14)), None)
        );
        assert_eq!(
            mesh_key(&cube(Some(11)), None),
            mesh_key(&cube(Some(11)), None)
        );
        // A ring's tile replaces the template's.
        assert_eq!(
            mesh_key(&cube(Some(11)), Some(14)),
            mesh_key(&cube(Some(14)), None)
        );
        let mut grid = cube(Some(11));
        grid.material.atlas_columns = 16;
        assert_ne!(mesh_key(&grid, None), mesh_key(&cube(Some(11)), None));
        // Only cubes read the atlas.
        let sphere = |tile| ObjectDef {
            mesh: Some("sphere".into()),
            ..cube(tile)
        };
        assert_eq!(
            mesh_key(&sphere(Some(11)), None),
            mesh_key(&sphere(Some(14)), None)
        );
    }

    fn attribute(mesh: &Mesh, id: bevy::mesh::MeshVertexAttribute) -> Vec<f32> {
        use bevy::mesh::VertexAttributeValues as V;
        match mesh.attribute(id) {
            Some(V::Float32x2(v)) => v.iter().flatten().copied().collect(),
            Some(V::Float32x3(v)) => v.iter().flatten().copied().collect(),
            other => panic!("unexpected attribute {other:?}"),
        }
    }

    /// The cache's promise, over every built-in scene: whatever shares a mesh
    /// key builds the very same mesh.
    #[test]
    fn built_in_objects_sharing_a_mesh_key_build_the_same_mesh() {
        for id in files::builtin_ids() {
            let f = files::read(&files::SceneSource::Builtin(id)).unwrap();
            let def = SceneDef::parse(f.scene_toml().unwrap()).unwrap();
            let dim = def.scene.dimension;
            let mut made: Vec<(&ObjectDef, Option<u32>)> = def
                .objects
                .iter()
                .filter(|o| o.model.is_none())
                .map(|o| (o, None))
                .collect();
            for ring in def.rings.iter().filter(|r| r.template.model.is_none()) {
                made.push((&ring.template, None));
                made.extend(ring.tiles.iter().map(|&t| (&ring.template, Some(t))));
            }
            let mut first: HashMap<String, Mesh> = HashMap::new();
            for (obj, tile) in made {
                let mesh = build_mesh(obj, dim, tile).unwrap();
                match first.get(&mesh_key(obj, tile)) {
                    Some(seen) => {
                        for a in [Mesh::ATTRIBUTE_POSITION, Mesh::ATTRIBUTE_UV_0] {
                            assert_eq!(
                                attribute(seen, a),
                                attribute(&mesh, a),
                                "{id}: {:?} shares a mesh it does not match",
                                obj.name
                            );
                        }
                    }
                    None => {
                        first.insert(mesh_key(obj, tile), mesh);
                    }
                }
            }
        }
    }

    #[test]
    fn reacting_materials_stay_out_of_the_shared_cache() {
        let mut cache = HashMap::new();
        let key = || "white".to_string();
        // An object whose reactions pulse its material gets its own…
        assert_eq!(cached(&mut cache, key(), true, || Ok(1)), Ok(1));
        // …and a plain one declared after it must not be handed that one.
        assert_eq!(cached(&mut cache, key(), false, || Ok(2)), Ok(2));
        assert_eq!(cached(&mut cache, key(), false, || Ok(3)), Ok(2));
        assert_eq!(cached(&mut cache, key(), true, || Ok(4)), Ok(4));
    }

    /// `(min, max)` of a mesh's vertices in x/y.
    fn bounds(mesh: &Mesh) -> (Vec2, Vec2) {
        let points: Vec<Vec2> = attribute(mesh, Mesh::ATTRIBUTE_POSITION)
            .chunks(3)
            .map(|p| Vec2::new(p[0], p[1]))
            .collect();
        let min = points.iter().copied().fold(Vec2::INFINITY, Vec2::min);
        let max = points.iter().copied().fold(Vec2::NEG_INFINITY, Vec2::max);
        (min, max)
    }

    #[test]
    fn colliders_cover_the_drawn_mesh_whatever_the_anchor() {
        use avian2d::prelude::SimpleCollider;
        for (kind, size) in [("rect", vec![20.0, 200.0]), ("circle", vec![30.0])] {
            for anchor in [Anchor::Center, Anchor::Bottom] {
                let obj = ObjectDef {
                    mesh: Some(kind.into()),
                    size: size.clone(),
                    anchor,
                    ..ObjectDef::default()
                };
                let (lo, hi) = bounds(&build_mesh(&obj, Dimension::TwoD, None).unwrap());
                let aabb = collider_2d(&obj, &ColliderDef::default()).aabb(Vec2::ZERO, 0.0);
                assert!(
                    (aabb.min - lo).length() < 1e-3 && (aabb.max - hi).length() < 1e-3,
                    "{kind} {anchor:?}: collider {:?}..{:?}, mesh {lo}..{hi}",
                    aabb.min,
                    aabb.max
                );
            }
        }
    }
}
