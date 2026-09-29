// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scene sound effects (`[sounds.<name>]`), fired by triggers.
//!
//! bava visualizes whatever the system is playing, so a scene's own sounds are
//! heard by the loopback capture too. The triggers are chosen with that in
//! mind: clicks, key presses, ball spawns and impacts are user-driven, and
//! `beat` sounds take an `every` divider so a scene can't drown the music or
//! chase its own echo. Offline renders (`--input`) never play scene sounds.

use std::time::Duration;

use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings, Volume};
use bevy::prelude::*;

use crate::gui::EditorState;
use crate::scene::SceneEntity;
use crate::scene::def::{SoundDef, SoundTrigger};
use crate::vis::features::AudioFeatures;
use crate::vis::fx::particles::FxParticles;
use crate::vis::physics::Ball;

/// Loudest linear gain a scene sound plays at.
const MAX_VOLUME: f32 = 2.0;
/// Largest playback-speed variation (a speed of 0 would never finish).
const MAX_JITTER: f32 = 0.9;

/// One loaded scene sound and its trigger state.
pub struct SceneSound {
    /// The definition, with `volume` / `jitter` / `min_interval` sanitized.
    pub def: SoundDef,
    pub handle: Handle<AudioSource>,
    pub key: Option<KeyCode>,
    /// When it last played, on the app clock ([`Time::elapsed`]).
    last: Option<Duration>,
    started: bool,
}

impl SceneSound {
    pub fn new(mut def: SoundDef, handle: Handle<AudioSource>) -> Self {
        let key = def.key.as_deref().and_then(crate::config::parse_key);
        // TOML accepts nan and inf, and `clamp` passes NaN through: a NaN gain
        // writes NaN samples, and a NaN speed plays at a 1 Hz sample rate.
        let defaults = SoundDef::default();
        let finite_or = |v: f32, default: f32| if v.is_finite() { v } else { default };
        def.volume = finite_or(def.volume, defaults.volume).clamp(0.0, MAX_VOLUME);
        def.jitter = finite_or(def.jitter, defaults.jitter).clamp(0.0, MAX_JITTER);
        def.min_interval = finite_or(def.min_interval, defaults.min_interval).max(0.0);
        Self {
            def,
            handle,
            key,
            last: None,
            started: false,
        }
    }

    /// Whether `min_interval` has passed since the last play, at `now`.
    fn rested(&self, now: Duration) -> bool {
        self.last.is_none_or(|last| {
            now.saturating_sub(last).as_secs_f64() >= f64::from(self.def.min_interval)
        })
    }
}

/// The active scene's sounds.
#[derive(Resource, Default)]
pub struct SceneSounds {
    pub sounds: Vec<SceneSound>,
    /// False for offline renders.
    pub enabled: bool,
    rng: Option<fastrand::Rng>,
}

/// Whether `trigger` fires this frame.
#[allow(clippy::too_many_arguments)]
fn fired(
    sound: &SceneSound,
    features: &AudioFeatures,
    spawned: usize,
    impacts: u32,
    clicked: bool,
    keys: &ButtonInput<KeyCode>,
) -> bool {
    match sound.def.trigger {
        SoundTrigger::Start => !sound.started,
        SoundTrigger::Loop => false,
        SoundTrigger::Beat => {
            features.beat
                && features
                    .beats
                    .is_multiple_of(u64::from(sound.def.every.max(1)))
        }
        SoundTrigger::Spawn => spawned > 0,
        SoundTrigger::Impact => impacts > 0,
        SoundTrigger::Click => clicked,
        SoundTrigger::Key => sound.key.is_some_and(|k| keys.just_pressed(k)),
    }
}

/// Fire every sound whose trigger happened this frame.
#[allow(clippy::too_many_arguments)]
pub fn play_scene_sounds(
    mut commands: Commands,
    mut sounds: ResMut<SceneSounds>,
    time: Res<Time>,
    features: Res<AudioFeatures>,
    particles: Res<FxParticles>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    editor: Res<EditorState>,
    new_balls: Query<(), Added<Ball>>,
) {
    if !sounds.enabled || sounds.sounds.is_empty() {
        return;
    }
    // Not `features.time`: an f32 seconds counter stops advancing once a frame
    // is under half its ulp (~36 h at 144 Hz), which would gate every sound
    // shut for good.
    let now = time.elapsed();
    let spawned = new_balls.iter().count();
    let clicked = mouse.just_pressed(MouseButton::Left) && !editor.capture_pointer;
    let keys = if editor.capture_keyboard {
        // Typing in the editor must not play the scene's key sounds.
        &ButtonInput::<KeyCode>::default()
    } else {
        &*keys
    };
    let impacts = particles.impacts;
    let sounds = &mut *sounds;
    let rng = sounds
        .rng
        .get_or_insert_with(|| fastrand::Rng::with_seed(0x736f_756e_6473));

    for sound in &mut sounds.sounds {
        if sound.def.trigger == SoundTrigger::Loop {
            if !sound.started {
                sound.started = true;
                commands.spawn((
                    AudioPlayer::new(sound.handle.clone()),
                    PlaybackSettings::LOOP.with_volume(Volume::Linear(sound.def.volume)),
                    SceneEntity,
                ));
            }
            continue;
        }
        let fire = fired(sound, &features, spawned, impacts, clicked, keys);
        sound.started = true;
        if !fire || !sound.rested(now) {
            continue;
        }
        sound.last = Some(now);
        let speed = 1.0 + (rng.f32() * 2.0 - 1.0) * sound.def.jitter;
        commands.spawn((
            AudioPlayer::new(sound.handle.clone()),
            PlaybackSettings::DESPAWN
                .with_volume(Volume::Linear(sound.def.volume))
                .with_speed(speed),
            SceneEntity,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(trigger: SoundTrigger) -> SceneSound {
        SceneSound::new(
            SoundDef {
                path: "x.ogg".into(),
                trigger,
                every: 4,
                key: Some("j".into()),
                ..SoundDef::default()
            },
            Handle::default(),
        )
    }

    #[test]
    fn beat_sounds_respect_every() {
        let s = sound(SoundTrigger::Beat);
        let keys = ButtonInput::<KeyCode>::default();
        let mut f = AudioFeatures::default();
        f.beat = true;
        f.beats = 3;
        assert!(!fired(&s, &f, 0, 0, false, &keys));
        f.beats = 8;
        assert!(fired(&s, &f, 0, 0, false, &keys));
        f.beat = false;
        assert!(!fired(&s, &f, 0, 0, false, &keys));
    }

    #[test]
    fn user_triggers_fire_on_their_event_only() {
        let keys = ButtonInput::<KeyCode>::default();
        let f = AudioFeatures::default();
        assert!(fired(&sound(SoundTrigger::Click), &f, 0, 0, true, &keys));
        assert!(!fired(&sound(SoundTrigger::Click), &f, 1, 1, false, &keys));
        assert!(fired(&sound(SoundTrigger::Spawn), &f, 2, 0, false, &keys));
        assert!(fired(&sound(SoundTrigger::Impact), &f, 0, 3, false, &keys));
        assert!(fired(&sound(SoundTrigger::Start), &f, 0, 0, false, &keys));
        let mut pressed = ButtonInput::<KeyCode>::default();
        pressed.press(KeyCode::KeyJ);
        assert!(fired(&sound(SoundTrigger::Key), &f, 0, 0, false, &pressed));
        assert!(!fired(&sound(SoundTrigger::Key), &f, 0, 0, false, &keys));
    }

    fn sanitized(volume: f32, jitter: f32, min_interval: f32) -> (f32, f32, f32) {
        let def = SceneSound::new(
            SoundDef {
                path: "x.ogg".into(),
                volume,
                jitter,
                min_interval,
                ..SoundDef::default()
            },
            Handle::default(),
        )
        .def;
        (def.volume, def.jitter, def.min_interval)
    }

    #[test]
    fn volume_jitter_and_interval_are_sanitized() {
        let d = SoundDef::default();
        assert_eq!(sanitized(0.7, 0.1, 0.2), (0.7, 0.1, 0.2));
        assert_eq!(sanitized(40.0, 5.0, -1.0), (MAX_VOLUME, MAX_JITTER, 0.0));
        assert_eq!(sanitized(-1.0, -0.5, 0.0), (0.0, 0.0, 0.0));
        assert_eq!(
            sanitized(f32::NAN, f32::NAN, f32::NAN),
            (d.volume, d.jitter, d.min_interval)
        );
        assert_eq!(
            sanitized(f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY),
            (d.volume, d.jitter, d.min_interval)
        );
    }

    /// An app running just [`play_scene_sounds`] over `sounds`, its clock at
    /// `start`.
    fn sound_app(sounds: Vec<SceneSound>, start: Duration) -> App {
        let mut time = Time::<()>::default();
        time.advance_to(start);
        let mut app = App::new();
        app.insert_resource(time)
            .insert_resource(SceneSounds {
                sounds,
                enabled: true,
                rng: None,
            })
            .init_resource::<AudioFeatures>()
            .init_resource::<FxParticles>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<EditorState>()
            .add_systems(Update, play_scene_sounds);
        app
    }

    /// Settings of every player the sounds spawned.
    fn played(app: &mut App) -> Vec<PlaybackSettings> {
        let world = app.world_mut();
        world
            .query_filtered::<&PlaybackSettings, With<AudioPlayer>>()
            .iter(world)
            .copied()
            .collect()
    }

    #[test]
    fn loops_get_the_same_volume_cap_as_one_shots() {
        let loud = |trigger| {
            SceneSound::new(
                SoundDef {
                    path: "x.ogg".into(),
                    trigger,
                    volume: 40.0,
                    ..SoundDef::default()
                },
                Handle::default(),
            )
        };
        let mut app = sound_app(
            vec![loud(SoundTrigger::Loop), loud(SoundTrigger::Start)],
            Duration::ZERO,
        );
        app.update();
        let played = played(&mut app);
        assert_eq!(played.len(), 2);
        for p in played {
            assert_eq!(p.volume, Volume::Linear(MAX_VOLUME));
        }
    }

    #[test]
    fn min_interval_follows_the_app_clock_after_days_of_uptime() {
        // At 2^17 s (~36 h) an f32 seconds counter can no longer take a 144 Hz
        // step, so `AudioFeatures::time` stands still there. Freeze it and
        // click every frame for a second: the gate must keep opening.
        let frame = Duration::from_secs_f64(1.0 / 144.0);
        let mut app = sound_app(
            vec![sound(SoundTrigger::Click)],
            Duration::from_secs(1 << 17),
        );
        app.world_mut().resource_mut::<AudioFeatures>().time = 131_072.0;
        for _ in 0..144 {
            app.world_mut().resource_mut::<Time>().advance_by(frame);
            let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
            mouse.reset_all();
            mouse.press(MouseButton::Left);
            app.update();
        }
        // min_interval 0.05 s at 144 Hz: every 8th frame (7 are only 48.6 ms).
        assert_eq!(played(&mut app).len(), 18);
    }
}
