// SPDX-License-Identifier: MIT OR Apache-2.0
//
// A blocky daytime sky on the scene's sky dome ([environment] sky_shader):
// a zenith → horizon gradient, a square sun that pulses on the beat, and a
// horizon that warms with the palette on each kick.
//
// params[0] = (zenith.rgb, _)
// params[1] = (horizon.rgb, _)
// params[2] = (sun direction xyz, sun half-size)

#import bevy_pbr::forward_io::VertexOutput
#import bava::fx_material::{fx, palette}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let d = normalize(in.world_position.xyz);
    let up = d.y;
    let zenith = fx.params[0].rgb;
    let horizon = fx.params[1].rgb;
    var col = mix(horizon, zenith, pow(clamp(up, 0.0, 1.0), 0.55));
    if up < 0.0 {
        col = mix(horizon, horizon * 0.55, clamp(-up * 4.0, 0.0, 1.0));
    }

    // A square sun: measure the view direction on the plane facing the sun.
    let sd = normalize(fx.params[2].xyz);
    let right = normalize(cross(sd, vec3<f32>(0.0, 1.0, 0.0)));
    let upv = cross(right, sd);
    let along = dot(d, sd);
    let x = dot(d, right) / max(along, 1e-3);
    let y = dot(d, upv) / max(along, 1e-3);
    let size = fx.params[2].w * (1.0 + 0.3 * fx.clock.x);
    let disc = step(0.0, along) * step(max(abs(x), abs(y)), size);
    let glow = pow(max(along, 0.0), 80.0) * 0.9 + pow(max(along, 0.0), 10.0) * 0.25;
    col += vec3<f32>(1.0, 0.94, 0.72) * (disc * 5.0 + glow * (1.0 + fx.audio.x));

    // The horizon catches the beat.
    let band = 1.0 - clamp(abs(up) * 3.0, 0.0, 1.0);
    col += palette(0.6) * fx.clock.x * 0.18 * band;
    return vec4<f32>(col, 1.0);
}
