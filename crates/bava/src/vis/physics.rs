// SPDX-License-Identifier: GPL-3.0-or-later
//! Physics playground layered over the visualizers (avian2d).
//!
//! The collision geometry **tracks the rendered meshes** rather than a single
//! smooth bottom wave. Physics is supported only on the shapes whose meshes map
//! cleanly onto floor-anchored colliders; the rest are inert for now:
//!
//! - **Bars / Levels (box, un-mirrored)** — one kinematic rounded column per bar,
//!   pooled and reshaped every frame to sit exactly on the rendered bar (gaps and
//!   all), reusing [`bars`](crate::vis::bars)'s [`Layout`]/[`column_geom`]. Balls
//!   rest on top of columns and fall **between** them to the floor. A
//!   [`push_columns`] pass launches balls a rising column drives into, and
//!   unsticks any a fast column has swallowed.
//! - **Wave (box)** — a single smooth kinematic [`Collider::heightfield`] rebuilt
//!   each frame from the (interpolated, time-smoothed) cava values; balls ride the
//!   waveform and are launched along its true surface normal by [`push_balls`].
//! - **All circle modes** — global gravity off, each ball pulled toward the center
//!   by `central_gravity`, bouncing the pulsing [`Collider::polyline`] blob (rebuilt
//!   each frame from the same [`blob_ring`] the renderer draws).
//! - **Particles / Spine / Splitter (box)** and **mirrored Bars/Levels** — physics
//!   **disabled**: colliders parked, clicks ignored. (Their meshes float above /
//!   cross the axis, which a floor surface can't represent — a later increment.)
//!
//! Switching mode runs [`on_mode_change`], which despawns the live balls and zeroes
//! the surface caches so nothing carries phantom velocity into the new mode.
//!
//! Physics runs in [`PostUpdate`] with a variable timestep (not avian's default
//! fixed `FixedPostUpdate`) so it steps once per render frame, in lockstep with
//! the per-frame cava analysis. Balls carry [`SweptCcd`] so they don't tunnel.
//! [`PhysicsDebugPlugin`] draws the live collider wireframes when
//! `[physics] debug_draw` is on (toggle at runtime with **F3**).

use std::collections::VecDeque;

use avian2d::prelude::*;
use bevy::prelude::*;

use crate::cava::Cava;
use crate::gui::EditorState;
use crate::vis::bars::{column_geom, mirror_values, Layout, LEVEL_STEPS, MAX_HEIGHT_FRAC};
use crate::vis::circle::blob_ring;
use crate::vis::stroke::{apply_stroke_tapered, empty_stroke_mesh, stroke_material, STROKE_FEATHER};
use crate::vis::{
    gradient_color, spread_monstercat, DrawingMode, MirrorMode, VisFamily, VisSettings, VisShape,
};

/// 1 physics "metre" = this many world pixels. Scales avian's internal
/// tolerances (contact margins etc.) to our pixel-space coordinates.
const LENGTH_UNIT: f32 = 100.0;
/// Thickness of the boundary walls, in pixels.
const WALL_THICKNESS: f32 = 200.0;
/// Horizontal resolution of the Wave heightfield collider. Higher = smoother
/// curve and finer slope normals.
const SAMPLES: usize = 192;
/// Park an inactive surface/planet/column body far outside the world.
const PARKED: f32 = 1.0e6;

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
    /// Surface smoothing time constant, in seconds (larger = smoother/slower).
    pub bar_smoothing: f32,
    /// Restitution of the spectrum surface.
    pub bar_restitution: f32,
    /// Launch gain: how strongly a rising surface/column flings balls.
    pub bar_push: f32,
    /// Planet mode: radial acceleration pulling balls toward the center, px/s².
    pub central_gravity: f32,
    /// Draw a fading color trail behind each ball.
    pub trails: bool,
    /// Trail length: how many recent positions each trail keeps.
    pub trail_length: usize,
    /// Draw the avian collider wireframes (toggle at runtime with F3).
    pub debug_draw: bool,
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
            bar_push: 1.6,
            central_gravity: 1500.0,
            trails: true,
            trail_length: 18,
            debug_draw: false,
        }
    }
}

/// A spawned ball. `id` is a monotonic spawn counter used to evict the oldest;
/// `radius` is cached for the surface-push proximity test.
#[derive(Component)]
struct Ball {
    id: u64,
    radius: f32,
}

/// Monotonic counter handing out [`Ball::id`]s.
#[derive(Resource, Default)]
struct BallCounter(u64);

/// A fading color trail rendered behind a ball as a feathered stroke. Lives on
/// its own (un-parented) entity so the polyline can stay in world space; it is
/// reaped by [`update_trails`] the frame its `ball` no longer exists, covering
/// every despawn path (cap, escape, mode change).
#[derive(Component)]
struct Trail {
    /// The ball this trail follows.
    ball: Entity,
    /// Recent world positions, oldest → newest.
    points: VecDeque<Vec2>,
    /// The ball's (HDR) tint; the per-point alpha fades it tail-ward.
    color: Color,
    /// Stroke half-width, derived from the ball radius.
    half_width: f32,
}

/// Shared blend material for every trail mesh (per-vertex color/alpha do the work).
#[derive(Resource)]
struct TrailMaterial(Handle<ColorMaterial>);

/// Only record a new trail point once the ball has moved at least this far (px²),
/// so a resting ball doesn't pile up a zero-length smear.
const TRAIL_MIN_STEP_SQ: f32 = 4.0;

/// Time-smoothed Wave surface, shared by the heightfield/mesh update and the
/// ball-push pass. `heights`/`prev` are absolute world-space y of the surface at
/// each of [`SAMPLES`] evenly spaced x columns (this frame and last).
#[derive(Resource)]
struct Surface {
    heights: Vec<f32>,
    prev: Vec<f32>,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            heights: vec![0.0; SAMPLES],
            prev: vec![0.0; SAMPLES],
        }
    }
}

/// Per-bar column-top world y for Bars/Levels (this frame + last), sized to the
/// live bar count. Shared by the column collider update and [`push_columns`].
#[derive(Resource, Default)]
struct Columns {
    tops: Vec<f32>,
    prev: Vec<f32>,
}

/// Planet-mode blob ring, sampled this frame and last (per-segment radii from the
/// center). Shared by the collider rebuild and the radial ball forces.
#[derive(Resource, Default)]
struct Planet {
    radii: Vec<f32>,
    prev: Vec<f32>,
    /// Closed-loop polyline indices, cached and rebuilt only when the segment
    /// count changes (it's a pure function of `n`, identical every frame).
    indices: Vec<[u32; 2]>,
}

/// The single kinematic polyline body for the planet blob.
#[derive(Component)]
struct PlanetBody;

/// The single kinematic heightfield body for the Wave surface.
#[derive(Component)]
struct SurfaceBody;

/// One kinematic column collider for the Bars/Levels pool; the field is its Cava
/// bar index (parallel to `bars::Bar`).
#[derive(Component)]
struct BarColumn(usize);

/// Which boundary a wall is, so it can be repositioned on resize.
#[derive(Component, Clone, Copy)]
enum WallSide {
    Left,
    Right,
    Top,
    Bottom,
}

/// Physics plugin: avian + ball spawning + the mesh-matched spectrum colliders.
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhysicsSettings>()
            .init_resource::<BallCounter>()
            .init_resource::<Surface>()
            .init_resource::<Columns>()
            .init_resource::<Planet>()
            // Run the simulation in PostUpdate (variable timestep) so it steps
            // once per render frame, matching the per-frame cava analysis.
            .add_plugins((
                PhysicsPlugins::default()
                    .with_length_unit(LENGTH_UNIT)
                    .set(PhysicsSchedulePlugin::new(PostUpdate)),
                PhysicsDebugPlugin::default(),
            ))
            .add_systems(Startup, setup_physics)
            .add_systems(
                Update,
                (
                    // Tear down on a mode switch before anything reads the caches.
                    on_mode_change,
                    spawn_ball_on_click,
                    enforce_ball_cap,
                    despawn_escaped_balls,
                    resize_walls,
                    update_gravity_mode,
                    // Update each surface before reading it to move balls.
                    (update_surface, push_balls).chain(),
                    (reconcile_columns, update_columns, push_columns).chain(),
                    (update_planet, planet_forces).chain(),
                    update_trails,
                    toggle_physics_debug,
                    sync_physics_debug,
                ),
            );
    }
}

/// Whether the spectrum **column** pool drives physics this frame: the
/// floor-anchored, un-mirrored Bars/Levels box modes.
fn columns_active(mode: DrawingMode, vis: &VisSettings) -> bool {
    use crate::vis::Direction;
    mode.family() == VisFamily::Box
        && matches!(mode.shape(), VisShape::Bars | VisShape::Levels)
        && vis.mirror == MirrorMode::Off
        // Physics column colliders are floor-anchored (BottomTop only).
        && vis.direction == Direction::BottomTop
}

/// Whether the **Wave** heightfield drives physics this frame.
fn wave_active(mode: DrawingMode, vis: &VisSettings) -> bool {
    use crate::vis::Direction;
    mode == DrawingMode::WaveBox && vis.direction == Direction::BottomTop
}

/// Whether the **planet** blob drives physics this frame. The smooth WaveCircle
/// rim is the only circle shape with a continuous collider; the other circle
/// shapes render as discrete spokes/dots, so the blob would be invisible there.
fn planet_active(mode: DrawingMode) -> bool {
    mode == DrawingMode::WaveCircle
}

/// Whether physics is supported at all in this mode (otherwise: inert).
fn physics_supported(mode: DrawingMode, vis: &VisSettings) -> bool {
    planet_active(mode) || columns_active(mode, vis) || wave_active(mode, vis)
}

/// Spawn the boundary walls, the kinematic Wave heightfield, and the planet body.
fn setup_physics(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    windows: Query<&Window>,
) {
    // Shared blend material for the ball trails. Inserted unconditionally: the
    // always-scheduled `spawn_ball_on_click` requires `Res<TrailMaterial>`, and a
    // missing required resource fails param validation → panic on the first frame
    // (Bevy 0.18). It just early-returns in its body when physics is disabled.
    commands.insert_resource(TrailMaterial(materials.add(stroke_material())));

    if !settings.enabled {
        return;
    }

    // Gravity in our pixel space; Box mode uses it, circle modes zero it.
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

    // Kinematic Wave heightfield, flat at the floor until the first update.
    let floor = -h / 2.0;
    commands.spawn((
        RigidBody::Kinematic,
        Collider::heightfield(vec![floor; SAMPLES], Vec2::new(w, 1.0)),
        Restitution::new(settings.bar_restitution),
        Transform::from_xyz(0.0, -PARKED, 0.0),
        SurfaceBody,
    ));

    // Kinematic planet blob, parked offscreen until a circle mode activates it.
    // Frictionless (Min combine) so orbiting balls slide along the pulsing rim
    // instead of being pinned by friction and dragged as the blob shape shifts.
    commands.spawn((
        RigidBody::Kinematic,
        Collider::circle(10.0),
        Restitution::new(settings.bar_restitution),
        Friction::new(0.0).with_combine_rule(CoefficientCombine::Min),
        Transform::from_xyz(PARKED, PARKED, 0.0),
        PlanetBody,
    ));
}

/// Size and center of a wall, given the current window dimensions. Walls sit
/// just outside the visible area so balls bounce off the window edges; the
/// bottom wall's top face sits exactly on the floor (`-h/2`), the floor balls
/// rest on when they fall between columns.
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

/// Despawn the live balls and zero the surface caches whenever the drawing mode
/// changes, so nothing carries stale geometry or phantom launch velocity into
/// the new mode (gravity flips on box↔circle; supported↔inert toggles spawning).
fn on_mode_change(
    mut commands: Commands,
    mode: Res<DrawingMode>,
    mut last: Local<Option<DrawingMode>>,
    balls: Query<Entity, With<Ball>>,
    surface: ResMut<Surface>,
    columns: ResMut<Columns>,
    planet: ResMut<Planet>,
) {
    if *last == Some(*mode) {
        return;
    }
    *last = Some(*mode);

    for entity in &balls {
        commands.entity(entity).despawn();
    }
    // Collapse each rate cache to zero delta (prev == current) so the first frame
    // back doesn't read a huge rise/expand and fling the (now absent) balls.
    // Reborrow to split-borrow the two fields past `ResMut`'s Deref.
    let surface = surface.into_inner();
    surface.prev.copy_from_slice(&surface.heights);
    let columns = columns.into_inner();
    columns.prev.copy_from_slice(&columns.tops);
    let planet = planet.into_inner();
    planet.prev.copy_from_slice(&planet.radii);
}

/// Left-click spawns a ball at the cursor with (optionally randomized) props,
/// but only in a mode where physics is supported.
#[allow(clippy::too_many_arguments)]
fn spawn_ball_on_click(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    vis: Res<VisSettings>,
    editor: Res<EditorState>,
    trail_mat: Res<TrailMaterial>,
    mut counter: ResMut<BallCounter>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) {
    // Skip when disabled, when the click lands on egui, or in an inert mode.
    if !settings.enabled
        || editor.capture_pointer
        || !mouse.just_pressed(MouseButton::Left)
        || !physics_supported(*mode, &vis)
    {
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

    let color = gradient_color(vis.fg_lo(), vis.fg_hi(), tint, vis.glow_gain);
    let id = counter.0;
    counter.0 += 1;

    // In planet (WaveCircle) mode, launch tangentially for a near-circular orbit;
    // otherwise drop it in (gravity takes over).
    let velocity = if planet_active(*mode) {
        let r = world.length();
        if r > 1.0 {
            let radial = world / r;
            let dir = if fastrand::bool() { 1.0 } else { -1.0 };
            let tangent = Vec2::new(-radial.y, radial.x) * dir;
            tangent * (settings.central_gravity * r).sqrt()
        } else {
            Vec2::ZERO
        }
    } else {
        Vec2::ZERO
    };

    let ball = commands
        .spawn((
            RigidBody::Dynamic,
            Collider::circle(radius),
            Restitution::new(restitution),
            LinearDamping(damping),
            Mass(mass),
            LinearVelocity(velocity),
            // Continuous collision detection: stops fast-falling balls from
            // tunneling through the surface and the walls.
            SweptCcd::default(),
            Mesh2d(meshes.add(Circle::new(radius))),
            MeshMaterial2d(materials.add(color)),
            Transform::from_translation(world.extend(1.0)),
            Ball { id, radius },
        ))
        .id();

    // A trail entity following this ball, drawn just behind it (z = 0.9).
    if settings.trails {
        commands.spawn((
            Mesh2d(meshes.add(empty_stroke_mesh())),
            MeshMaterial2d(trail_mat.0.clone()),
            Transform::from_xyz(0.0, 0.0, 0.9),
            Trail {
                ball,
                points: VecDeque::with_capacity(settings.trail_length + 1),
                color,
                half_width: (radius * 0.7).max(1.0),
            },
        ));
    }
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
    let over = count - max;
    // Partition the `over` oldest (smallest id) to the front in O(n) rather than
    // fully sorting O(n log n) every frame we're at the cap.
    let mut by_age: Vec<(Entity, u64)> = balls.iter().map(|(e, b)| (e, b.id)).collect();
    by_age.select_nth_unstable_by_key(over - 1, |(_, id)| *id);
    for (entity, _) in by_age.into_iter().take(over) {
        commands.entity(entity).despawn();
    }
}

/// Safety net for balls that escape the play area — e.g. tunneled past a wall on
/// a frame spike. Anything well past the window bounds (especially *below* the
/// floor, where stuck balls collect) is despawned rather than left lost.
fn despawn_escaped_balls(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    windows: Query<&Window>,
    balls: Query<(Entity, &Transform), With<Ball>>,
) {
    if !settings.enabled {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    // Generous margins on the sides/top so balls mid-bounce aren't culled, but a
    // tight floor margin: nothing should ever be below it, so anything there
    // tunneled the bottom wall and should go.
    let margin = 0.5 * window.height().max(window.width());
    let (max_x, min_y, max_y) = (
        window.width() / 2.0 + margin,
        -window.height() / 2.0 - 40.0,
        window.height() / 2.0 + margin,
    );
    for (entity, transform) in &balls {
        let p = transform.translation;
        if p.y < min_y || p.y > max_y || p.x.abs() > max_x {
            commands.entity(entity).despawn();
        }
    }
}

/// Resolve the smoothed Wave surface height for column `s` (0..SAMPLES) from the
/// cava bar `values`, interpolating smoothly between bars for a blobby curve.
fn sample_height(values: &[f32], s: usize) -> f32 {
    let n = values.len();
    if n == 1 {
        return values[0];
    }
    let f = s as f32 / (SAMPLES - 1) as f32 * (n - 1) as f32;
    let i0 = (f.floor() as usize).min(n - 1);
    let i1 = (i0 + 1).min(n - 1);
    let frac = f - i0 as f32;
    let t = frac * frac * (3.0 - 2.0 * frac); // smoothstep
    values[i0] + (values[i1] - values[i0]) * t
}

/// Rebuild the **Wave** heightfield collider from the latest audio, time-smoothed.
/// Parked offscreen unless WaveBox is active. Updates the shared [`Surface`] for
/// [`push_balls`].
fn update_surface(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    time: Res<Time>,
    windows: Query<&Window>,
    surface: ResMut<Surface>,
    mut body: Query<(&mut Collider, &mut Transform), With<SurfaceBody>>,
) {
    if !settings.enabled {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());
    // Match `bars::update_box_lines`'s Wave geometry: the draw region is inset by
    // `area_margin` and shifted by `area_offset`, so the heightfield's floor,
    // amplitude and horizontal span all follow the same transform.
    let eff_w = (w - 2.0 * vis.area_margin).max(1.0);
    let eff_h = (h - 2.0 * vis.area_margin).max(1.0);
    let ox = vis.area_offset.x * w * 0.5;
    let oy = vis.area_offset.y * h * 0.5;
    let floor = -eff_h * 0.5 + oy;
    let max_h = eff_h * MAX_HEIGHT_FRAC;
    let dt = time.delta_secs();
    let active = wave_active(*mode, &vis);

    // Save last frame's heights for the velocity field, then compute targets.
    // Reborrow so the two fields can be split-borrowed past `ResMut`'s Deref.
    let surface = surface.into_inner();
    surface.prev.copy_from_slice(&surface.heights);

    let alpha = if settings.bar_smoothing > 0.0 {
        1.0 - (-dt / settings.bar_smoothing).exp()
    } else {
        1.0
    };

    let n = cava.bars_per_channel;
    if !active || n == 0 {
        // Relax the surface flat to the floor when inactive.
        for hgt in &mut surface.heights {
            *hgt += (floor - *hgt) * alpha;
        }
    } else {
        // Same value array the rendered Wave line is built from (monstercat,
        // mirror and `reverse_order` all applied).
        let values = mirror_values(&cava, &vis, n);
        for s in 0..SAMPLES {
            let v = sample_height(&values, s).clamp(0.0, 1.5);
            let target = floor + v * max_h;
            surface.heights[s] += (target - surface.heights[s]) * alpha;
        }
    }

    // Rebuild the heightfield collider to match (x spans the inset draw width,
    // centered at the horizontal offset); park the whole body offscreen while
    // Wave isn't the active mode. Skip the expensive collider/BVH rebuild once
    // parked and fully relaxed to the floor — otherwise every non-Wave mode would
    // reconstruct an unused heightfield ~60×/sec.
    if let Ok((mut collider, mut transform)) = body.single_mut() {
        let settled = !active && surface.heights.iter().all(|hh| (hh - floor).abs() <= 0.5);
        if !settled {
            *collider = Collider::heightfield(surface.heights.clone(), Vec2::new(eff_w, 1.0));
        }
        transform.translation.x = if active { ox } else { 0.0 };
        transform.translation.y = if active { 0.0 } else { -PARKED };
    }
}

/// Push balls **perpendicular to the local Wave slope** when the surface rises
/// into them, and **unstick** any ball the surface has swallowed. Direction is
/// the heightfield normal `n = (-dh/dx, 1)`; speed is the column rise rate
/// `dh/dt`, scaled by `bar_push`.
fn push_balls(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    vis: Res<VisSettings>,
    time: Res<Time>,
    windows: Query<&Window>,
    surface: Res<Surface>,
    mut balls: Query<(&mut Transform, &mut LinearVelocity, &Ball)>,
) {
    if !settings.enabled || !wave_active(*mode, &vis) {
        return;
    }
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    // Sample columns across the same inset draw region the surface was built on.
    let eff_w = (window.width() - 2.0 * vis.area_margin).max(1.0);
    let left = -eff_w * 0.5 + vis.area_offset.x * window.width() * 0.5;
    let dx = eff_w / (SAMPLES - 1) as f32;

    for (mut transform, mut vel, ball) in &mut balls {
        let (x, y) = (transform.translation.x, transform.translation.y);
        // Map x to a column index with neighbours for the slope.
        let f = (x - left) / eff_w * (SAMPLES - 1) as f32;
        if f < 1.0 || f > (SAMPLES - 2) as f32 {
            continue;
        }
        let i = f.round() as usize;
        let surf_y = surface.heights[i];
        let dist = y - surf_y;

        // Surface rise speed at this column.
        let rise = (surface.heights[i] - surface.prev[i]) / dt;

        // Unstick: the whole ball is below the surface → lift it back on top and
        // carry it upward at least as fast as the surface is rising.
        if dist < -ball.radius {
            transform.translation.y = surf_y + ball.radius;
            let up = rise.max(0.0).max(120.0);
            vel.0.y = vel.0.y.max(up);
            continue;
        }

        // Otherwise, launch only when in the contact band and the surface rises.
        if dist > ball.radius + 4.0 || rise <= 0.0 {
            continue;
        }

        // Local outward normal from the slope, and the rise projected onto it.
        let slope = (surface.heights[i + 1] - surface.heights[i - 1]) / (2.0 * dx);
        let n = Vec2::new(-slope, 1.0).normalize();
        let target_n = rise * n.y * settings.bar_push;

        // Add only the shortfall along n, so we launch without compounding.
        let along = vel.0.dot(n);
        if target_n > along {
            vel.0 += n * (target_n - along);
        }
    }
}

/// Grow or shrink the column-collider pool to match the live bar count, mirroring
/// [`bars::reconcile_bars`](crate::vis::bars). Indices stay contiguous `0..target`.
fn reconcile_columns(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    cava: Res<Cava>,
    columns: Query<(Entity, &BarColumn)>,
) {
    if !settings.enabled {
        return;
    }
    let target = cava.bars_per_channel.max(1);
    let current = columns.iter().count();
    if current < target {
        for i in current..target {
            spawn_column(&mut commands, &settings, i);
        }
    } else if current > target {
        for (entity, col) in &columns {
            if col.0 >= target {
                commands.entity(entity).despawn();
            }
        }
    }
}

/// Spawn one parked kinematic column collider for bar index `i`.
fn spawn_column(commands: &mut Commands, settings: &PhysicsSettings, i: usize) {
    commands.spawn((
        RigidBody::Kinematic,
        Collider::rectangle(1.0, 1.0),
        Restitution::new(settings.bar_restitution),
        Transform::from_xyz(PARKED, PARKED, 0.0),
        BarColumn(i),
    ));
}

/// Reshape and reposition each column collider onto the rendered Bars/Levels mesh
/// (reusing [`Layout`]/[`column_geom`] so the collider can't drift from the bar),
/// caching each column top for [`push_columns`]. Parks the whole pool when the
/// column shapes aren't the active mode.
fn update_columns(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    columns: ResMut<Columns>,
    mut bodies: Query<(&BarColumn, &mut Collider, &mut Transform)>,
) {
    if !settings.enabled {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let columns = columns.into_inner();

    if !columns_active(*mode, &vis) {
        for (_, _, mut transform) in &mut bodies {
            transform.translation = Vec3::new(PARKED, PARKED, 0.0);
        }
        // Keep tops flat so re-entry sees zero delta.
        columns.prev.copy_from_slice(&columns.tops);
        return;
    }

    let (w, h) = (window.width(), window.height());
    let n = cava.bars_per_channel;
    if n == 0 {
        // No audio this frame: keep prev synced to tops so the next non-empty
        // frame doesn't divide a multi-frame top delta by one frame's dt (which
        // would fling resting balls upward).
        columns.prev.copy_from_slice(&columns.tops);
        return;
    }
    // Read the bars exactly as `bars::update_bars` draws them: same per-bar
    // values (monstercat + `reverse_order`) and the same `area_margin`/
    // `area_offset` layout, so the colliders can't drift from the meshes.
    let values = mirror_values(&cava, &vis, n);
    let lyt = Layout::new_with_margin(w, h, n, false, vis.area_margin, vis.area_offset);

    if columns.tops.len() != n {
        columns.tops = vec![lyt.floor; n];
        columns.prev = columns.tops.clone();
    }
    columns.prev.copy_from_slice(&columns.tops);

    let levels = mode.shape() == VisShape::Levels;
    for (col, mut collider, mut transform) in &mut bodies {
        let i = col.0;
        if i >= n {
            transform.translation = Vec3::new(PARKED, PARKED, 0.0);
            continue;
        }
        let mut v = values.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.5);
        if levels {
            // Snap the height to discrete VU-style steps, matching the renderer.
            v = (v * LEVEL_STEPS).round() / LEVEL_STEPS;
        }
        let bar_h = (v * lyt.max_h).max(1.0);
        let (cy, half) = column_geom(&lyt, bar_h, false);
        *collider = Collider::rectangle(half.x * 2.0, half.y * 2.0);
        transform.translation = Vec3::new(lyt.bar_x(i), cy, 0.0);
        columns.tops[i] = lyt.floor + bar_h;
    }
}

/// Launch balls a rising column drives into (straight up — columns only move
/// vertically), and unstick any a fast column has swallowed. Balls horizontally
/// in a gap between bars are ignored: the bottom wall (floor) holds them.
fn push_columns(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    vis: Res<VisSettings>,
    time: Res<Time>,
    windows: Query<&Window>,
    columns: Res<Columns>,
    mut balls: Query<(&mut Transform, &mut LinearVelocity, &Ball)>,
) {
    if !settings.enabled || !columns_active(*mode, &vis) {
        return;
    }
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());
    let n = columns.tops.len();
    if n == 0 {
        return;
    }
    let lyt = Layout::new_with_margin(w, h, n, false, vis.area_margin, vis.area_offset);

    for (mut transform, mut vel, ball) in &mut balls {
        let (x, y) = (transform.translation.x, transform.translation.y);
        let slot = ((x - lyt.left) / lyt.slot_w).floor();
        if slot < 0.0 || slot as usize >= n {
            continue;
        }
        let i = slot as usize;
        // Ignore balls horizontally over a gap — they belong to the floor.
        if (x - lyt.bar_x(i)).abs() > lyt.bar_w * 0.5 + ball.radius {
            continue;
        }
        let top = columns.tops[i];
        let rise = (columns.tops[i] - columns.prev[i]) / dt;
        let dist = y - top;

        // Unstick: ball center sits inside the column → lift it onto the top and
        // carry it up at least as fast as the column is rising.
        if dist < -ball.radius {
            transform.translation.y = top + ball.radius;
            let up = rise.max(0.0).max(120.0);
            vel.0.y = vel.0.y.max(up);
            continue;
        }

        // Otherwise launch only when resting in the contact band and rising.
        if dist > ball.radius + 4.0 || rise <= 0.0 {
            continue;
        }
        let target = rise * settings.bar_push;
        if target > vel.0.y {
            vel.0.y = target;
        }
    }
}

/// Switch global gravity by mode: off in planet (WaveCircle) mode, where
/// [`planet_forces`] applies radial gravity toward the center instead; downward
/// everywhere else (box modes, and the inert non-Wave circle shapes).
fn update_gravity_mode(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    mut gravity: ResMut<Gravity>,
) {
    if !settings.enabled {
        return;
    }
    let target = if planet_active(*mode) {
        Vec2::ZERO
    } else {
        Vec2::NEG_Y * settings.gravity
    };
    // Only write when it actually changes, to avoid per-frame change-detection churn.
    if gravity.0 != target {
        gravity.0 = target;
    }
}

/// Rebuild the planet blob collider from the rendered [`blob_ring`], cache its
/// per-segment radii (this frame + last) for the radial forces, and park the body
/// offscreen while a box mode is active.
fn update_planet(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    windows: Query<&Window>,
    planet: ResMut<Planet>,
    mut body: Query<(&mut Collider, &mut Transform), With<PlanetBody>>,
) {
    if !settings.enabled {
        return;
    }
    let Ok((mut collider, mut transform)) = body.single_mut() else {
        return;
    };
    if !planet_active(*mode) {
        transform.translation = Vec3::new(PARKED, PARKED, 0.0);
        return;
    }
    transform.translation = Vec3::ZERO;

    let Some(window) = windows.iter().next() else {
        return;
    };
    let extent = window.width().min(window.height());
    let mut values = cava.mono();
    if values.is_empty() {
        return;
    }
    spread_monstercat(&mut values, vis.monstercat);
    let ring = blob_ring(&values, extent, vis.inner_radius, vis.rotation);
    let n = ring.len();
    if n < 3 {
        return;
    }

    // Cache radii (this frame + last) for the radial velocity field, and the
    // closed-loop indices (rebuilt only when the segment count changes).
    let planet = planet.into_inner();
    if planet.radii.len() != n {
        planet.radii = ring.iter().map(|p| p.length()).collect();
        planet.prev = planet.radii.clone();
        planet.indices = (0..n as u32).map(|k| [k, (k + 1) % n as u32]).collect();
    } else {
        planet.prev.copy_from_slice(&planet.radii);
        for (i, p) in ring.iter().enumerate() {
            planet.radii[i] = p.length();
        }
    }

    // Closed-loop polyline matching the rendered blob (positions change each
    // frame, so the BVH rebuild is unavoidable; the index list is reused).
    *collider = Collider::polyline(ring, Some(planet.indices.clone()));
}

/// Planet-mode radial forces: pull every ball toward the center, fling balls the
/// expanding blob touches outward along the radius, and unstick any it swallows.
fn planet_forces(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    time: Res<Time>,
    planet: Res<Planet>,
    mut balls: Query<(&mut Transform, &mut LinearVelocity, &Ball)>,
) {
    if !settings.enabled || !planet_active(*mode) {
        return;
    }
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let n = planet.radii.len();
    for (mut transform, mut vel, ball) in &mut balls {
        let pos = transform.translation.truncate();
        let r = pos.length();
        if r < 1.0 {
            continue;
        }
        let outward = pos / r;

        // Central gravity: accelerate toward the center.
        vel.0 -= outward * settings.central_gravity * dt;

        if n < 3 {
            continue;
        }
        // Blob radius in this ball's direction (invert `ring_point`'s mapping:
        // angle = t·TAU − π/2).
        let ang = pos.y.atan2(pos.x);
        let t = ((ang + std::f32::consts::FRAC_PI_2) / std::f32::consts::TAU).rem_euclid(1.0);
        let k = ((t * n as f32).round() as usize) % n;
        let surf_r = planet.radii[k];
        let expand = (planet.radii[k] - planet.prev[k]) / dt;

        // Unstick: the blob is solid, so a ball whose center has crossed *inside*
        // the ring at all should never be there. Eject it the moment it crosses,
        // not after a full radius — otherwise central gravity keeps dragging it
        // deeper until it slips under the border and jitters there. Push it back
        // out onto the rim and remove any inward velocity.
        if r < surf_r {
            transform.translation =
                (outward * (surf_r + ball.radius)).extend(transform.translation.z);
            let push = expand.max(0.0).max(120.0);
            let along = vel.0.dot(outward);
            if push > along {
                vel.0 += outward * (push - along);
            }
            continue;
        }
        // Contact band + expanding blob → fling outward.
        if (r - surf_r).abs() < ball.radius + 4.0 && expand > 0.0 {
            let target = expand * settings.bar_push;
            let along = vel.0.dot(outward);
            if target > along {
                vel.0 += outward * (target - along);
            }
        }
    }
}

/// Extend each ball's trail with its current position (or retract it when the
/// ball is at rest), rebuild the fading feathered stroke, and reap trails whose
/// ball has been despawned. The per-point alpha ramps 0 → 1 from tail to head,
/// so the trail fades out behind the ball and blooms via the HDR camera.
fn update_trails(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    balls: Query<&Transform, With<Ball>>,
    mut trails: Query<(Entity, &mut Trail, &Mesh2d)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let max_len = settings.trail_length.max(2);
    for (entity, mut trail, mesh2d) in &mut trails {
        // Reap the trail once its ball is gone (covers every despawn path).
        let Ok(ball_tf) = balls.get(trail.ball) else {
            commands.entity(entity).despawn();
            continue;
        };
        let pos = ball_tf.translation.truncate();

        // Record a new head when the ball moved; otherwise retract the tail so a
        // resting ball's trail shrinks away instead of lingering as a smear.
        let moved = trail
            .points
            .back()
            .is_none_or(|&last| last.distance_squared(pos) > TRAIL_MIN_STEP_SQ);
        if moved {
            trail.points.push_back(pos);
        } else {
            trail.points.pop_front();
        }
        while trail.points.len() > max_len {
            trail.points.pop_front();
        }

        let m = trail.points.len();
        let pts: Vec<(Vec2, Color)> = trail
            .points
            .iter()
            .enumerate()
            .map(|(i, &p)| (p, trail.color.with_alpha((i + 1) as f32 / m as f32)))
            .collect();
        if let Some(mesh) = meshes.get_mut(&mesh2d.0) {
            // Taper the stroke to a point at the tail and full width at the head
            // (the ball end), so the trail reads as a triangle / comet tail.
            apply_stroke_tapered(mesh, &pts, 0.0, trail.half_width, STROKE_FEATHER, false);
        }
    }
}

/// Toggle the collider debug draw with **F3** (suppressed while egui has the
/// keyboard, so typing in the editor doesn't flip it).
fn toggle_physics_debug(
    keys: Res<ButtonInput<KeyCode>>,
    editor: Res<EditorState>,
    mut settings: ResMut<PhysicsSettings>,
) {
    if !editor.capture_keyboard && keys.just_pressed(KeyCode::F3) {
        settings.debug_draw = !settings.debug_draw;
    }
}

/// Mirror `[physics] debug_draw` onto avian's gizmo config, drawing only the
/// collider wireframes (the axis/joint/contact gizmos stay off).
fn sync_physics_debug(settings: Res<PhysicsSettings>, mut store: ResMut<GizmoConfigStore>) {
    let (config, gizmos) = store.config_mut::<PhysicsGizmos>();
    config.enabled = settings.enabled && settings.debug_draw;
    if config.enabled {
        gizmos.collider_color = Some(Color::srgb(0.2, 1.0, 0.4));
        gizmos.axis_lengths = None;
        gizmos.aabb_color = None;
    }
}
