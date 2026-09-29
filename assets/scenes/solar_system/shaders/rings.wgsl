// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Saturn's rings on an `annulus` (ring) mesh, squashed flat by the object's
// scale. The rings are drawn as two objects — the far half behind the planet
// and the near half in front — and params[1].x picks which half this is
// (1 = far/top, -1 = near/bottom). The annulus mesh's UVs are
// (radial 0..1, angle fraction starting at the top).
//
// params[0] = (inner.rgb, _)
// params[1] = (half, gap strength, _, _)

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx::{fbm, TAU}
#import bava::fx_material::fx

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let radial = in.uv.x;
    let top = cos(in.uv.y * TAU); // +1 at the top of the ring
    let half = fx.params[1].x;
    let keep = smoothstep(-0.02, 0.02, top * half);
    // Ringlets: fine radial bands with a Cassini-style gap.
    let lanes = 0.55 + 0.45 * sin(radial * 70.0 + fbm(vec2<f32>(radial * 12.0, 0.5)) * 4.0);
    let gap = 1.0 - fx.params[1].y * smoothstep(0.04, 0.0, abs(radial - 0.62));
    let edge = smoothstep(0.0, 0.08, radial) * smoothstep(1.0, 0.9, radial);
    let col = mix(fx.params[0].rgb, fx.color.rgb, radial) * (0.6 + 0.8 * lanes);
    let shimmer = 1.0 + fx.audio.z * 0.8 * lanes;
    let a = keep * lanes * gap * edge * 0.85 * fx.color.a;
    return vec4<f32>(col * shimmer, a);
}
