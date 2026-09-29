// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Deep-space backdrop, blended additively over the (dimmed) album art: three
// slowly rotating parallax star layers that twinkle on the treble, and a faint
// palette-tinted nebula that breathes with the overall energy.
//
// params[0] = (star density, nebula intensity, star brightness, _)

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx::{fbm, hash21, hash22}
#import bava::fx_material::{fx, palette}

fn rotate(p: vec2<f32>, a: f32) -> vec2<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y);
}

fn star_layer(p: vec2<f32>, scale: f32, density: f32, seed: f32, time: f32, treble: f32) -> f32 {
    let g = p * scale;
    let cell = floor(g);
    let f = fract(g) - 0.5;
    let h = hash22(cell + seed);
    let present = step(hash21(cell + seed * 1.37), density);
    let offset = (h - 0.5) * 0.7;
    let d = length(f - offset);
    let size = 0.035 + 0.05 * h.x;
    let twinkle = 0.55 + 0.45 * sin(time * (1.5 + 4.0 * h.y) + h.x * 40.0);
    let spark = 1.0 + treble * 2.5 * step(0.8, h.y);
    return present * smoothstep(size, 0.0, d) * twinkle * spark;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let res = max(fx.info.xy, vec2<f32>(1.0));
    let unit = min(res.x, res.y);
    let p = in.world_position.xy / unit;
    let time = fx.clock.z;
    let flow = fx.clock.y;
    let treble = fx.audio.z;
    let energy = fx.audio.w;
    let pulse = fx.clock.x;
    let density = fx.params[0].x;
    let nebula_gain = fx.params[0].y;
    let star_gain = fx.params[0].z;

    var stars = 0.0;
    stars += star_layer(rotate(p, time * 0.004), 38.0, density, 1.0, time, treble) * 0.55;
    stars += star_layer(rotate(p, time * 0.007), 22.0, density * 0.8, 7.0, time, treble) * 0.8;
    stars += star_layer(rotate(p, time * 0.011), 12.0, density * 0.6, 13.0, time, treble) * 1.1;

    let q = p * 2.4;
    let w = vec2<f32>(fbm(q + vec2<f32>(0.0, flow * 0.02)), fbm(q + vec2<f32>(4.1, -flow * 0.025)));
    let n = fbm(q * 0.9 + 1.8 * w);
    let cloud = pow(smoothstep(0.35, 0.95, n), 1.7);
    // Keep the nebula away from the dead center, where the blob lives.
    let ring = smoothstep(0.08, 0.45, length(p));
    let neb = palette(n) * cloud * ring * (0.05 + 0.2 * energy + 0.14 * pulse) * nebula_gain;

    let star_col = mix(vec3<f32>(0.75, 0.85, 1.0), palette(hash21(floor(p * 12.0))), 0.35);
    let rgb = neb + star_col * stars * star_gain;
    return vec4<f32>(rgb, 1.0);
}
