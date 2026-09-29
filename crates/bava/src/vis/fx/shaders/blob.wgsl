// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The blob's interior: a slowly swirling, domain-warped plasma in the live
// palette, with filaments that brighten on the bass and a hot rim that flares
// on every beat. Drawn on the WaveCircle triangle fan, whose UVs carry
// `uv.x` = 0 at the center → 1 on the rim.
//
// params[0] = (glow gain, opacity, style, _) — style 0 is the legacy flat
// translucent fill, anything else is the plasma.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx_cached::fbm
#import bava::fx_material::{fx, palette}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let radial = clamp(in.uv.x, 0.0, 1.0);
    let glow = fx.params[0].x;
    let opacity = fx.params[0].y;
    let peak = fx.params[0].w;
    if fx.params[0].z < 0.5 {
        // Legacy look: one translucent tint, brighter when loud.
        return vec4<f32>(palette(peak) * (1.0 + peak * glow), opacity);
    }

    let base = max(fx.info.z, 1.0);
    let p = in.world_position.xy / base;
    let flow = fx.clock.y;
    let bass = fx.audio.x;
    let pulse = fx.clock.x;

    // Two layers of domain warping: the flow clock drives the swirl, so the
    // plasma churns faster when the music is louder.
    let q = vec2<f32>(
        fbm(p * 1.25 + vec2<f32>(0.0, flow * 0.11)),
        fbm(p * 1.25 + vec2<f32>(5.2, -flow * 0.09)),
    );
    let r = vec2<f32>(
        fbm(p * 1.6 + 2.4 * q + vec2<f32>(1.7, 9.2) + flow * 0.05),
        fbm(p * 1.6 + 2.4 * q + vec2<f32>(8.3, 2.8) - flow * 0.04),
    );
    let n = fbm(p * 1.9 + 2.2 * r);

    let filaments = smoothstep(0.52, 0.9, n);
    let rim = pow(radial, 6.0);
    let core = pow(1.0 - radial, 3.0);
    let hue = clamp(n * 0.85 + radial * 0.35 - core * 0.2, 0.0, 1.0);
    let col = palette(hue);

    let intensity = 0.22
        + filaments * (0.6 + 1.1 * bass)
        + rim * (0.9 + 2.2 * bass + 1.5 * pulse)
        + core * pulse * 0.9;
    let rgb = col * intensity * (1.0 + glow * 0.35);
    let alpha = clamp(opacity * (0.45 + 0.55 * radial + filaments * 0.35), 0.0, 1.0);
    return vec4<f32>(rgb, alpha);
}
