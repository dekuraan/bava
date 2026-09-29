// SPDX-License-Identifier: MIT OR Apache-2.0
//! The "juice" layer on top of the visualizers: shaders, particles and camera
//! effects keyed to the music.
//!
//! None of it changes what the spectrum or the physics *do* — the blob is the
//! same geometry and the balls orbit exactly as before. It changes how they
//! *look* and how the whole frame reacts to the beat:
//!
//! - **Plasma fill** (`shaders/blob.wgsl`): the WaveCircle interior becomes a
//!   domain-warped plasma in the live palette whose filaments swell on the bass.
//! - **Halo + corona** (`shaders/halo.wgsl`): an additive glow that hugs the
//!   blob's actual rim shape, throwing noisy streaks outward.
//! - **Backdrop** (`shaders/backdrop.wgsl`): parallax starfield + a faint nebula
//!   blended additively over the album art.
//! - **Glossy balls** (`shaders/ball.wgsl`): lit spheres with a specular
//!   highlight and a fresnel rim light. Balls share a small bank of
//!   palette-bucketed materials ([`BallLooks`]) instead of one material each.
//! - **Particles** ([`particles`]): plasma flares shed from the loudest parts of
//!   the rim, sparks where a ball is struck hard, and a shockwave ring off the
//!   blob on every detected beat — all accumulated into one additive mesh.
//! - **Camera** ([`camera`]): a zoom punch and a small shake on the beat, plus
//!   chromatic aberration and a vignette from Bevy's post-process stack.
//!
//! Every piece is individually tunable from `[fx]` (and the editor), and
//! `[fx] enabled = false` returns the classic look.

pub mod camera;
pub mod material;
pub mod particles;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::vis::circle::BlobShape;
use crate::vis::features::{FeaturesPlugin, FeaturesSet};
use crate::vis::{DrawingMode, VisFamily, VisSettings, sample_gradient};
use material::{
    BACKDROP_SHADER, BALL_SHADER, FxBlend, FxMaterial, FxMaterialPlugin, FxSyncSet, HALO_SHADER,
};

/// Live effect tunables — also the `[fx]` table of the config file, verbatim
/// (every field is optional there; [`sanitized`](Self::sanitized) clamps a
/// hand-edited file into range).
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FxSettings {
    /// Master switch; off restores the classic flat look.
    pub enabled: bool,
    /// Plasma shader for the blob fill (off: the flat translucent tint).
    pub plasma: bool,
    /// Opacity of the plasma fill, 0..1.
    pub blob_opacity: f32,
    /// Halo glow intensity around the blob (0 = off).
    pub halo: f32,
    /// Corona streak amount in the halo (0 = a plain glow).
    pub corona: f32,
    /// Starfield + nebula backdrop.
    pub backdrop: bool,
    /// Star density, 0..1.
    pub stars: f32,
    /// Nebula intensity (0 = stars only).
    pub nebula: f32,
    /// Shockwave rings off the blob on each beat.
    pub shockwaves: bool,
    /// Plasma flares shed from the rim; a rate multiplier (0 = off).
    pub flares: f32,
    /// Sparks where balls are struck hard.
    pub sparks: bool,
    /// Lit, glossy balls (off: flat discs).
    pub glossy_balls: bool,
    /// Beat zoom punch, as a fraction of the view (0 = off).
    pub punch: f32,
    /// Beat camera shake amplitude, in pixels (0 = off).
    pub shake: f32,
    /// Chromatic aberration on the beat (0 = off).
    pub chromatic: f32,
    /// Vignette darkening, 0..1 (0 = off).
    pub vignette: f32,
    /// Album-art backdrop zoom on the bass, as a fraction (0 = off).
    pub art_zoom: f32,
}

impl Default for FxSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            plasma: true,
            blob_opacity: 0.6,
            halo: 1.0,
            corona: 1.0,
            backdrop: true,
            stars: 0.35,
            nebula: 1.0,
            shockwaves: true,
            flares: 1.0,
            sparks: true,
            glossy_balls: true,
            punch: 0.02,
            shake: 3.0,
            chromatic: 0.012,
            vignette: 0.35,
            art_zoom: 0.035,
        }
    }
}

impl FxSettings {
    /// Clamp every tunable into the range the editor offers, and replace
    /// non-finite values with the defaults.
    pub fn sanitized(&self) -> Self {
        let d = Self::default();
        let clamp = |v: f32, lo: f32, hi: f32, def: f32| {
            if v.is_finite() { v.clamp(lo, hi) } else { def }
        };
        Self {
            blob_opacity: clamp(self.blob_opacity, 0.0, 1.0, d.blob_opacity),
            halo: clamp(self.halo, 0.0, 4.0, d.halo),
            corona: clamp(self.corona, 0.0, 4.0, d.corona),
            stars: clamp(self.stars, 0.0, 1.0, d.stars),
            nebula: clamp(self.nebula, 0.0, 4.0, d.nebula),
            flares: clamp(self.flares, 0.0, 4.0, d.flares),
            punch: clamp(self.punch, 0.0, 0.2, d.punch),
            shake: clamp(self.shake, 0.0, 40.0, d.shake),
            chromatic: clamp(self.chromatic, 0.0, 0.1, d.chromatic),
            vignette: clamp(self.vignette, 0.0, 1.0, d.vignette),
            art_zoom: clamp(self.art_zoom, 0.0, 0.3, d.art_zoom),
            ..self.clone()
        }
    }
}

/// How many palette buckets the balls are spread over. Each bucket is one
/// shared [`FxMaterial`]; at 16 the steps are far finer than a two-to-five-stop
/// gradient can show.
pub(crate) const BALL_BUCKETS: usize = 16;

/// The shared ball materials, one per palette bucket.
///
/// Balls used to carry one `ColorMaterial` each, all rewritten whenever the
/// palette moved. A ball's *mesh* stays its own (it is rigid, so it is never
/// re-uploaded — see the batching note in AGENTS.md), but its look only depends
/// on where it sits in the palette, so sixteen materials cover every ball:
/// a palette fade rewrites 16 small uniforms rather than one per ball, and
/// balls in the same bucket can share a bind group.
///
/// The buckets are deliberately **not live** ([`ball_material`]): a modified
/// material re-specializes and re-queues every mesh drawn with it, so a
/// per-frame audio refresh would cost O(balls) render work every frame even
/// with nothing changing on screen. `ball.wgsl` therefore reads only the
/// bucket's color and style, and [`update_ball_looks`] writes a bucket only
/// when its look actually changes.
#[derive(Resource, Clone)]
pub(crate) struct BallLooks {
    handles: Vec<Handle<FxMaterial>>,
}

impl BallLooks {
    /// The material for a ball at palette position `tint` (0..1).
    pub(crate) fn material(&self, tint: f32) -> Handle<FxMaterial> {
        self.handles[bucket(tint)].clone()
    }
}

/// A ball bucket's material: the glossy shader, opted out of the per-frame
/// sync (see [`BallLooks`]).
fn ball_material() -> FxMaterial {
    let mut material = FxMaterial::new(BALL_SHADER);
    material.live = false;
    material
}

/// Palette bucket index for `tint`.
fn bucket(tint: f32) -> usize {
    (tint.clamp(0.0, 1.0) * (BALL_BUCKETS - 1) as f32).round() as usize
}

/// Palette position a bucket is drawn at.
fn bucket_tint(i: usize) -> f32 {
    i as f32 / (BALL_BUCKETS - 1) as f32
}

/// Marks the blob's halo quad.
#[derive(Component)]
struct Halo;

/// Marks the full-window backdrop quad.
#[derive(Component)]
struct Backdrop;

/// Handles for the effect quads' materials.
#[derive(Resource)]
struct FxHandles {
    halo: Handle<FxMaterial>,
    backdrop: Handle<FxMaterial>,
}

/// The halo's material, for scenes that swap its shader.
pub(crate) fn halo_material(world: &World) -> Option<Handle<FxMaterial>> {
    world.get_resource::<FxHandles>().map(|h| h.halo.clone())
}

/// The backdrop's material, for scenes that swap its shader.
pub(crate) fn backdrop_material(world: &World) -> Option<Handle<FxMaterial>> {
    world
        .get_resource::<FxHandles>()
        .map(|h| h.backdrop.clone())
}

/// Effects plugin: materials, audio features, and every effect system.
pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FxSettings>()
            .add_plugins((FeaturesPlugin, FxMaterialPlugin));

        // Created at build time so ball spawning (which may run on the very
        // first Update) always finds them.
        let handles = {
            let mut materials = app.world_mut().resource_mut::<Assets<FxMaterial>>();
            (0..BALL_BUCKETS)
                .map(|_| materials.add(ball_material()))
                .collect()
        };
        app.insert_resource(BallLooks { handles })
            .add_systems(Startup, setup_fx)
            .add_systems(
                Update,
                (update_halo, update_backdrop, update_ball_looks)
                    .after(FxSyncSet)
                    .after(FeaturesSet)
                    .after(crate::vis::circle::BlobShapeSet),
            )
            .add_plugins((particles::ParticlesPlugin, camera::FxCameraPlugin));
    }
}

fn setup_fx(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<FxMaterial>>,
) {
    let quad = meshes.add(Rectangle::new(1.0, 1.0));

    // Behind the fill (-5) and the ring, above the album art (-10).
    let halo = materials.add(FxMaterial::new(HALO_SHADER).with_blend(FxBlend::Additive));
    commands.spawn((
        Mesh2d(quad.clone()),
        MeshMaterial2d(halo.clone()),
        Transform::from_xyz(0.0, 0.0, -6.0),
        Visibility::Hidden,
        Halo,
    ));

    // Above the album art (-10) and user background (-12), so the stars read
    // over a cover too; additive, so it only ever adds light.
    let backdrop = materials.add(FxMaterial::new(BACKDROP_SHADER).with_blend(FxBlend::Additive));
    commands.spawn((
        Mesh2d(quad),
        MeshMaterial2d(backdrop.clone()),
        Transform::from_xyz(0.0, 0.0, -9.0),
        Visibility::Hidden,
        Backdrop,
    ));
    commands.insert_resource(FxHandles { halo, backdrop });
}

/// Size, show and feed the halo: it follows the blob in every circle mode.
fn update_halo(
    fx: Res<FxSettings>,
    mode: Res<DrawingMode>,
    shape: Res<BlobShape>,
    handles: Res<FxHandles>,
    mut materials: ResMut<Assets<FxMaterial>>,
    mut q: Query<(&mut Transform, &mut Visibility), With<Halo>>,
) {
    let active = fx.enabled && fx.halo > 0.0 && mode.family() == VisFamily::Circle && shape.active;
    let Ok((mut transform, mut visibility)) = q.single_mut() else {
        return;
    };
    visibility.set_if_neq(if active {
        Visibility::Visible
    } else {
        Visibility::Hidden
    });
    if !active {
        return;
    }
    // Enough room for the widest glow and the longest corona streaks (the
    // shader fades out over the outer 30% of the quad).
    let size = 2.0 * (shape.max_radius() + shape.base.max(40.0) * 2.4);
    transform.scale = Vec3::new(size, size, 1.0);
    if let Some(mut m) = materials.get_mut(&handles.halo) {
        m.uniform.set_shape(&shape.radii);
        m.uniform.info.z = shape.base;
        m.uniform.info.w = shape.rotation;
        m.uniform.params[0] = Vec4::new(fx.halo, fx.corona, 1.0, size * 0.5);
    }
}

/// Cover the window with the backdrop (with slack for the camera shake).
fn update_backdrop(
    fx: Res<FxSettings>,
    windows: Query<&Window>,
    handles: Res<FxHandles>,
    mut materials: ResMut<Assets<FxMaterial>>,
    mut q: Query<(&mut Transform, &mut Visibility), With<Backdrop>>,
) {
    let Ok((mut transform, mut visibility)) = q.single_mut() else {
        return;
    };
    let active = fx.enabled && fx.backdrop;
    visibility.set_if_neq(if active {
        Visibility::Visible
    } else {
        Visibility::Hidden
    });
    if !active {
        return;
    }
    let Some(window) = windows.iter().next() else {
        return;
    };
    let size = Vec3::new(window.width() * 1.15, window.height() * 1.15, 1.0);
    if transform.scale != size {
        transform.scale = size;
    }
    if let Some(mut m) = materials.get_mut(&handles.backdrop) {
        m.uniform.params[0] = Vec4::new(fx.stars, fx.nebula, 1.0, 0.0);
    }
}

/// Recolor the ball buckets when the palette or glow changes, and flip them
/// between the glossy and flat looks. Only buckets whose look really moved are
/// written: `VisSettings` changes for plenty of unrelated edits, and every
/// modified bucket re-specializes every ball drawn with it.
fn update_ball_looks(
    fx: Res<FxSettings>,
    vis: Res<VisSettings>,
    looks: Res<BallLooks>,
    mut materials: ResMut<Assets<FxMaterial>>,
    mut primed: Local<bool>,
) {
    if *primed && !vis.is_changed() && !fx.is_changed() {
        return;
    }
    *primed = true;
    let stops = vis.fg_stops();
    let style = if fx.enabled && fx.glossy_balls {
        1.0
    } else {
        0.0
    };
    for (i, handle) in looks.handles.iter().enumerate() {
        let color = sample_gradient(&stops, bucket_tint(i), vis.glow_gain)
            .to_linear()
            .to_vec4();
        // Compared through `Deref`; only the write below flags the asset.
        if let Some(mut m) = materials.get_mut(handle)
            && (m.uniform.color != color || m.uniform.params[0].x != style)
        {
            m.uniform.color = color;
            m.uniform.params[0].x = style;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_cover_the_palette_ends() {
        assert_eq!(bucket(0.0), 0);
        assert_eq!(bucket(1.0), BALL_BUCKETS - 1);
        assert_eq!(bucket(-3.0), 0);
        assert_eq!(bucket(7.0), BALL_BUCKETS - 1);
        for i in 0..BALL_BUCKETS {
            assert_eq!(bucket(bucket_tint(i)), i, "bucket {i} round-trips");
        }
    }

    #[test]
    fn ball_buckets_read_no_per_frame_uniform_fields() {
        // The buckets never get the per-frame refresh, so the shader must not
        // read anything only that refresh keeps current, or balls freeze.
        assert!(!ball_material().live);
        let src = include_str!("shaders/ball.wgsl");
        for field in ["fx.clock", "fx.audio", "fx.palette", "fx.info", "palette("] {
            assert!(!src.contains(field), "ball.wgsl reads per-frame `{field}`");
        }
    }

    /// `AssetEvent::Modified`s seen since the last drain.
    #[derive(Resource, Default)]
    struct Modified(usize);

    fn count_modified(
        mut events: MessageReader<AssetEvent<FxMaterial>>,
        mut out: ResMut<Modified>,
    ) {
        out.0 += events
            .read()
            .filter(|e| matches!(e, AssetEvent::Modified { .. }))
            .count();
    }

    fn drain_modified(app: &mut App) -> usize {
        std::mem::take(&mut app.world_mut().resource_mut::<Modified>().0)
    }

    #[test]
    fn ball_looks_write_only_buckets_whose_look_changed() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<FxMaterial>()
            .init_resource::<FxSettings>()
            .init_resource::<VisSettings>()
            .init_resource::<Modified>()
            .add_systems(Update, update_ball_looks)
            .add_systems(Last, count_modified);
        let handles = {
            let mut materials = app.world_mut().resource_mut::<Assets<FxMaterial>>();
            (0..BALL_BUCKETS)
                .map(|_| materials.add(ball_material()))
                .collect()
        };
        app.insert_resource(BallLooks { handles });

        app.update();
        assert_eq!(
            drain_modified(&mut app),
            BALL_BUCKETS,
            "first pass colors all"
        );
        app.update();
        assert_eq!(drain_modified(&mut app), 0);

        // An edit that doesn't move the palette touches nothing.
        app.world_mut().resource_mut::<VisSettings>().set_changed();
        app.update();
        assert_eq!(drain_modified(&mut app), 0, "unchanged buckets rewritten");

        // Flipping to the flat look changes every bucket.
        app.world_mut().resource_mut::<FxSettings>().glossy_balls = false;
        app.update();
        assert_eq!(drain_modified(&mut app), BALL_BUCKETS);
    }

    #[test]
    fn sanitized_clamps_hand_edited_values() {
        let wild = FxSettings {
            blob_opacity: 7.0,
            shake: f32::NAN,
            punch: -1.0,
            ..FxSettings::default()
        };
        let s = wild.sanitized();
        assert_eq!(s.blob_opacity, 1.0);
        assert_eq!(s.shake, FxSettings::default().shake);
        assert_eq!(s.punch, 0.0);
    }

    #[test]
    fn defaults_are_on_and_sane() {
        let fx = FxSettings::default();
        assert!(fx.enabled && fx.plasma && fx.backdrop);
        assert!((0.0..=1.0).contains(&fx.blob_opacity));
        assert!(fx.punch < 0.1, "punch is a small fraction of the view");
    }
}
