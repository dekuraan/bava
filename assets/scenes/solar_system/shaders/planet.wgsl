// SPDX-License-Identifier: MIT OR Apache-2.0
//
// A planet: a flat 2D circle shaded as a spinning, sun-lit sphere. The light
// comes from the world origin — where the sun (bava's blob) sits — so every
// planet's day side faces the sun wherever it is on its orbit.
//
// Drawn on a `circle` mesh; the disc fills 82% of it, leaving room for the
// atmosphere glow.
//
// color     tint (scene `color`; `brightness` reactions scale it)
// params[0] = (surface.rgb, band count)       — banded gas giants when > 0
// params[1] = (band.rgb, band contrast 0..1)
// params[2] = (craters 0..1, clouds 0..1, spin rad/s, 1 = use texture)
// params[3] = (atmosphere.rgb, atmosphere strength)
// texture   an equirectangular surface map (params[2].w = 1)

#import bevy_sprite::mesh2d_vertex_output::VertexOutput
#import bava::fx::{fbm, PI, TAU}
#import bava::fx_material::{fx, fx_texture, fx_sampler}

const DISC: f32 = 0.82;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let r = length(p);
    let aa = max(fwidth(r) * 1.5, 1e-4);
    let disc = 1.0 - smoothstep(DISC - aa, DISC, r);

    let q = p / DISC;
    let z = sqrt(max(1.0 - dot(q, q), 0.0));
    let n = vec3<f32>(q, z);

    // Light from the sun at the origin, a little toward the viewer.
    let to_sun = -in.world_position.xy;
    let l = normalize(vec3<f32>(to_sun / max(length(to_sun), 1e-3), 0.35));
    let lambert = max(dot(n, l), 0.0);

    // Spherical coordinates on the spinning globe.
    let lon = atan2(n.x, n.z) + fx.clock.z * fx.params[2].z;
    let lat = asin(clamp(n.y, -1.0, 1.0));

    // Sample unconditionally (uniform control flow), use it if asked to.
    let map_uv = vec2<f32>(fract(lon / TAU), 0.5 - lat / PI);
    let mapped = textureSampleLevel(fx_texture, fx_sampler, map_uv, 0.0).rgb;

    var albedo = fx.params[0].rgb;
    let bands = fx.params[0].w;
    if bands > 0.0 {
        let turb = fbm(vec2<f32>(lon * 1.5, lat * 5.0) + vec2<f32>(fx.clock.y * 0.04, 0.0));
        let b = 0.5 + 0.5 * sin(lat * bands + turb * 3.5);
        albedo = mix(albedo, fx.params[1].rgb, b * fx.params[1].w);
    }
    let craters = fx.params[2].x;
    if craters > 0.0 {
        let c = fbm(vec2<f32>(lon * 2.0, lat * 2.0) * 3.0);
        albedo *= 1.0 - craters * smoothstep(0.5, 0.75, c) * 0.6;
    }
    albedo = mix(albedo, mapped, step(0.5, fx.params[2].w));
    let clouds = fx.params[2].y;
    if clouds > 0.0 {
        let cl = smoothstep(0.55, 0.8, fbm(vec2<f32>(lon * 1.3 + fx.clock.z * 0.03, lat * 3.0)));
        albedo = mix(albedo, vec3<f32>(1.0), cl * clouds);
    }

    var col = albedo * (0.05 + 1.3 * lambert) * fx.color.rgb;
    // Atmosphere: a rim light on the day side, and a soft halo off the disc.
    let atmo = fx.params[3];
    let fresnel = pow(1.0 - z, 3.0);
    col += atmo.rgb * atmo.w * fresnel * (0.25 + lambert) * disc;
    let halo = (1.0 - smoothstep(DISC, 1.0, r)) * (1.0 - disc);
    let side = max(dot(normalize(vec3<f32>(p, 0.0001)), l), 0.0);
    let glow = atmo.rgb * atmo.w * halo * halo * (0.25 + side);

    let alpha = max(disc, halo * halo * atmo.w * 0.7) * fx.color.a;
    return vec4<f32>(col * disc + glow, alpha);
}
