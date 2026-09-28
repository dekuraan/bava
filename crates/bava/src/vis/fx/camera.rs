// SPDX-License-Identifier: MIT OR Apache-2.0
//! Beat-driven camera effects: a zoom punch, a small shake, chromatic
//! aberration and a vignette.
//!
//! The punch and shake move the *camera*, never the world, so the physics
//! layout, the colliders and click-to-spawn (which goes through
//! `viewport_to_world_2d`) are unaffected. Chromatic aberration and the vignette
//! are Bevy's own post-process effects on the vis camera.

use bevy::post_process::effect_stack::{ChromaticAberration, Vignette};
use bevy::prelude::*;

use super::FxSettings;
use crate::vis::bars::VisCamera;
use crate::vis::features::{AudioFeatures, FeaturesSet};

pub(crate) struct FxCameraPlugin;

impl Plugin for FxCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (camera_fx, art_zoom).after(FeaturesSet));
    }
}

/// The camera's offset from its rest pose this frame, for a beat `pulse`
/// (1 → 0) at `time` seconds: `(translation, zoom scale)`.
pub(crate) fn beat_offset(fx: &FxSettings, pulse: f32, time: f32) -> (Vec2, f32) {
    if !fx.enabled {
        return (Vec2::ZERO, 1.0);
    }
    let pulse = pulse.clamp(0.0, 1.0);
    // Two incommensurate sines: a jitter that never visibly repeats.
    let jitter = Vec2::new(
        (time * 47.3).sin() + 0.5 * (time * 91.7).sin(),
        (time * 53.9).cos() + 0.5 * (time * 77.1).cos(),
    ) / 1.5;
    let shake = jitter * fx.shake * pulse * pulse;
    let zoom = 1.0 - fx.punch * pulse * pulse;
    (shake, zoom)
}

#[allow(clippy::type_complexity)]
fn camera_fx(
    mut commands: Commands,
    fx: Res<FxSettings>,
    features: Res<AudioFeatures>,
    mut cameras: Query<
        (
            Entity,
            &mut Transform,
            &mut Projection,
            Option<&mut ChromaticAberration>,
            Option<&mut Vignette>,
        ),
        With<VisCamera>,
    >,
) {
    let pulse = features.beat_pulse;
    let (offset, zoom) = beat_offset(&fx, pulse, features.time);
    for (entity, mut transform, mut projection, aberration, vignette) in &mut cameras {
        let at = Vec3::new(offset.x, offset.y, transform.translation.z);
        if transform.translation != at {
            transform.translation = at;
        }
        if let Projection::Orthographic(ortho) = &mut *projection
            && ortho.scale != zoom
        {
            ortho.scale = zoom;
        }

        let chroma = if fx.enabled {
            fx.chromatic * (0.25 + 0.75 * pulse) + fx.chromatic * 0.5 * features.bass
        } else {
            0.0
        };
        match aberration {
            Some(mut a) => {
                if a.intensity != chroma {
                    a.intensity = chroma;
                }
            }
            None if chroma > 0.0 => {
                commands.entity(entity).insert(ChromaticAberration {
                    intensity: chroma,
                    ..default()
                });
            }
            None => {}
        }

        let vig = if fx.enabled { fx.vignette } else { 0.0 };
        match vignette {
            Some(mut v) => {
                if v.intensity != vig {
                    v.intensity = vig;
                }
            }
            None if vig > 0.0 => {
                commands.entity(entity).insert(Vignette {
                    intensity: vig,
                    radius: 0.9,
                    smoothness: 3.0,
                    ..default()
                });
            }
            None => {}
        }
    }
}

/// Breathe the album-art backdrop with the bass.
fn art_zoom(
    fx: Res<FxSettings>,
    features: Res<AudioFeatures>,
    mut art: Query<&mut Transform, With<crate::vis::hud::ArtBackground>>,
) {
    let scale = if fx.enabled {
        1.0 + fx.art_zoom * (0.6 * features.bass.min(1.2) + 0.4 * features.beat_pulse)
    } else {
        1.0
    };
    for mut t in &mut art {
        let s = Vec3::new(scale, scale, 1.0);
        if t.scale != s {
            t.scale = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_rests_without_a_beat_or_when_disabled() {
        let fx = FxSettings::default();
        assert_eq!(beat_offset(&fx, 0.0, 1.23), (Vec2::ZERO, 1.0));
        let off = FxSettings {
            enabled: false,
            ..FxSettings::default()
        };
        assert_eq!(beat_offset(&off, 1.0, 1.23), (Vec2::ZERO, 1.0));
    }

    #[test]
    fn beat_punches_in_and_shake_is_bounded() {
        let fx = FxSettings::default();
        for i in 0..200 {
            let (shake, zoom) = beat_offset(&fx, 1.0, i as f32 * 0.013);
            assert!(shake.length() <= fx.shake * 1.5 + 1e-4);
            assert!((zoom - (1.0 - fx.punch)).abs() < 1e-6);
        }
    }
}
