//! Physics playground layered over the visualizers (avian2d).
//!
//! **Box mode** (the bars visualizer): left-click spawns a ball that falls under
//! gravity, bounces off the window walls, and — the whole point — gets launched
//! by the spectrum bars. Each bar is backed by a thin **kinematic platform** sat
//! at the bar's top edge; the platform is teleported to the audio-driven height
//! every frame *and* given a matching [`LinearVelocity`] so the solver transfers
//! real bounce energy to resting balls. The platform target is smoothed so the
//! collider tracks a clean curve instead of the raw, jittery cava values.
//!
//! Physics runs in [`PostUpdate`] with a variable timestep (not avian's default
//! fixed `FixedPostUpdate`) so the simulation steps once per render frame, in
//! lockstep with the per-frame cava analysis that drives the bars.
//!
//! Planet/orbit mode (circle visualizer) is a later pass.

use avian2d::prelude::*;
use bevy::prelude::*;

use crate::cava::{Cava, CavaSettings};
use crate::vis::{gradient_color, spread_monstercat, VisSettings, VisStyle};

/// 1 physics "metre" = this many world pixels. Scales avian's internal
/// tolerances (contact margins etc.) to our pixel-space coordinates.
const LENGTH_UNIT: f32 = 100.0;
/// Thickness of the kinematic bar platforms, in pixels.
const BAR_THICKNESS: f32 = 8.0;
/// Thickness of the boundary walls, in pixels.
const WALL_THICKNESS: f32 = 200.0;
/// Must match `bars::BAR_GAP` so platforms line up under the bar sprites.
const BAR_GAP: f32 = 2.0;
/// Must match `bars::MAX_HEIGHT_FRAC`.
const MAX_HEIGHT_FRAC: f32 = 0.9;
/// Park inactive (circle-mode) bar platforms far below the world.
const PARKED_Y: f32 = -1.0e6;

/// Runtime physics tunables, mapped from `[physics]` in the config.
#[derive(Resource, Clone, Debug)]
pub struct PhysicsSettings {
    /// Master switch; when false the plugin spawns nothing and ignores clicks.
    pub enabled: bool,
    /// Downward acceleration in px/s² (Box mode). ~980 ≈ earth at 100 px/m.
    pub gravity: f32,
    /// Default ball restitution (bounciness), 0..1.
    pub restitution: f32,
    /// Default ball linear damping (air resistance).
    pub air_resistance: f32,
    /// Default ball mass.
    pub mass: f32,
    /// Default ball radius, in pixels.
    pub radius: f32,
    /// Maximum live balls; oldest are despawned past this.
    pub max_balls: usize,
    /// Randomize each spawned ball's properties around the defaults.
    pub randomize: bool,
    /// Bar-platform smoothing time constant, in seconds (larger = smoother).
    pub bar_smoothing: f32,
    /// Restitution of the bar platforms.
    pub bar_restitution: f32,
}

impl Default for PhysicsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            gravity: 980.0,
            restitution: 0.85,
            air_resistance: 0.1,
            mass: 1.0,
            radius: 12.0,
            max_balls: 200,
            randomize: true,
            bar_smoothing: 0.05,
            bar_restitution: 1.0,
        }
    }
}

/// A spawned ball. `id` is a monotonic spawn counter used to evict the oldest.
#[derive(Component)]
struct Ball {
    id: u64,
}

/// Monotonic counter handing out [`Ball::id`]s.
#[derive(Resource, Default)]
struct BallCounter(u64);

/// A kinematic platform tracking one bar's top edge.
#[derive(Component)]
struct BarBody {
    /// Index into the cava bars.
    index: usize,
    /// Smoothed bar value (0..~1.5), low-pass filtered toward the raw value.
    smoothed: f32,
    /// Previous frame's top-edge y, for deriving the platform velocity.
    prev_top: f32,
}

/// Which boundary a wall is, so it can be repositioned on resize.
#[derive(Component, Clone, Copy)]
enum WallSide {
    Left,
    Right,
    Top,
    Bottom,
}

/// Physics plugin: avian + ball spawning + the kinematic bar platforms.
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhysicsSettings>()
            .init_resource::<BallCounter>()
            // Run the simulation in PostUpdate (variable timestep) so it steps
            // once per render frame, matching the per-frame cava analysis.
            .add_plugins(
                PhysicsPlugins::default()
                    .with_length_unit(LENGTH_UNIT)
                    .set(PhysicsSchedulePlugin::new(PostUpdate)),
            )
            .add_systems(Startup, setup_physics)
            .add_systems(
                Update,
                (spawn_ball_on_click, enforce_ball_cap, resize_walls, drive_bar_bodies),
            );
    }
}

/// Spawn the boundary walls and one kinematic platform per bar.
fn setup_physics(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    cava_settings: Res<CavaSettings>,
    windows: Query<&Window>,
) {
    if !settings.enabled {
        return;
    }

    // Gravity in our pixel space; Box mode uses it, planet mode will zero it.
    commands.insert_resource(Gravity(Vec2::NEG_Y * settings.gravity));

    let (w, h) = windows
        .iter()
        .next()
        .map(|win| (win.width(), win.height()))
        .unwrap_or((1280.0, 720.0));

    for side in [WallSide::Left, WallSide::Right, WallSide::Top, WallSide::Bottom] {
        let (size, pos) = wall_geometry(side, w, h);
        commands.spawn((
            RigidBody::Static,
            Collider::rectangle(size.x, size.y),
            Transform::from_translation(pos.extend(0.0)),
            side,
        ));
    }

    let n = cava_settings.bars_per_channel.max(1);
    for i in 0..n {
        commands.spawn((
            RigidBody::Kinematic,
            Collider::rectangle(8.0, BAR_THICKNESS),
            Restitution::new(settings.bar_restitution),
            Transform::from_translation(Vec3::new(0.0, PARKED_Y, 0.0)),
            LinearVelocity::default(),
            BarBody {
                index: i,
                smoothed: 0.0,
                prev_top: PARKED_Y,
            },
        ));
    }
}

/// Size and center of a wall, given the current window dimensions. Walls sit
/// just outside the visible area so balls bounce off the window edges.
fn wall_geometry(side: WallSide, w: f32, h: f32) -> (Vec2, Vec2) {
    let t = WALL_THICKNESS;
    match side {
        WallSide::Left => (Vec2::new(t, h + 2.0 * t), Vec2::new(-w / 2.0 - t / 2.0, 0.0)),
        WallSide::Right => (Vec2::new(t, h + 2.0 * t), Vec2::new(w / 2.0 + t / 2.0, 0.0)),
        WallSide::Top => (Vec2::new(w + 2.0 * t, t), Vec2::new(0.0, h / 2.0 + t / 2.0)),
        WallSide::Bottom => (Vec2::new(w + 2.0 * t, t), Vec2::new(0.0, -h / 2.0 - t / 2.0)),
    }
}

/// Keep the boundary walls glued to the window edges when it is resized.
fn resize_walls(
    windows: Query<&Window>,
    mut walls: Query<(&WallSide, &mut Transform, &mut Collider)>,
) {
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());
    for (side, mut transform, mut collider) in &mut walls {
        let (size, pos) = wall_geometry(*side, w, h);
        transform.translation = pos.extend(0.0);
        *collider = Collider::rectangle(size.x, size.y);
    }
}

/// Left-click spawns a ball at the cursor with (optionally randomized) props.
fn spawn_ball_on_click(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    settings: Res<PhysicsSettings>,
    vis: Res<VisSettings>,
    mut counter: ResMut<BallCounter>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) {
    if !settings.enabled || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Some((camera, cam_transform)) = cameras.iter().next() else {
        return;
    };
    let Ok(world) = camera.viewport_to_world_2d(cam_transform, cursor) else {
        return;
    };

    // Resolve ball properties, randomizing around the defaults when enabled.
    let (radius, restitution, damping, mass, tint) = if settings.randomize {
        let radius = settings.radius * fastrand::f32().mul_add(1.4, 0.6); // 0.6..2.0×
        let restitution = (0.5 + fastrand::f32() * 0.48).min(0.98);
        let damping = fastrand::f32() * 0.4;
        // Heavier when bigger: mass scales with area.
        let mass = settings.mass * (radius / settings.radius).powi(2);
        (radius, restitution, damping, mass, fastrand::f32())
    } else {
        (settings.radius, settings.restitution, settings.air_resistance, settings.mass, 0.5)
    };

    let color = gradient_color(vis.color_lo, vis.color_hi, tint);
    let id = counter.0;
    counter.0 += 1;

    commands.spawn((
        RigidBody::Dynamic,
        Collider::circle(radius),
        Restitution::new(restitution),
        LinearDamping(damping),
        Mass(mass),
        Mesh2d(meshes.add(Circle::new(radius))),
        MeshMaterial2d(materials.add(color)),
        Transform::from_translation(world.extend(1.0)),
        Ball { id },
    ));
}

/// Despawn the oldest balls once the live count exceeds `max_balls`.
fn enforce_ball_cap(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    balls: Query<(Entity, &Ball)>,
) {
    let max = settings.max_balls;
    let count = balls.iter().count();
    if count <= max {
        return;
    }
    let mut by_age: Vec<(Entity, u64)> = balls.iter().map(|(e, b)| (e, b.id)).collect();
    by_age.sort_by_key(|(_, id)| *id);
    for (entity, _) in by_age.into_iter().take(count - max) {
        commands.entity(entity).despawn();
    }
}

/// Drive the kinematic bar platforms from the latest audio.
///
/// Mirrors `bars::update_bars`' layout (monstercat spread, height budget, slot
/// positions) so the invisible platforms sit exactly under the bar sprites. Each
/// frame we low-pass the bar value, teleport the platform to the smoothed top
/// edge, and set its velocity to the per-frame delta so contacts launch balls.
/// While the circle style is active, platforms are parked far offscreen.
fn drive_bar_bodies(
    style: Res<VisStyle>,
    settings: Res<PhysicsSettings>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    time: Res<Time>,
    windows: Query<&Window>,
    mut bodies: Query<(&mut BarBody, &mut Transform, &mut LinearVelocity, &mut Collider)>,
) {
    if !settings.enabled {
        return;
    }
    let dt = time.delta_secs();

    // Park everything while the circle visualizer is active.
    if *style != VisStyle::Bars {
        for (mut body, mut transform, mut vel, _) in &mut bodies {
            transform.translation.y = PARKED_Y;
            vel.0 = Vec2::ZERO;
            body.prev_top = PARKED_Y;
            body.smoothed = 0.0;
        }
        return;
    }

    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());

    let mut values = cava.mono();
    let n = values.len();
    if n == 0 {
        return;
    }
    spread_monstercat(&mut values, vis.monstercat);

    let slot_w = w / n as f32;
    let bar_w = (slot_w - BAR_GAP).max(1.0);
    let max_h = h * MAX_HEIGHT_FRAC * if vis.mirror { 0.5 } else { 1.0 };
    let floor = -h / 2.0;
    let left = -w / 2.0;
    // Exponential smoothing factor for this frame's dt.
    let alpha = if settings.bar_smoothing > 0.0 {
        1.0 - (-dt / settings.bar_smoothing).exp()
    } else {
        1.0
    };

    for (mut body, mut transform, mut vel, mut collider) in &mut bodies {
        let raw = values.get(body.index).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        body.smoothed += (raw - body.smoothed) * alpha;

        let bar_h = (body.smoothed * max_h).max(1.0);
        // Top edge: bars grow from the floor, or from the center when mirrored.
        let top = if vis.mirror { bar_h } else { floor + bar_h };
        let x = left + slot_w * (body.index as f32 + 0.5);

        transform.translation.x = x;
        transform.translation.y = top;
        *collider = Collider::rectangle(bar_w, BAR_THICKNESS);

        // Velocity from the per-frame top delta so the solver imparts bounce.
        if dt > 0.0 && body.prev_top > PARKED_Y / 2.0 {
            vel.0 = Vec2::new(0.0, (top - body.prev_top) / dt);
        } else {
            vel.0 = Vec2::ZERO;
        }
        body.prev_top = top;
    }
}
