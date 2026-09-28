// SPDX-License-Identifier: MIT OR Apache-2.0
//
// A physics ball: a glossy, lit sphere with a specular highlight and a fresnel
// rim light. Drawn on Bevy's `Circle` mesh, whose UVs span the unit square, so
// the circle itself (and its antialiased edge) is resolved here.
//
// Reads only `color` and `params`: the ball buckets are not refreshed per frame
// (see `BallLooks` in `vis/fx/mod.rs`), so audio/clock fields would be stale.
//
// color     the ball's palette color (HDR)
// params[0] = (style, _, _, _) — style 0 is a flat disc like the old look.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx_material::fx

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let r2 = dot(p, p);
    let r = sqrt(r2);
    // The mesh is a 32-gon; keep the drawn circle inside it.
    let edge = 0.985;
    let aa = max(fwidth(r) * 1.25, 1e-4);
    let mask = 1.0 - smoothstep(edge - aa, edge, r);
    let base = fx.color.rgb;
    if fx.params[0].x < 0.5 {
        return vec4<f32>(base, fx.color.a * mask);
    }

    // `r` can exceed 1 on MSAA edge samples (the UV is extrapolated past the
    // mesh), so every `pow` base below is kept non-negative: a negative base
    // is undefined in WGSL and NaN on most backends.
    let z = sqrt(max(1.0 - r2 / (edge * edge), 0.0));
    let n = normalize(vec3<f32>(p / edge, z));
    let light = normalize(vec3<f32>(-0.45, 0.6, 0.66));
    let diffuse = max(dot(n, light), 0.0);
    let spec = pow(max(dot(reflect(-light, n), vec3<f32>(0.0, 0.0, 1.0)), 0.0), 24.0);
    let fresnel = pow(1.0 - z, 2.2);

    var rgb = base * (0.18 + 0.9 * diffuse);
    rgb += vec3<f32>(1.0) * spec * 1.4;
    rgb += base * fresnel * 1.5;
    return vec4<f32>(rgb, fx.color.a * mask);
}
