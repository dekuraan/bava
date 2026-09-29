// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Block-game water on a plane: the atlas's water tile repeated once per block
// in world space (so it lines up with the terrain grid), drifting slowly,
// shimmering with the treble and flashing a little on the beat.
//
// params[0] = (tile index, atlas columns, atlas rows, opacity)
// texture   the block atlas (pixelated)

#import bevy_pbr::forward_io::VertexOutput
#import bava::fx_material::{fx, fx_texture, fx_sampler}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let t = fx.clock.z;
    let w = in.world_position.xz + vec2<f32>(t * 0.35, t * 0.12);
    let cell = fract(w);
    let tile = fx.params[0].x;
    let cols = max(fx.params[0].y, 1.0);
    let rows = max(fx.params[0].z, 1.0);
    let origin = vec2<f32>(tile - cols * floor(tile / cols), floor(tile / cols));
    let uv = (origin + 0.002 + cell * 0.996) / vec2<f32>(cols, rows);
    let c = textureSampleLevel(fx_texture, fx_sampler, uv, 0.0).rgb;
    let ripple = sin(w.x * 2.7 + t * 3.1) * sin(w.y * 1.9 - t * 2.3);
    let shimmer = 1.0 + fx.audio.z * 0.7 * ripple;
    let col = c * (0.85 + 0.6 * fx.clock.x) * shimmer;
    return vec4<f32>(col, fx.params[0].w);
}
