// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Glow and corona around the blob, blended additively on a quad behind it.
// Reads the live rim shape from `fx.shape`, so the glow hugs every bulge of the
// waveform instead of being a plain circle, and throws noisy corona streaks
// outward that stretch on the bass and flare on the beat.
//
// params[0] = (intensity, corona amount, falloff scale, quad half-size px)

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx::{fbm, TAU}
#import bava::fx_material::{fx, palette, rim_radius}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = in.world_position.xy;
    let r = length(p);
    let theta = atan2(p.y, p.x);
    let dir = p / max(r, 1e-3);
    let rim = rim_radius(theta);
    let base = max(fx.info.z, 1.0);
    let bass = fx.audio.x;
    let pulse = fx.clock.x;
    let flow = fx.clock.y;
    let intensity = fx.params[0].x;
    let corona_amount = fx.params[0].y;

    let d = r - rim; // px outside the rim (negative inside)
    let falloff = base * fx.params[0].z * (0.16 + 0.22 * bass + 0.18 * pulse);
    var glow = exp(-max(d, 0.0) / max(falloff, 1.0));
    // Fade in just inside the rim so the fill's edge has no seam.
    glow *= smoothstep(-base * 0.12, 0.0, d);

    // Corona: noise sampled along each ray, radius-stretched so it reads as
    // streaks; the flow clock drags the pattern outward.
    let rr = 11.0 + (max(d, 0.0) / base) * 1.4 - flow * 0.55;
    let streak = fbm(dir * rr + vec2<f32>(3.1, 7.7));
    let streak2 = fbm(dir * (rr * 0.5) + vec2<f32>(-flow * 0.2, 1.3));
    let corona = pow(max(streak * 0.65 + streak2 * 0.55 - 0.35, 0.0), 1.6)
        * exp(-max(d, 0.0) / (base * (0.35 + 0.6 * bass + 0.4 * pulse)))
        * smoothstep(-base * 0.05, base * 0.05, d);

    let hue = clamp(0.35 + 0.5 * glow + 0.3 * corona, 0.0, 1.0);
    let col = palette(hue);
    let energy = glow * (0.35 + 0.9 * bass + 1.2 * pulse) + corona * corona_amount * (1.0 + 2.0 * bass);
    // Fade out before the quad's edge so a big corona never shows a square.
    let half = max(fx.params[0].w, 1.0);
    let edge = 1.0 - smoothstep(0.7 * half, 0.98 * half, max(abs(p.x), abs(p.y)));
    let rgb = col * energy * intensity * edge;
    // Additive: the blend ignores alpha.
    return vec4<f32>(rgb, 1.0);
}
