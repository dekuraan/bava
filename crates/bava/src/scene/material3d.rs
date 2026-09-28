// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`FxMaterial3d`]: the 3D twin of [`FxMaterial`](crate::vis::fx::material::FxMaterial).
//!
//! Same uniform, same bindings, same `#import bava::fx_material` — so one WGSL
//! file can serve a 2D object and a 3D one — with the fragment shader chosen
//! per instance through the `bind_group_data` key, exactly as in 2D. Scene
//! shaders for 3D meshes take `bevy_pbr::forward_io::VertexOutput` as input.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::{Shader, ShaderRef};

use crate::vis::VisSettings;
use crate::vis::features::AudioFeatures;
use crate::vis::fx::material::{
    ADDITIVE_SHADER, FxBlend, FxSyncSet, FxUniform, audio_uniform, palette_uniform,
};

/// A 3D material whose fragment shader, blending and face culling are chosen
/// per instance.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
#[bind_group_data(FxMaterial3dKey)]
pub struct FxMaterial3d {
    #[uniform(0)]
    pub uniform: FxUniform,
    #[texture(1)]
    #[sampler(2)]
    pub texture: Option<Handle<Image>>,
    pub shader: Handle<Shader>,
    pub blend: FxBlend,
    /// Draw back faces too (sky domes, thin shells).
    pub double_sided: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FxMaterial3dKey {
    shader: Handle<Shader>,
    double_sided: bool,
}

impl From<&FxMaterial3d> for FxMaterial3dKey {
    fn from(m: &FxMaterial3d) -> Self {
        Self {
            shader: m.shader.clone(),
            double_sided: m.double_sided,
        }
    }
}

impl Material for FxMaterial3d {
    fn fragment_shader() -> ShaderRef {
        // Replaced per instance in `specialize`.
        ShaderRef::Handle(ADDITIVE_SHADER)
    }

    fn alpha_mode(&self) -> AlphaMode {
        match self.blend {
            FxBlend::Opaque => AlphaMode::Opaque,
            FxBlend::Alpha => AlphaMode::Blend,
            FxBlend::Additive => AlphaMode::Add,
        }
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader = key.bind_group_data.shader.clone();
        }
        if key.bind_group_data.double_sided {
            descriptor.primitive.cull_mode = None;
        }
        Ok(())
    }
}

/// Refresh every [`FxMaterial3d`] with this frame's audio, palette and
/// viewport, like `sync_fx_materials` does for 2D.
fn sync_fx_materials_3d(
    features: Res<AudioFeatures>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    mut materials: ResMut<Assets<FxMaterial3d>>,
) {
    if materials.is_empty() {
        return;
    }
    let (palette, count) = palette_uniform(&vis.fg_stops());
    let (audio, clock) = audio_uniform(&features, count);
    let (w, h) = windows
        .iter()
        .next()
        .map(|w| (w.width(), w.height()))
        .unwrap_or((1280.0, 720.0));
    for (_, material) in materials.iter_mut() {
        let u = &mut material.uniform;
        u.palette = palette;
        u.audio = audio;
        u.clock = clock;
        u.info.x = w;
        u.info.y = h;
    }
}

pub struct FxMaterial3dPlugin;

impl Plugin for FxMaterial3dPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<FxMaterial3d>::default())
            .add_systems(Update, sync_fx_materials_3d.in_set(FxSyncSet));
    }
}
