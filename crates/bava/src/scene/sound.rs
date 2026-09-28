// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scene sound effects (`[sounds.<name>]`), fired by triggers.
//!
//! bava visualizes whatever the system is playing, so a scene's own sounds are
//! heard by the loopback capture too. The triggers are chosen with that in
//! mind: clicks, key presses, ball spawns and impacts are user-driven, and
//! `beat` sounds take an `every` divider so a scene can't drown the music or
//! chase its own echo. Offline renders (`--input`) never play scene sounds.

use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings, Volume};
use bevy::prelude::*;

use crate::gui::EditorState;
use crate::scene::SceneEntity;
use crate::scene::def::{SoundDef, SoundTrigger};
use crate::vis::features::AudioFeatures;
use crate::vis::fx::particles::FxParticles;
use crate::vis::physics::Ball;

/// One loaded scene sound and its trigger state.
pub struct SceneSound {
    pub def: SoundDef,
    pub handle: Handle<AudioSource>,
    pub key: Option<KeyCode>,
    last: f32,
    started: bool,
}

impl SceneSound {
    pub fn new(def: SoundDef, handle: Handle<AudioSource>) -> Self {
        let key = def.key.as_deref().and_then(crate::config::parse_key);
        Self {
            def,
            handle,
            key,
            last: f32::NEG_INFINITY,
            started: false,
        }
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
    let now = features.time;
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
        if !fire || now - sound.last < sound.def.min_interval {
            continue;
        }
        sound.last = now;
        let jitter = sound.def.jitter.clamp(0.0, 0.9);
        let speed = 1.0 + (rng.f32() * 2.0 - 1.0) * jitter;
        commands.spawn((
            AudioPlayer::new(sound.handle.clone()),
            PlaybackSettings::DESPAWN
                .with_volume(Volume::Linear(sound.def.volume.clamp(0.0, 2.0)))
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
}
