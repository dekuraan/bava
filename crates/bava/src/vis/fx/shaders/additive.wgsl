// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Vertex-colored geometry blended additively — sparks, flares and beat
// shockwaves. The vertex alpha (the stroke feather) scales the contribution.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx_material::fx

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
#ifdef VERTEX_COLORS
    let c = in.color;
#else
    let c = vec4<f32>(1.0);
#endif
    return vec4<f32>(c.rgb * c.a * fx.color.rgb, 1.0);
}
