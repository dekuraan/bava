// SPDX-License-Identifier: MIT OR Apache-2.0
//
// bava's shared shader library. Every built-in effect shader and every scene
// shader can `#import bava::fx::{...}` for the uniform layout and the noise /
// palette helpers below, and `#import bava::fx_material::{fx, fx_texture,
// fx_sampler}` for the material bindings themselves.
//
// The uniform is filled by the app every frame (see `vis/fx/material.rs`).
// `palette(t)` and `rim_radius(theta)` live in `bava::fx_material`, next to
// the binding they read:
//
//   color        base tint of this material (linear RGB, HDR allowed)
//   palette[4]   the live foreground gradient stops (album colors or profile)
//   audio        (bass, mid, treble, energy), each roughly 0..1
//   clock        (beat_pulse 1→0, flow clock, seconds, palette stop count)
//   params[4]    free per-material parameters (scene `params = [...]`)
//   shape[16]    64 floats: the blob rim radius (px) sampled evenly by angle
//   info         (viewport width, viewport height, blob base radius px, rotation)

#define_import_path bava::fx

struct FxUniform {
    color: vec4<f32>,
    palette: array<vec4<f32>, 4>,
    audio: vec4<f32>,
    clock: vec4<f32>,
    params: array<vec4<f32>, 4>,
    shape: array<vec4<f32>, 16>,
    info: vec4<f32>,
};

const PI: f32 = 3.14159265358979;
const TAU: f32 = 6.28318530717959;

// PCG-style 2D integer hash (Jarzynski & Olano, "Hash Functions for GPU
// Rendering", JCGT 2020). Integer arithmetic wraps, so it stays exact at any
// cell coordinate — the old `fract(p * 456.21)` float hash ran out of
// fractional bits a few thousand cells out, and the effect shaders offset
// their noise by the ever-growing flow clock.
fn pcg2d(seed: vec2<u32>) -> vec2<u32> {
    var v = seed * 1664525u + 1013904223u;
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    return v;
}

// Two independent uniform values in [0, 1) for the point `p`. Its integer
// cell is hashed exactly (to ±2^31); a fractional part (non-integer seeds such
// as `cell + 1.37`) is quantized to 1/65536 and mixed in, so it still hashes
// apart from the plain cell.
fn hash22(p: vec2<f32>) -> vec2<f32> {
    let cell = floor(p);
    let frac_bits = vec2<u32>((p - cell) * 65536.0);
    let seed = bitcast<vec2<u32>>(vec2<i32>(cell)) ^ (frac_bits * 2654435769u);
    // The top 24 bits convert to f32 exactly, so the result never rounds up to 1.
    return vec2<f32>(pcg2d(seed) >> vec2<u32>(8u)) * (1.0 / 16777216.0);
}

fn hash21(p: vec2<f32>) -> f32 {
    return hash22(p).x;
}

// Smooth value noise in 0..1.
fn noise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Five-octave fractal noise in roughly 0..1.
fn fbm(p: vec2<f32>) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    // A rotation between octaves hides the value-noise grid.
    let rot = mat2x2<f32>(0.8, 0.6, -0.6, 0.8);
    for (var i = 0; i < 5; i++) {
        sum += amp * noise2(q);
        q = rot * q * 2.03 + vec2<f32>(1.7, 9.2);
        amp *= 0.5;
    }
    return sum / 0.96875;
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}
