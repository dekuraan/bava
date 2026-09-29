// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The sun's surface, drawn in place of bava's blob fill ([blob] fill_shader).
// Boiling granulation cells, drifting sunspots, and a limb that flares with
// the bass. Colors come from the live palette — the scene pins a sun palette in
// [config.vis], so 0 = deep red, 1 = white-hot.
//
// params[0] is bava's: (glow gain, opacity, style, loudest rim level).
// params[1] = (granule scale, spot amount, boil speed, _)

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx::fbm
#import bava::fx_material::{fx, palette}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let radial = clamp(in.uv.x, 0.0, 1.0);
    let base = max(fx.info.z, 1.0);
    let p = in.world_position.xy / base;
    let boil = fx.clock.y * max(fx.params[1].z, 0.1);
    let bass = fx.audio.x;
    let pulse = fx.clock.x;
    let grain = max(fx.params[1].x, 1.0);

    // Granulation: two scales of noise, boiling at different rates.
    let g1 = fbm(p * grain + vec2<f32>(boil * 0.15, -boil * 0.1));
    let g2 = fbm(p * grain * 2.3 - vec2<f32>(boil * 0.32, boil * 0.21));
    let gran = g1 * 0.6 + g2 * 0.4;

    // Sunspots: slow dark patches.
    let spots = smoothstep(0.6, 0.72, fbm(p * 1.8 + vec2<f32>(fx.clock.z * 0.02, 3.0)))
        * fx.params[1].y;

    let heat = clamp(0.15 + gran * 0.75 - spots * 0.6 + (1.0 - radial) * 0.25, 0.0, 1.0);
    var col = palette(heat) * (0.75 + 1.0 * gran + 1.2 * pulse * (1.0 - radial));
    col *= 1.0 - spots * 0.8;
    // A hot limb that flares on the bass.
    col += palette(0.95) * pow(radial, 7.0) * (0.6 + 1.8 * bass);
    return vec4<f32>(col * (1.0 + fx.params[0].x * 0.2), 0.98);
}
