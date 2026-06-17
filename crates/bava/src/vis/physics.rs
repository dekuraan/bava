//! Physics playground layered over the visualizers (avian2d).
//!
//! **Box mode** (bars): left-click spawns a ball that falls under gravity,
//! bounces off the window walls, and gets launched by the spectrum. Instead of
//! one collider per bar, the spectrum is a single **smooth kinematic heightfield**
//! — a continuous blobby curve rebuilt every frame from the (interpolated,
//! time-smoothed) cava values. avian resolves contacts along the *true* surface
//! normal, so balls are pushed **perpendicular to the local slope**, not just up.
//!
//! Because one rigid body can carry only one velocity, the heightfield can't
//! itself impart per-column launch energy. So a [`push_balls`] pass reads the
//! analytic surface-velocity field (how fast each column is rising) and adds an
//! impulse **along the local surface normal** — direction from the slope, speed
//! from the rise rate. Shape comes from the engine, energy is scripted, and both
//! agree on the same normal.
//!
//! **Planet mode** (circle visualizer): global gravity is switched off and each
//! ball is pulled toward the center instead, so balls fall in and **orbit/bounce
//! the pulsing blob**. The blob is a kinematic [`Collider::polyline`] ring rebuilt
//! every frame from the same [`blob_ring`] the renderer draws, so the collision
//! shape tracks the visual. When the blob expands it flings balls radially
//! outward (and unsticks any it swallows), mirroring the bar surface. Clicking in
//! this mode spawns balls with a tangential velocity so they orbit immediately.
//!
//! Physics runs in [`PostUpdate`] with a variable timestep (not avian's default
//! fixed `FixedPostUpdate`) so it steps once per render frame, in lockstep with
//! the per-frame cava analysis. Balls carry [`SweptCcd`] so they don't tunnel.

use avian2d::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::cava::Cava;
use crate::gui::EditorState;
use crate::vis::circle::blob_ring;
use crate::vis::{gradient_color, spread_monstercat, DrawingMode, VisFamily, VisSettings};

/// 1 physics "metre" = this many world pixels. Scales avian's internal
/// tolerances (contact margins etc.) to our pixel-space coordinates.
const LENGTH_UNIT: f32 = 100.0;
/// Thickness of the boundary walls, in pixels.
const WALL_THICKNESS: f32 = 200.0;
/// Must match `bars::MAX_HEIGHT_FRAC`.
const MAX_HEIGHT_FRAC: f32 = 0.9;
/// Horizontal resolution of the heightfield collider and its fill mesh. Higher =
/// smoother curve and finer slope normals.
const SAMPLES: usize = 192;
/// Park an inactive surface/planet body far outside the world.
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
    /// Launch gain: how strongly a rising surface flings balls along its normal.
    pub bar_push: f32,
    /// Planet mode: radial acceleration pulling balls toward the center, px/s².
    pub central_gravity: f32,
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

/// Time-smoothed spectrum surface, shared by the collider/mesh update and the
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

/// Planet-mode blob ring, sampled this frame and last (per-segment radii from the
/// center). Shared by the collider rebuild and the radial ball forces.
#[derive(Resource, Default)]
struct Planet {
    radii: Vec<f32>,
    prev: Vec<f32>,
}

/// The single kinematic polyline body for the planet blob.
#[derive(Component)]
struct PlanetBody;

/// The single kinematic heightfield body for the spectrum surface.
#[derive(Component)]
struct SurfaceBody;

/// The smooth filled visual for the spectrum surface.
#[derive(Component)]
struct SurfaceFill;

/// Mesh/material handles for the surface fill, updated each frame.
#[derive(Resource)]
struct FillHandles {
    mesh: Handle<Mesh>,
    material: Handle<ColorMaterial>,
}

/// Which boundary a wall is, so it can be repositioned on resize.
#[derive(Component, Clone, Copy)]
enum WallSide {
    Left,
    Right,
    Top,
    Bottom,
}

/// Physics plugin: avian + ball spawning + the smooth spectrum surface.
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhysicsSettings>()
            .init_resource::<BallCounter>()
            .init_resource::<Surface>()
            .init_resource::<Planet>()
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
                (
                    spawn_ball_on_click,
                    enforce_ball_cap,
                    despawn_escaped_balls,
                    resize_walls,
                    update_gravity_mode,
                    // Update each surface before reading it to move balls.
                    (update_surface, push_balls).chain(),
                    (update_planet, planet_forces).chain(),
                ),
            );
    }
}

/// Spawn the boundary walls, the kinematic heightfield surface, and its fill.
fn setup_physics(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
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

    // Kinematic heightfield surface, flat at the floor until the first update.
    let floor = -h / 2.0;
    commands.spawn((
        RigidBody::Kinematic,
        Collider::heightfield(vec![floor; SAMPLES], Vec2::new(w, 1.0)),
        Restitution::new(settings.bar_restitution),
        Transform::default(),
        SurfaceBody,
    ));

    // Kinematic planet blob, parked offscreen until a circle mode activates it.
    commands.spawn((
        RigidBody::Kinematic,
        Collider::circle(10.0),
        Restitution::new(settings.bar_restitution),
        Transform::from_xyz(PARKED, PARKED, 0.0),
        PlanetBody,
    ));

    // Smooth filled visual for the surface, sitting above the bar sprites and
    // below the balls (z = 0.5).
    let mesh = meshes.add(fill_mesh());
    let material = materials.add(ColorMaterial::from(Color::NONE));
    commands.spawn((
        Mesh2d(mesh.clone()),
        MeshMaterial2d(material.clone()),
        Transform::from_xyz(0.0, 0.0, 0.5),
        Visibility::Hidden,
        SurfaceFill,
    ));
    commands.insert_resource(FillHandles { mesh, material });
}

/// A vertical strip mesh: `SAMPLES` top vertices (the curve) and `SAMPLES` bottom
/// vertices (the floor). Positions are placeholders (overwritten each frame);
/// the triangle indices are fixed.
fn fill_mesh() -> Mesh {
    let positions = vec![[0.0f32, 0.0, 0.0]; 2 * SAMPLES];
    let normals = vec![[0.0f32, 0.0, 1.0]; 2 * SAMPLES];
    let uvs = vec![[0.0f32, 0.0]; 2 * SAMPLES];
    // Vertex layout: top_i = 2*i, bottom_i = 2*i + 1.
    let mut indices = Vec::with_capacity((SAMPLES - 1) * 6);
    for i in 0..SAMPLES - 1 {
        let (t0, b0, t1, b1) = (2 * i as u32, 2 * i as u32 + 1, 2 * (i as u32 + 1), 2 * (i as u32 + 1) + 1);
        indices.extend_from_slice(&[t0, b0, t1, b0, b1, t1]);
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
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
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    vis: Res<VisSettings>,
    editor: Res<EditorState>,
    mut counter: ResMut<BallCounter>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) {
    // Skip when the click lands on the egui settings window.
    if !settings.enabled || editor.capture_pointer || !mouse.just_pressed(MouseButton::Left) {
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

    let color = gradient_color(vis.fg_lo(), vis.fg_hi(), tint);
    let id = counter.0;
    counter.0 += 1;

    // In planet mode, launch tangentially for a near-circular orbit; otherwise
    // drop it in (gravity takes over).
    let velocity = if mode.family() == VisFamily::Circle {
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

    commands.spawn((
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

/// Resolve the smoothed surface height for column `s` (0..SAMPLES) from the cava
/// bar `values`, interpolating smoothly between bars for a blobby curve.
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

/// Rebuild the spectrum surface (heightfield collider + fill mesh) from the
/// latest audio, time-smoothed. Parks flat at the floor while a circle mode is
/// active. Updates the shared [`Surface`] resource for [`push_balls`].
#[allow(clippy::too_many_arguments)]
fn update_surface(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    cava: Res<Cava>,
    vis: Res<VisSettings>,
    time: Res<Time>,
    windows: Query<&Window>,
    surface: ResMut<Surface>,
    handles: Res<FillHandles>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut body: Query<(&mut Collider, &mut Transform), With<SurfaceBody>>,
    mut fill_vis: Query<&mut Visibility, With<SurfaceFill>>,
) {
    if !settings.enabled {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let (w, h) = (window.width(), window.height());
    let floor = -h / 2.0;
    let dt = time.delta_secs();
    let active = mode.family() == VisFamily::Box;

    for mut v in &mut fill_vis {
        *v = if active { Visibility::Visible } else { Visibility::Hidden };
    }

    // Save last frame's heights for the velocity field, then compute targets.
    // Reborrow so the two fields can be split-borrowed past `ResMut`'s Deref.
    let surface = surface.into_inner();
    surface.prev.copy_from_slice(&surface.heights);

    let alpha = if settings.bar_smoothing > 0.0 {
        1.0 - (-dt / settings.bar_smoothing).exp()
    } else {
        1.0
    };

    let mut values = cava.mono();
    if !active || values.is_empty() {
        // Relax the surface flat to the floor when inactive.
        for hgt in &mut surface.heights {
            *hgt += (floor - *hgt) * alpha;
        }
    } else {
        spread_monstercat(&mut values, vis.monstercat);
        let max_h = h * MAX_HEIGHT_FRAC;
        for s in 0..SAMPLES {
            let v = sample_height(&values, s).clamp(0.0, 1.5);
            let target = floor + v * max_h;
            surface.heights[s] += (target - surface.heights[s]) * alpha;
        }
    }

    // Rebuild the heightfield collider to match (x spans [-w/2, w/2]); park the
    // whole body offscreen while a circle mode owns the simulation.
    if let Ok((mut collider, mut transform)) = body.single_mut() {
        *collider = Collider::heightfield(surface.heights.clone(), Vec2::new(w, 1.0));
        transform.translation.y = if active { 0.0 } else { -PARKED };
    }

    // Rebuild the fill mesh (top curve + floor), tinted by loudness.
    if active {
        let mut peak = 0.0f32;
        if let Some(mesh) = meshes.get_mut(&handles.mesh) {
            let mut positions = Vec::with_capacity(2 * SAMPLES);
            for s in 0..SAMPLES {
                let x = -w / 2.0 + s as f32 / (SAMPLES - 1) as f32 * w;
                let y = surface.heights[s];
                peak = peak.max((y - floor) / (h * MAX_HEIGHT_FRAC).max(1.0));
                positions.push([x, y, 0.0]);
                positions.push([x, floor, 0.0]);
            }
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        }
        if let Some(mat) = materials.get_mut(&handles.material) {
            mat.color = gradient_color(vis.fg_lo(), vis.fg_hi(), peak).with_alpha(0.9);
        }
    }
}

/// Push balls **perpendicular to the local surface slope** when the surface is
/// rising into them, and **unstick** any ball the surface has swallowed (a loud
/// column can shoot up past a resting ball, trapping it between the surface and
/// the floor where gravity can't free it). Direction is the heightfield normal
/// `n = (-dh/dx, 1)`; speed is the column rise rate `dh/dt`, scaled by `bar_push`.
fn push_balls(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    time: Res<Time>,
    windows: Query<&Window>,
    surface: Res<Surface>,
    mut balls: Query<(&mut Transform, &mut LinearVelocity, &Ball)>,
) {
    if !settings.enabled || mode.family() != VisFamily::Box {
        return;
    }
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let w = window.width();
    let dx = w / (SAMPLES - 1) as f32;

    for (mut transform, mut vel, ball) in &mut balls {
        let (x, y) = (transform.translation.x, transform.translation.y);
        // Map x to a column index with neighbours for the slope.
        let f = (x + w / 2.0) / w * (SAMPLES - 1) as f32;
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

/// Switch global gravity by mode: downward in box mode; off in planet mode, where
/// [`planet_forces`] applies radial gravity toward the center instead.
fn update_gravity_mode(
    mode: Res<DrawingMode>,
    settings: Res<PhysicsSettings>,
    mut gravity: ResMut<Gravity>,
) {
    if !settings.enabled {
        return;
    }
    let target = if mode.family() == VisFamily::Box {
        Vec2::NEG_Y * settings.gravity
    } else {
        Vec2::ZERO
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
    if mode.family() != VisFamily::Circle {
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
    let ring = blob_ring(&values, extent);
    let n = ring.len();
    if n < 3 {
        return;
    }

    // Cache radii (this frame + last) for the radial velocity field.
    let planet = planet.into_inner();
    if planet.radii.len() != n {
        planet.radii = ring.iter().map(|p| p.length()).collect();
        planet.prev = planet.radii.clone();
    } else {
        planet.prev.copy_from_slice(&planet.radii);
        for (i, p) in ring.iter().enumerate() {
            planet.radii[i] = p.length();
        }
    }

    // Closed-loop polyline matching the rendered blob.
    let indices: Vec<[u32; 2]> = (0..n as u32).map(|k| [k, (k + 1) % n as u32]).collect();
    *collider = Collider::polyline(ring, Some(indices));
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
    if !settings.enabled || mode.family() != VisFamily::Circle {
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

        // Unstick: ball swallowed inside the blob → push it back out onto it.
        if r < surf_r - ball.radius {
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
