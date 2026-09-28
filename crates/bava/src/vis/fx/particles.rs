// SPDX-License-Identifier: MIT OR Apache-2.0
//! Short-lived additive effects: rim flares, impact sparks and beat shockwaves.
//!
//! All three are rebuilt from scratch every frame, which is exactly the case
//! [`MeshBatch`] exists for: every live particle and ring goes into **one**
//! mesh on **one** entity (one allocation, one draw call), drawn with the
//! additive [`FxMaterial`] so overlapping sparks pile up into light.
//!
//! The simulation uses its own seeded RNG rather than the global `fastrand`
//! state, so an offline render (`--input`) with no randomized balls still comes
//! out bit-identical run to run.

use std::collections::{HashMap, VecDeque};
use std::f32::consts::TAU;

use avian2d::prelude::{LinearVelocity, PhysicsSystems};
use bevy::prelude::*;

use super::FxSettings;
use super::material::{ADDITIVE_SHADER, FxBlend, FxMaterial};
use crate::vis::circle::{BlobShape, SHAPE_SAMPLES};
use crate::vis::features::AudioFeatures;
use crate::vis::physics::Ball;
use crate::vis::stroke::{MeshBatch, empty_stroke_mesh};
use crate::vis::{DrawingMode, VisFamily, VisSettings, VisShape, gradient_color, sample_gradient};

/// Hard cap on live particles (oldest are dropped first).
const MAX_PARTICLES: usize = 2400;
/// Hard cap on simultaneous shockwave rings.
const MAX_WAVES: usize = 8;
/// A ball whose velocity changes by more than this in one frame (px/s) was
/// struck, and throws sparks.
const IMPACT_DV: f32 = 520.0;
/// Seconds a shockwave ring lives.
const WAVE_LIFE: f32 = 0.95;
/// Points around a shockwave ring.
const WAVE_POINTS: usize = 128;

/// One additive streak particle.
#[derive(Clone, Debug)]
struct Particle {
    pos: Vec2,
    vel: Vec2,
    age: f32,
    life: f32,
    /// Core half-width at the head, px.
    size: f32,
    /// Linear HDR color.
    color: Vec4,
    /// Exponential velocity damping rate, 1/s.
    drag: f32,
}

/// A ring expanding off the blob rim from the shape it had on the beat.
#[derive(Clone, Debug)]
struct Shockwave {
    age: f32,
    radii: [f32; SHAPE_SAMPLES],
    base: f32,
    rotation: f32,
    color: Vec4,
}

/// Live particles and rings, plus the simulation's own RNG.
#[derive(Resource)]
pub(crate) struct FxParticles {
    particles: VecDeque<Particle>,
    waves: Vec<Shockwave>,
    rng: fastrand::Rng,
    /// Fractional flares owed from previous frames (emission is a rate).
    owed: f32,
    /// Each ball's velocity last frame, for impact detection.
    last_velocity: HashMap<Entity, Vec2>,
    /// Balls struck hard this frame (scene `impact` sounds listen for it).
    pub(crate) impacts: u32,
}

impl Default for FxParticles {
    fn default() -> Self {
        Self {
            particles: VecDeque::new(),
            waves: Vec::new(),
            rng: fastrand::Rng::with_seed(0x6261_7661_5f66_7821),
            owed: 0.0,
            last_velocity: HashMap::new(),
            impacts: 0,
        }
    }
}

impl FxParticles {
    fn push(&mut self, p: Particle) {
        if self.particles.len() >= MAX_PARTICLES {
            self.particles.pop_front();
        }
        self.particles.push_back(p);
    }

    /// Advance every particle and ring by `dt`, dropping the expired ones.
    fn step(&mut self, dt: f32) {
        for p in &mut self.particles {
            p.age += dt;
            p.pos += p.vel * dt;
            p.vel *= (-p.drag * dt).exp();
        }
        self.particles.retain(|p| p.age < p.life);
        for w in &mut self.waves {
            w.age += dt;
        }
        self.waves.retain(|w| w.age < WAVE_LIFE);
    }

    fn rand_range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rng.f32()
    }
}

/// Marks the single entity every particle and ring is drawn into.
#[derive(Component)]
struct ParticleBatch;

/// Handle for the batched particle mesh, rebuilt each frame.
#[derive(Resource)]
struct ParticleHandles {
    mesh: Handle<Mesh>,
}

pub(crate) struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FxParticles>()
            .add_systems(Startup, setup_particles)
            // After avian has written back this frame's velocities (impacts are
            // read from them) and after every Update system that shapes the
            // blob, so emission and drawing see final values.
            .add_systems(
                PostUpdate,
                (emit_and_step, draw_particles)
                    .chain()
                    .after(PhysicsSystems::Writeback),
            );
    }
}

fn setup_particles(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<FxMaterial>>,
) {
    let mesh = meshes.add(empty_stroke_mesh());
    let mut material = FxMaterial::new(ADDITIVE_SHADER).with_blend(FxBlend::Additive);
    material.live = false;
    commands.spawn((
        Mesh2d(mesh.clone()),
        MeshMaterial2d(materials.add(material)),
        // Above the balls (1.0) and their trails.
        Transform::from_xyz(0.0, 0.0, 3.0),
        ParticleBatch,
    ));
    commands.insert_resource(ParticleHandles { mesh });
}

/// Spawn this frame's flares, sparks and rings, then advance everything.
#[allow(clippy::too_many_arguments)]
fn emit_and_step(
    time: Res<Time>,
    fx: Res<FxSettings>,
    vis: Res<VisSettings>,
    mode: Res<DrawingMode>,
    features: Res<AudioFeatures>,
    shape: Res<BlobShape>,
    balls: Query<(Entity, &Transform, &LinearVelocity, &Ball)>,
    mut state: ResMut<FxParticles>,
) {
    let dt = time.delta_secs();
    let state = &mut *state;
    state.impacts = 0;
    if !fx.enabled {
        state.particles.clear();
        state.waves.clear();
    }
    let circle = mode.family() == VisFamily::Circle && shape.active;
    let (lo, hi) = (vis.fg_lo(), vis.fg_hi());

    // Beat shockwave off the rim, in every circle mode.
    if fx.enabled && fx.shockwaves && circle && features.beat {
        if state.waves.len() >= MAX_WAVES {
            state.waves.remove(0);
        }
        let color = gradient_color(lo, hi, 0.85, vis.glow_gain)
            .to_linear()
            .to_vec4();
        state.waves.push(Shockwave {
            age: 0.0,
            radii: shape.radii,
            base: shape.base,
            rotation: shape.rotation,
            color,
        });
    }

    // Plasma flares, from the WaveCircle rim only (it is the one with a surface
    // to shed from). A steady rate that rises with the bass, plus a burst per
    // beat; each flare leaves from a rim point chosen with a bias toward the
    // loud ones.
    if fx.enabled
        && fx.flares > 0.0
        && circle
        && mode.shape() == VisShape::Wave
        && !shape.points.is_empty()
    {
        let rate = fx.flares * (18.0 + 160.0 * features.bass);
        state.owed += rate * dt;
        if features.beat {
            state.owed += fx.flares * 36.0;
        }
        let emit = state.owed.floor() as usize;
        state.owed -= emit as f32;
        let n = shape.points.len();
        let lift = vis.line_thickness * 0.5;
        for _ in 0..emit.min(400) {
            // Rejection-sample a rim point weighted toward the loud ones.
            let mut k = state.rng.usize(0..n);
            for _ in 0..3 {
                if state.rng.f32() * 1.5 < 0.25 + shape.points[k].1 {
                    break;
                }
                k = state.rng.usize(0..n);
            }
            let (pos, level) = shape.points[k];
            let dir = pos.normalize_or_zero();
            let tangent = Vec2::new(-dir.y, dir.x);
            let speed = state.rand_range(60.0, 140.0) + 420.0 * level * (0.4 + features.bass);
            let side = state.rand_range(-50.0, 50.0);
            let life = state.rand_range(0.55, 1.25);
            let size = state.rand_range(1.2, 3.2);
            let boost = state.rand_range(1.4, 2.6);
            let color = gradient_color(lo, hi, level.min(1.0), vis.glow_gain).to_linear();
            state.push(Particle {
                pos: pos + dir * lift,
                vel: dir * speed + tangent * side,
                age: 0.0,
                life,
                size,
                color: color.to_vec4() * Vec4::new(boost, boost, boost, 1.0),
                drag: 1.6,
            });
        }
    } else {
        state.owed = 0.0;
    }

    // Impacts: a ball whose velocity jumped was just struck by the rim, a
    // column, the wave or another ball. Counted even with the effects off (a
    // scene's impact sound listens for them); sparks only when enabled.
    let sparks = fx.enabled && fx.sparks;
    {
        let stops = vis.fg_stops();
        let mut seen: HashMap<Entity, Vec2> = HashMap::with_capacity(state.last_velocity.len());
        for (entity, transform, velocity, ball) in &balls {
            let v = velocity.0;
            seen.insert(entity, v);
            let Some(&before) = state.last_velocity.get(&entity) else {
                continue;
            };
            let dv = v - before;
            let hit = dv.length();
            if hit < IMPACT_DV {
                continue;
            }
            state.impacts += 1;
            if !sparks {
                continue;
            }
            let normal = dv / hit;
            let count = ((hit / 110.0) as usize).clamp(4, 18);
            let color = sample_gradient(&stops, ball.tint, vis.glow_gain)
                .to_linear()
                .to_vec4()
                * Vec4::new(2.2, 2.2, 2.2, 1.0);
            let at = transform.translation.truncate() - normal * ball.radius;
            for _ in 0..count {
                let spread = state.rand_range(-1.0, 1.0) * 1.1;
                let dir = Vec2::from_angle(spread).rotate(normal);
                let speed = state.rand_range(0.25, 0.8) * hit.min(1600.0);
                let life = state.rand_range(0.2, 0.55);
                let size = state.rand_range(0.8, 2.0);
                state.push(Particle {
                    pos: at,
                    vel: dir * speed,
                    age: 0.0,
                    life,
                    size,
                    color,
                    drag: 4.0,
                });
            }
        }
        state.last_velocity = seen;
    }

    state.step(dt);
}

/// Accumulate every particle and ring into the one additive mesh.
fn draw_particles(
    state: Res<FxParticles>,
    handles: Res<ParticleHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut batch: Local<MeshBatch>,
    mut pts: Local<Vec<(Vec2, Color)>>,
) {
    crate::profile_scope!("fx_particles");
    batch.clear();
    for p in &state.particles {
        let fade = (1.0 - p.age / p.life).clamp(0.0, 1.0).powf(1.4);
        let c = p.color;
        let head = Color::linear_rgba(c.x, c.y, c.z, fade);
        let tail = Color::linear_rgba(c.x, c.y, c.z, 0.0);
        // A comet streak whose length follows the speed.
        let len = (p.vel.length() * 0.035).max(2.0);
        let back = p.pos - p.vel.normalize_or(Vec2::X) * len;
        pts.clear();
        pts.push((back, tail));
        pts.push((p.pos, head));
        batch.push_stroke(&pts, 0.0, p.size * (0.4 + 0.6 * fade), 1.0, false);
    }
    for w in &state.waves {
        let u = (w.age / WAVE_LIFE).clamp(0.0, 1.0);
        let grow = u.powf(0.6) * w.base.max(40.0) * 1.4;
        let fade = (1.0 - u).powi(2);
        let c = w.color;
        let color = Color::linear_rgba(c.x, c.y, c.z, fade);
        pts.clear();
        for k in 0..WAVE_POINTS {
            let t = k as f32 / WAVE_POINTS as f32;
            let r = rim_at(&w.radii, t) + grow;
            let ang = t * TAU - std::f32::consts::FRAC_PI_2 + w.rotation;
            pts.push((Vec2::new(ang.cos(), ang.sin()) * r, color));
        }
        batch.push_stroke(
            &pts,
            1.0 + 4.0 * (1.0 - u),
            1.0 + 4.0 * (1.0 - u),
            2.0,
            true,
        );
    }
    if let Some(mut mesh) = meshes.get_mut(&handles.mesh) {
        batch.write(&mut mesh);
    }
}

/// Rim radius at angle fraction `t`, linearly interpolated from the samples.
fn rim_at(radii: &[f32; SHAPE_SAMPLES], t: f32) -> f32 {
    let x = t.rem_euclid(1.0) * SHAPE_SAMPLES as f32;
    let i0 = x.floor() as usize % SHAPE_SAMPLES;
    let i1 = (i0 + 1) % SHAPE_SAMPLES;
    radii[i0] + (radii[i1] - radii[i0]) * x.fract()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn particle(life: f32, vel: Vec2) -> Particle {
        Particle {
            pos: Vec2::ZERO,
            vel,
            age: 0.0,
            life,
            size: 1.0,
            color: Vec4::ONE,
            drag: 2.0,
        }
    }

    #[test]
    fn particles_move_slow_down_and_expire() {
        let mut s = FxParticles::default();
        s.push(particle(0.5, Vec2::new(100.0, 0.0)));
        s.step(0.1);
        let p = &s.particles[0];
        assert!((p.pos.x - 10.0).abs() < 1e-4);
        assert!(p.vel.x < 100.0, "drag slows it");
        for _ in 0..5 {
            s.step(0.1);
        }
        assert!(s.particles.is_empty(), "expired after its life");
    }

    #[test]
    fn particle_cap_drops_the_oldest() {
        let mut s = FxParticles::default();
        for i in 0..MAX_PARTICLES + 10 {
            s.push(particle(1.0 + i as f32, Vec2::ZERO));
        }
        assert_eq!(s.particles.len(), MAX_PARTICLES);
        assert_eq!(s.particles[0].life, 11.0, "first ten evicted");
    }

    #[test]
    fn waves_expire_after_their_life() {
        let mut s = FxParticles::default();
        s.waves.push(Shockwave {
            age: 0.0,
            radii: [100.0; SHAPE_SAMPLES],
            base: 100.0,
            rotation: 0.0,
            color: Vec4::ONE,
        });
        s.step(WAVE_LIFE * 0.5);
        assert_eq!(s.waves.len(), 1);
        s.step(WAVE_LIFE);
        assert!(s.waves.is_empty());
    }

    #[test]
    fn rim_interpolates_and_wraps() {
        let mut radii = [0.0f32; SHAPE_SAMPLES];
        radii[0] = 10.0;
        radii[1] = 20.0;
        radii[SHAPE_SAMPLES - 1] = 30.0;
        let step = 1.0 / SHAPE_SAMPLES as f32;
        assert!((rim_at(&radii, 0.0) - 10.0).abs() < 1e-4);
        assert!((rim_at(&radii, step * 0.5) - 15.0).abs() < 1e-3);
        // Between the last sample and the first, wrapping around.
        assert!((rim_at(&radii, 1.0 - step * 0.5) - 20.0).abs() < 1e-3);
        assert!((rim_at(&radii, 1.0) - 10.0).abs() < 1e-4);
    }

    #[test]
    fn seeded_rng_is_reproducible() {
        let mut a = FxParticles::default();
        let mut b = FxParticles::default();
        for _ in 0..10 {
            assert_eq!(a.rand_range(0.0, 1.0), b.rand_range(0.0, 1.0));
        }
    }
}
