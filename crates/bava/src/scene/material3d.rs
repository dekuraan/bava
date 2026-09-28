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
use crate::vis::features::{AudioFeatures, FeaturesSet};
use crate::vis::fx::material::{
    ADDITIVE_BLEND, ADDITIVE_SHADER, FxBlend, FxSyncSet, FxUniform, LiveInputs,
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
    blend: FxBlend,
    /// Alpha-blended, but the material color is fully opaque (`alpha = 1`):
    /// keep writing depth (see its `alpha_mode`).
    depth_write: bool,
    double_sided: bool,
}

impl From<&FxMaterial3d> for FxMaterial3dKey {
    fn from(m: &FxMaterial3d) -> Self {
        Self {
            shader: m.shader.clone(),
            blend: m.blend,
            depth_write: m.blend == FxBlend::Alpha && m.uniform.color.w >= 1.0,
            double_sided: m.double_sided,
        }
    }
}

impl Material for FxMaterial3d {
    fn fragment_shader() -> ShaderRef {
        // Replaced per instance in `specialize`.
        ShaderRef::Handle(ADDITIVE_SHADER)
    }

    /// `Alpha` stays in the transparent (sorted) phase even at `alpha = 1`,
    /// unlike a plain `StandardMaterial`: an effect shader can make its own
    /// alpha (the built-in Blockland water does), and in the opaque phase that
    /// would blend against whatever happened to be drawn first. At `alpha = 1`
    /// it writes depth instead (`depth_write` in the key), so an opaque-looking
    /// torus hides its own far side and intersecting effect objects occlude
    /// per pixel rather than per object.
    fn alpha_mode(&self) -> AlphaMode {
        match self.blend {
            FxBlend::Opaque => AlphaMode::Opaque,
            FxBlend::Alpha => AlphaMode::Blend,
            // Transparent phase without depth writes. Bevy's `Add` is really
            // premultiplied-over — additive only for a shader that outputs
            // alpha 0, as `StandardMaterial`'s does — so `specialize` swaps in
            // a true additive blend state, like the 2D material.
            FxBlend::Additive => AlphaMode::Add,
        }
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        apply_key(descriptor, &key.bind_group_data);
        Ok(())
    }
}

/// Put the key's shader, blend state, depth writes and culling into the
/// pipeline Bevy built for the material's [`AlphaMode`].
fn apply_key(descriptor: &mut RenderPipelineDescriptor, key: &FxMaterial3dKey) {
    if let Some(fragment) = descriptor.fragment.as_mut() {
        fragment.shader = key.shader.clone();
        if key.blend == FxBlend::Additive {
            for target in fragment.targets.iter_mut().flatten() {
                target.blend = Some(ADDITIVE_BLEND);
            }
        }
    }
    if key.depth_write
        && let Some(depth) = descriptor.depth_stencil.as_mut()
    {
        depth.depth_write_enabled = Some(true);
    }
    if key.double_sided {
        descriptor.primitive.cull_mode = None;
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
    let live = LiveInputs::new(&features, &vis, &windows);
    for (_, material) in materials.iter_mut() {
        material.uniform = live.applied(material.uniform);
    }
}

pub struct FxMaterial3dPlugin;

impl Plugin for FxMaterial3dPlugin {
    fn build(&self, app: &mut App) {
        // After `FeaturesSet`, like the 2D sync: otherwise the executor may run
        // it first and hand 3D shaders last frame's audio (and an offline
        // render would no longer be deterministic).
        app.add_plugins(MaterialPlugin::<FxMaterial3d>::default())
            .add_systems(
                Update,
                sync_fx_materials_3d.in_set(FxSyncSet).after(FeaturesSet),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vis::fx::material::HALO_SHADER;
    use bevy::render::render_resource::{
        BlendState, ColorTargetState, ColorWrites, CompareFunction, DepthStencilState, Face,
        FragmentState, TextureFormat,
    };

    fn material(blend: FxBlend, alpha: f32) -> FxMaterial3d {
        FxMaterial3d {
            uniform: FxUniform {
                color: Vec4::new(1.0, 1.0, 1.0, alpha),
                ..FxUniform::default()
            },
            texture: None,
            shader: HALO_SHADER,
            blend,
            double_sided: false,
        }
    }

    /// What Bevy's mesh pipeline hands `specialize` for a transparent-phase
    /// material: blending on, depth test on, depth writes off, back faces culled.
    fn transparent_descriptor(blend: BlendState) -> RenderPipelineDescriptor {
        let mut d = RenderPipelineDescriptor {
            fragment: Some(FragmentState {
                shader: ADDITIVE_SHADER,
                targets: vec![Some(ColorTargetState {
                    format: TextureFormat::Rgba16Float,
                    blend: Some(blend),
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            depth_stencil: Some(DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(CompareFunction::GreaterEqual),
                stencil: default(),
                bias: default(),
            }),
            ..default()
        };
        d.primitive.cull_mode = Some(Face::Back);
        d
    }

    fn specialized(m: &FxMaterial3d, bevy_blend: BlendState) -> RenderPipelineDescriptor {
        let mut d = transparent_descriptor(bevy_blend);
        apply_key(&mut d, &FxMaterial3dKey::from(m));
        d
    }

    fn target_blend(d: &RenderPipelineDescriptor) -> Option<BlendState> {
        d.fragment.as_ref().unwrap().targets[0]
            .as_ref()
            .unwrap()
            .blend
    }

    fn writes_depth(d: &RenderPipelineDescriptor) -> Option<bool> {
        d.depth_stencil.as_ref().unwrap().depth_write_enabled
    }

    #[test]
    fn additive_installs_a_true_additive_blend() {
        let m = material(FxBlend::Additive, 1.0);
        assert_eq!(m.alpha_mode(), AlphaMode::Add);
        // `AlphaMode::Add` arrives as premultiplied-over, which would make an
        // alpha-1 glow shader an opaque occluder.
        let d = specialized(&m, BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        assert_eq!(target_blend(&d), Some(ADDITIVE_BLEND));
        assert_eq!(writes_depth(&d), Some(false), "glows must not occlude");
        assert_eq!(d.fragment.unwrap().shader, HALO_SHADER);
    }

    #[test]
    fn opaque_alpha_blend_writes_depth_but_keeps_blending() {
        let m = material(FxBlend::Alpha, 1.0);
        assert_eq!(m.alpha_mode(), AlphaMode::Blend);
        let d = specialized(&m, BlendState::ALPHA_BLENDING);
        assert_eq!(writes_depth(&d), Some(true));
        // Still blended: a shader-made alpha (the Blockland water) stays
        // translucent.
        assert_eq!(target_blend(&d), Some(BlendState::ALPHA_BLENDING));
    }

    #[test]
    fn translucent_alpha_blend_leaves_depth_alone() {
        let d = specialized(&material(FxBlend::Alpha, 0.5), BlendState::ALPHA_BLENDING);
        assert_eq!(writes_depth(&d), Some(false));
        assert_eq!(target_blend(&d), Some(BlendState::ALPHA_BLENDING));
    }

    #[test]
    fn key_separates_blend_modes_and_double_sided_disables_culling() {
        let alpha = FxMaterial3dKey::from(&material(FxBlend::Alpha, 1.0));
        let add = FxMaterial3dKey::from(&material(FxBlend::Additive, 1.0));
        let opaque = FxMaterial3dKey::from(&material(FxBlend::Opaque, 1.0));
        assert_ne!(alpha, add);
        assert_ne!(add, opaque);
        assert_ne!(alpha, opaque);

        let mut sky = material(FxBlend::Opaque, 1.0);
        sky.double_sided = true;
        assert_eq!(sky.alpha_mode(), AlphaMode::Opaque);
        let d = specialized(&sky, BlendState::REPLACE);
        assert_eq!(d.primitive.cull_mode, None);
        assert_eq!(target_blend(&d), Some(BlendState::REPLACE));
    }
}
