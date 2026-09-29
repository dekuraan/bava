// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The bindings of bava's effect material (`FxMaterial` in 2D, `FxMaterial3d`
// in 3D). Import these instead of declaring them, so a shader works unchanged
// on either pipeline:
//
//   #import bava::fx_material::{fx, fx_texture, fx_sampler}
//
// `fx_texture` is the scene material's `texture = "..."` (a 1×1 white image
// when none is set).

#define_import_path bava::fx_material

#import bava::fx::{FxUniform, PI, TAU}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> fx: FxUniform;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var fx_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var fx_sampler: sampler;

// Sample the live gradient (`fx.palette`, `fx.clock.w` stops) at `t` in 0..1.
fn palette(t: f32) -> vec3<f32> {
    let count = clamp(i32(fx.clock.w), 1, 4);
    if count == 1 {
        return fx.palette[0].rgb;
    }
    let x = clamp(t, 0.0, 1.0) * f32(count - 1);
    let i = min(i32(floor(x)), count - 2);
    return mix(fx.palette[i].rgb, fx.palette[i + 1].rgb, x - f32(i));
}

// The blob rim radius (px) in world direction `theta`, interpolated from the
// 64 samples in `fx.shape`. Matches the geometry the ring stroke is drawn from.
fn rim_radius(theta: f32) -> f32 {
    let t = fract((theta + PI * 0.5 - fx.info.w) / TAU);
    let x = t * 64.0;
    let i0 = u32(floor(x)) % 64u;
    let i1 = (i0 + 1u) % 64u;
    let a = fx.shape[i0 / 4u][i0 % 4u];
    let b = fx.shape[i1 / 4u][i1 % 4u];
    return mix(a, b, fract(x));
}
