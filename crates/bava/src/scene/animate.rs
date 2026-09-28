// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-frame motion of scene objects: orbits, spins, audio-reactive bindings,
//! and the 3D camera.
//!
//! Each object keeps its rest pose ([`SceneObject::base`]); every frame the pose
//! is rebuilt from scratch as *rest → orbit → spin → reactions*, so nothing
//! accumulates drift and a reaction always returns to rest when the music
//! stops. Objects are processed in declaration order, which `SceneDef::validate`
//! guarantees puts every orbit parent before its children, so a moon reads its
//! planet's position from the same frame.

use std::collections::HashMap;
use std::f32::consts::TAU;

use avian2d::prelude::{LinearVelocity, Position, RigidBody};
use bevy::prelude::*;

use crate::cava::Cava;
use crate::scene::material3d::FxMaterial3d;
use crate::vis::features::AudioFeatures;
use crate::vis::fx::material::FxMaterial;

/// A property an audio band can drive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Property {
    /// Uniform scale × (1 + amount · level).
    Scale,
    /// One scale axis × (1 + amount · level).
    ScaleAxis(usize),
    /// One position axis + amount · level.
    Position(usize),
    /// One rotation axis + amount · level degrees.
    Rotation(usize),
    /// Material brightness × (1 + amount · level).
    Brightness,
    /// Effect-shader parameter `params[vec][component]` + amount · level.
    Param(usize, usize),
}

impl Property {
    /// Parse a `react.property` string.
    pub fn parse(s: &str) -> Result<Self, String> {
        let axis = |a: &str| match a {
            "x" => Some(0),
            "y" => Some(1),
            "z" => Some(2),
            "w" => Some(3),
            _ => None,
        };
        let bad = || {
            format!(
                "unknown react property {s:?} (scale, scale.x|y|z, position.x|y|z, \
                 rotation.x|y|z, brightness, param.N.x|y|z|w)"
            )
        };
        let parts: Vec<&str> = s.trim().split('.').collect();
        Ok(match parts.as_slice() {
            ["scale"] => Property::Scale,
            ["brightness"] => Property::Brightness,
            ["scale", a] => Property::ScaleAxis(axis(a).filter(|&i| i < 3).ok_or_else(bad)?),
            ["position", a] => Property::Position(axis(a).filter(|&i| i < 3).ok_or_else(bad)?),
            ["rotation", a] => Property::Rotation(axis(a).filter(|&i| i < 3).ok_or_else(bad)?),
            ["param", n, a] => {
                let n: usize = n.parse().map_err(|_| bad())?;
                if n > 3 {
                    return Err(bad());
                }
                Property::Param(n, axis(a).ok_or_else(bad)?)
            }
            _ => return Err(bad()),
        })
    }
}

/// Where a binding reads its level from.
#[derive(Clone, Debug, PartialEq)]
pub enum BandRef {
    /// `bass`, `mid`, `treble`, `energy`, `beat`, or `bar:N`.
    Named(String),
    /// A position across the spectrum, 0 (lowest bar) ..= 1 (highest): a ring
    /// instance's own bar, resolved against the live bar count.
    Fraction(f32),
}

impl BandRef {
    /// The current level for this band.
    pub fn level(&self, features: &AudioFeatures, bars: &[f32]) -> f32 {
        match self {
            BandRef::Named(name) => features.band(name, bars),
            BandRef::Fraction(f) => {
                if bars.is_empty() {
                    0.0
                } else {
                    let i = (f.clamp(0.0, 1.0) * (bars.len() - 1) as f32).round() as usize;
                    bars[i]
                }
            }
        }
    }
}

/// One `react` entry, with its smoothing state.
#[derive(Clone, Debug)]
pub struct Binding {
    pub property: Property,
    pub band: BandRef,
    pub amount: f32,
    pub smooth: f32,
    pub level: f32,
}

impl Binding {
    /// Follow `now` with instant attack and exponential release.
    fn follow(&mut self, now: f32, dt: f32) -> f32 {
        if now >= self.level || self.smooth <= 1e-4 {
            self.level = now;
        } else if dt > 0.0 {
            self.level = now + (self.level - now) * (-dt / self.smooth).exp();
        }
        self.level
    }
}

/// A spawned scene object: its declaration index and rest pose.
#[derive(Component, Clone, Debug)]
pub struct SceneObject {
    pub index: usize,
    pub base: Transform,
}

/// Circle a point or another object.
#[derive(Component, Clone, Debug)]
pub struct Orbit {
    pub parent: Option<Entity>,
    pub center: Vec3,
    pub axes: Vec2,
    /// Radians per second (signed).
    pub angular: f32,
    pub angle: f32,
    /// Orientation of the orbit plane: identity is the XY plane in 2D and the
    /// XZ plane in 3D.
    pub plane: Quat,
    pub three_d: bool,
    pub speed_band: Option<BandRef>,
    pub speed_react: f32,
    /// Turn the object to face along its orbit angle (ring instances).
    pub face_out: bool,
    /// 2D: z while on the far half of the orbit.
    pub behind_z: Option<f32>,
    /// 2D: scale swing between the near and far points.
    pub perspective: f32,
}

impl Orbit {
    /// Offset from the center at the current angle.
    pub fn offset(&self) -> Vec3 {
        let (s, c) = self.angle.sin_cos();
        let local = if self.three_d {
            Vec3::new(c * self.axes.x, 0.0, s * self.axes.y)
        } else {
            Vec3::new(c * self.axes.x, s * self.axes.y, 0.0)
        };
        self.plane * local
    }

    /// The rotation that faces the object outward at the current angle.
    fn facing(&self) -> Quat {
        if self.three_d {
            self.plane * Quat::from_rotation_y(-self.angle)
        } else {
            // Local +Y points away from the center, so a bottom-anchored rect
            // becomes a radial bar growing outward.
            Quat::from_rotation_z(self.angle - std::f32::consts::FRAC_PI_2)
        }
    }
}

/// Constant rotation.
#[derive(Component, Clone, Debug)]
pub struct Spin {
    pub axis: Vec3,
    /// Radians per second.
    pub speed: f32,
    pub angle: f32,
}

/// Audio bindings.
#[derive(Component, Clone, Debug, Default)]
pub struct Reactive {
    pub bindings: Vec<Binding>,
}

/// The material an object's brightness / param reactions write to, with the
/// values they are relative to.
#[derive(Component, Clone, Debug)]
pub enum MaterialLink {
    Fx2d {
        handle: Handle<FxMaterial>,
        color: Vec4,
        params: [Vec4; 4],
    },
    Fx3d {
        handle: Handle<FxMaterial3d>,
        color: Vec4,
        params: [Vec4; 4],
    },
    Standard {
        handle: Handle<StandardMaterial>,
        color: LinearRgba,
        emissive: LinearRgba,
    },
    Color2d {
        handle: Handle<ColorMaterial>,
        color: LinearRgba,
    },
}

/// A light whose intensity reacts to the music.
#[derive(Component, Clone, Debug)]
pub struct SceneLight {
    pub base: f32,
    pub bindings: Vec<Binding>,
}

/// The scene's 3D camera rig.
#[derive(Component, Clone, Debug)]
pub struct SceneCamera {
    pub look_at: Vec3,
    /// Horizontal offset from `look_at` at rest, and height above it.
    pub offset: Vec3,
    pub orbit: f32,
    pub angle: f32,
    pub bob: f32,
    pub fov: f32,
    pub punch: f32,
}

/// Scene-wide layout: the 2D canvas scale.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct SceneLayout {
    /// Canvas units across the window's shorter side (0 = pixels).
    pub canvas: f32,
}

impl SceneLayout {
    /// Pixels per canvas unit for a `w × h` window.
    pub fn scale(&self, w: f32, h: f32) -> f32 {
        if self.canvas > 0.0 {
            w.min(h) / self.canvas
        } else {
            1.0
        }
    }
}

/// Rebuild every scene object's pose for this frame.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn animate_objects(
    time: Res<Time>,
    features: Res<AudioFeatures>,
    cava: Res<Cava>,
    layout: Res<SceneLayout>,
    windows: Query<&Window>,
    mut objects: Query<(
        Entity,
        &SceneObject,
        &mut Transform,
        Option<&mut Orbit>,
        Option<&mut Spin>,
        Option<&mut Reactive>,
        Option<&MaterialLink>,
        Option<(&RigidBody, &Position, &mut LinearVelocity)>,
    )>,
    mut fx2d: ResMut<Assets<FxMaterial>>,
    mut fx3d: ResMut<Assets<FxMaterial3d>>,
    mut standard: ResMut<Assets<StandardMaterial>>,
    mut color2d: ResMut<Assets<ColorMaterial>>,
    mut order: Local<Vec<(usize, Entity)>>,
    mut placed: Local<HashMap<Entity, Vec3>>,
    mut depth: Local<HashMap<Entity, f32>>,
) {
    let dt = time.delta_secs();
    let bars = cava.mono();
    let k = windows
        .iter()
        .next()
        .map(|w| layout.scale(w.width(), w.height()))
        .unwrap_or(1.0);
    order.clear();
    order.extend(objects.iter().map(|(e, o, ..)| (o.index, e)));
    order.sort_unstable();
    placed.clear();
    depth.clear();

    for &(_, entity) in order.iter() {
        let Ok((_, object, mut transform, orbit, spin, reactive, link, body)) =
            objects.get_mut(entity)
        else {
            continue;
        };
        let mut pose = object.base;

        if let Some(mut orbit) = orbit {
            let boost = orbit
                .speed_band
                .as_ref()
                .map(|b| 1.0 + orbit.speed_react * b.level(&features, &bars))
                .unwrap_or(1.0);
            orbit.angle = (orbit.angle + orbit.angular * boost * dt).rem_euclid(TAU);
            let center = orbit
                .parent
                .and_then(|p| placed.get(&p).copied())
                .unwrap_or(orbit.center);
            let offset = orbit.offset();
            pose.translation = center + offset + object.base.translation;
            if orbit.face_out {
                pose.rotation = orbit.facing() * pose.rotation;
            }
            if !orbit.three_d {
                // How far round the back of its orbit this object is (-1 near
                // .. 1 far). A satellite takes its parent's depth: the Moon and
                // Saturn's rings go behind the sun together with their planet.
                let own = if orbit.axes.y > 1e-3 {
                    (offset.y / orbit.axes.y).clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                let far = orbit
                    .parent
                    .and_then(|p| depth.get(&p).copied())
                    .unwrap_or(own);
                depth.insert(entity, far);
                if let Some(z) = orbit.behind_z
                    && far > 0.0
                {
                    pose.translation.z = z;
                }
                pose.scale *= 1.0 - orbit.perspective * far;
            }
        }

        if let Some(mut spin) = spin {
            spin.angle = (spin.angle + spin.speed * dt).rem_euclid(TAU);
            pose.rotation *= Quat::from_axis_angle(spin.axis, spin.angle);
        }

        let mut brightness = 1.0;
        let mut params: Option<[Vec4; 4]> = None;
        if let Some(mut reactive) = reactive {
            for b in &mut reactive.bindings {
                let now = b.band.level(&features, &bars);
                let v = b.follow(now, dt) * b.amount;
                match b.property {
                    Property::Scale => pose.scale *= 1.0 + v,
                    Property::ScaleAxis(i) => pose.scale[i] *= 1.0 + v,
                    Property::Position(i) => pose.translation[i] += v,
                    Property::Rotation(i) => {
                        let axis = [Vec3::X, Vec3::Y, Vec3::Z][i];
                        pose.rotation *= Quat::from_axis_angle(axis, v.to_radians());
                    }
                    Property::Brightness => brightness *= 1.0 + v,
                    Property::Param(n, c) => {
                        let base = match link {
                            Some(MaterialLink::Fx2d { params, .. })
                            | Some(MaterialLink::Fx3d { params, .. }) => *params,
                            _ => [Vec4::ZERO; 4],
                        };
                        let p = params.get_or_insert(base);
                        p[n][c] += v;
                    }
                }
            }
        }
        placed.insert(entity, pose.translation);
        // Canvas units → pixels (2D scenes only; `k` is 1 otherwise). Parents
        // are recorded in canvas units above, so children compose correctly.
        if k != 1.0 {
            pose.translation.x *= k;
            pose.translation.y *= k;
            pose.scale.x *= k;
            pose.scale.y *= k;
        }

        // Kinematic colliders are *driven* to their pose, so the solver sees a
        // moving body and balls get a proper push instead of a teleport.
        match body {
            Some((RigidBody::Kinematic, position, mut velocity)) if dt > 0.0 => {
                let target = pose.translation.truncate();
                velocity.0 = (target - position.0) / dt;
                transform.rotation = pose.rotation;
                transform.scale = pose.scale;
            }
            _ => {
                if *transform != pose {
                    *transform = pose;
                }
            }
        }

        if let Some(link) = link {
            apply_material(
                link,
                brightness,
                params,
                &mut fx2d,
                &mut fx3d,
                &mut standard,
                &mut color2d,
            );
        }
    }
}

/// Write reaction results into the object's material (only when it has
/// material-driving reactions, so static materials are never touched).
fn apply_material(
    link: &MaterialLink,
    brightness: f32,
    params: Option<[Vec4; 4]>,
    fx2d: &mut Assets<FxMaterial>,
    fx3d: &mut Assets<FxMaterial3d>,
    standard: &mut Assets<StandardMaterial>,
    color2d: &mut Assets<ColorMaterial>,
) {
    let scale = |c: Vec4| Vec4::new(c.x * brightness, c.y * brightness, c.z * brightness, c.w);
    match link {
        MaterialLink::Fx2d {
            handle,
            color,
            params: base,
        } => {
            if let Some(mut m) = fx2d.get_mut(handle) {
                m.uniform.color = scale(*color);
                m.uniform.params = params.unwrap_or(*base);
            }
        }
        MaterialLink::Fx3d {
            handle,
            color,
            params: base,
        } => {
            if let Some(mut m) = fx3d.get_mut(handle) {
                m.uniform.color = scale(*color);
                m.uniform.params = params.unwrap_or(*base);
            }
        }
        MaterialLink::Standard {
            handle,
            color,
            emissive,
        } => {
            if let Some(mut m) = standard.get_mut(handle) {
                if emissive.red + emissive.green + emissive.blue > 0.0 {
                    m.emissive = *emissive * brightness;
                } else {
                    let c = *color * brightness;
                    m.base_color = Color::LinearRgba(LinearRgba {
                        alpha: color.alpha,
                        ..c
                    });
                }
            }
        }
        MaterialLink::Color2d { handle, color } => {
            if let Some(mut m) = color2d.get_mut(handle) {
                let c = *color * brightness;
                m.color = Color::LinearRgba(LinearRgba {
                    alpha: color.alpha,
                    ..c
                });
            }
        }
    }
}

/// Scale reacting lights.
pub fn animate_lights(
    time: Res<Time>,
    features: Res<AudioFeatures>,
    cava: Res<Cava>,
    mut lights: Query<(
        &mut SceneLight,
        Option<&mut DirectionalLight>,
        Option<&mut PointLight>,
    )>,
) {
    let dt = time.delta_secs();
    let bars = cava.mono();
    for (mut light, dir, point) in &mut lights {
        let base = light.base;
        let mut k = 1.0;
        for b in &mut light.bindings {
            let now = b.band.level(&features, &bars);
            k *= 1.0 + b.follow(now, dt) * b.amount;
        }
        if let Some(mut d) = dir {
            d.illuminance = base * k;
        }
        if let Some(mut p) = point {
            p.intensity = base * k;
        }
    }
}

/// Orbit, bob and punch the 3D camera.
pub fn animate_camera(
    time: Res<Time>,
    features: Res<AudioFeatures>,
    fx: Res<crate::vis::fx::FxSettings>,
    mut cameras: Query<(&mut SceneCamera, &mut Transform, &mut Projection)>,
) {
    let dt = time.delta_secs();
    for (mut rig, mut transform, mut projection) in &mut cameras {
        rig.angle = (rig.angle + rig.orbit * dt).rem_euclid(TAU);
        let pos = rig.look_at
            + Quat::from_rotation_y(rig.angle) * rig.offset
            + Vec3::Y * rig.bob * features.bass.min(1.5);
        *transform = Transform::from_translation(pos).looking_at(rig.look_at, Vec3::Y);
        if let Projection::Perspective(p) = &mut *projection {
            let punch = if fx.enabled {
                1.0 - rig.punch * features.beat_pulse * features.beat_pulse
            } else {
                1.0
            };
            p.fov = rig.fov.to_radians() * punch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_parse() {
        assert_eq!(Property::parse("scale").unwrap(), Property::Scale);
        assert_eq!(Property::parse("scale.y").unwrap(), Property::ScaleAxis(1));
        assert_eq!(
            Property::parse("position.z").unwrap(),
            Property::Position(2)
        );
        assert_eq!(
            Property::parse("rotation.x").unwrap(),
            Property::Rotation(0)
        );
        assert_eq!(
            Property::parse(" brightness ").unwrap(),
            Property::Brightness
        );
        assert_eq!(Property::parse("param.2.w").unwrap(), Property::Param(2, 3));
        for bad in ["", "scale.w", "param.4.x", "param.x", "position", "glow"] {
            assert!(Property::parse(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn binding_attacks_instantly_and_releases_smoothly() {
        let mut b = Binding {
            property: Property::Scale,
            band: BandRef::Named("bass".into()),
            amount: 1.0,
            smooth: 0.1,
            level: 0.0,
        };
        assert_eq!(b.follow(0.8, 1.0 / 60.0), 0.8);
        let next = b.follow(0.0, 1.0 / 60.0);
        assert!(next > 0.0 && next < 0.8, "{next}");
        // No smoothing snaps.
        b.smooth = 0.0;
        assert_eq!(b.follow(0.0, 1.0 / 60.0), 0.0);
    }

    #[test]
    fn fraction_bands_resolve_against_the_live_bar_count() {
        let features = AudioFeatures::default();
        let bars = [0.1, 0.2, 0.3, 0.4, 0.5];
        assert_eq!(BandRef::Fraction(0.0).level(&features, &bars), 0.1);
        assert_eq!(BandRef::Fraction(1.0).level(&features, &bars), 0.5);
        assert_eq!(BandRef::Fraction(0.5).level(&features, &bars), 0.3);
        assert_eq!(BandRef::Fraction(0.5).level(&features, &[]), 0.0);
    }

    #[test]
    fn orbit_offset_lies_in_its_plane() {
        let mut o = Orbit {
            parent: None,
            center: Vec3::ZERO,
            axes: Vec2::new(10.0, 5.0),
            angular: 1.0,
            angle: 0.0,
            plane: Quat::IDENTITY,
            three_d: false,
            speed_band: None,
            speed_react: 0.0,
            face_out: false,
            behind_z: None,
            perspective: 0.0,
        };
        assert!((o.offset() - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-5);
        o.angle = std::f32::consts::FRAC_PI_2;
        assert!((o.offset() - Vec3::new(0.0, 5.0, 0.0)).length() < 1e-4);
        o.three_d = true;
        assert!((o.offset() - Vec3::new(0.0, 0.0, 5.0)).length() < 1e-4);
    }
}
