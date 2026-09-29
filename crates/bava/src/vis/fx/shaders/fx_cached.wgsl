// SPDX-License-Identifier: MIT OR Apache-2.0
// Built-in 2D effects share a four-corner PCG lookup table. Scene shaders
// retain the non-periodic procedural noise in bava::fx.
#define_import_path bava::fx_cached

@group(#{MATERIAL_BIND_GROUP}) @binding(3) var noise_texture: texture_2d<f32>;

fn noise_cell(p: vec2<f32>) -> vec4<f32> {
    let cell = vec2<i32>(p);
    // Power-of-two wrapping works for negative cells too, without signed
    // division/remainder in every octave of every fragment.
    let coord = vec2<i32>(bitcast<vec2<u32>>(cell) & vec2<u32>(511u));
    return textureLoad(noise_texture, coord, 0);
}

fn noise2(p: vec2<f32>) -> f32 {
    let corners = noise_cell(floor(p));
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(corners.x, corners.y, u.x), mix(corners.z, corners.w, u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    let rot = mat2x2<f32>(0.8, 0.6, -0.6, 0.8);
    // Four octaves retain the broad plasma/nebula structure. The fifth
    // carries only 1/32 amplitude, but costs another dependent texture fetch
    // in each of the many domain-warp layers.
    for (var i = 0; i < 4; i++) {
        sum += amp * noise2(q);
        q = rot * q * 2.03 + vec2<f32>(1.7, 9.2);
        amp *= 0.5;
    }
    return sum / 0.9375;
}
