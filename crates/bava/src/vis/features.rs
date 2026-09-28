// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-frame audio features derived from the [`Cava`] bars: band levels, a beat
//! detector and a couple of clocks for animating shaders.
//!
//! Everything that *reacts* to the music beyond drawing the spectrum itself —
//! the blob/halo shaders, beat shockwaves, camera punch, scene objects bound to
//! `bass` or `beat` — reads the one [`AudioFeatures`] resource, so a beat is
//! detected once per frame and every effect agrees on it.
//!
//! The beat detector reads [`AudioLevels`] — the low band metered on the raw
//! samples — not the bars: cavacore's bars are smoothed for looks and sit
//! pinned near 1.0 between kicks on a loud mix. It works in decibels against a
//! *valley follower* — a floor that drops instantly to each trough and creeps
//! back up slowly — and fires when the low band climbs a fixed margin out of
//! that floor, with hysteresis and a refractory window so one kick can't fire
//! twice. A running *average* would sit halfway up every kick on a dense mix
//! (a sustained 808 under the kick leaves only a few dB of swing); the floor
//! measures the kick from where it actually starts. Being relative and
//! logarithmic, the same margin holds for a quiet acoustic track and a
//! brick-walled club mix.

use bevy::prelude::*;

use crate::cava::{AudioLevels, Cava, CavaAnalysisSet};

/// Fraction of the (low → high) bars counted as bass.
const BASS_END: f32 = 0.14;
/// Fraction of the bars where the mids end and the treble begins.
const MID_END: f32 = 0.5;
/// Release time constant of the displayed band levels, in seconds. Attack is
/// instant so transients read as transients.
const BAND_RELEASE: f32 = 0.12;
/// How fast the low-band floor creeps back up after a trough, seconds.
const FLOOR_TAU: f32 = 0.35;
/// The low band must climb this many dB out of its floor to fire…
const TRIGGER_DB: f32 = 3.0;
/// …and settle back within this margin of it before it can fire again.
const REARM_DB: f32 = 1.0;
/// Below this low-band level (dB re. full scale) there is nothing to detect.
const SILENCE_DB: f32 = -60.0;
/// Minimum seconds between beats (≈ 400 BPM), so one kick can't double-fire.
const REFRACTORY: f32 = 0.15;
/// Decay time constant of [`AudioFeatures::beat_pulse`], in seconds.
const PULSE_TAU: f32 = 0.16;

/// Band levels, beat state and animation clocks for the current frame.
#[derive(Resource, Clone, Debug, Default)]
pub struct AudioFeatures {
    /// Low-band level (kick, bass), roughly `0..1`.
    pub bass: f32,
    /// Mid-band level (vocals, snare body), roughly `0..1`.
    pub mid: f32,
    /// High-band level (hats, air), roughly `0..1`.
    pub treble: f32,
    /// Mean level across every bar, roughly `0..1`.
    pub energy: f32,
    /// True on exactly the frames a beat onset was detected.
    pub beat: bool,
    /// `1.0` on a beat, decaying exponentially toward `0.0` — the envelope most
    /// effects should use rather than the one-frame [`beat`](Self::beat) flag.
    pub beat_pulse: f32,
    /// Beats detected since startup.
    pub beats: u64,
    /// Seconds since startup.
    pub time: f32,
    /// A clock that runs faster when the music is louder: shaders animated by
    /// it drift slowly in quiet passages and churn through loud ones.
    pub flow: f32,
    detector: BeatDetector,
}

impl AudioFeatures {
    /// Advance one frame over `dt` seconds from the current bars (`bars`
    /// ordered low → high, averaged across channels) and the raw signal levels.
    pub fn update(&mut self, bars: &[f32], levels: AudioLevels, dt: f32) {
        let dt = dt.max(0.0);
        let (bass, mid, treble, energy) = band_levels(bars);
        let release = if dt > 0.0 {
            (-dt / BAND_RELEASE).exp()
        } else {
            1.0
        };
        let follow = |shown: f32, now: f32| {
            if now >= shown {
                now
            } else {
                now + (shown - now) * release
            }
        };
        self.bass = follow(self.bass, bass);
        self.mid = follow(self.mid, mid);
        self.treble = follow(self.treble, treble);
        self.energy = follow(self.energy, energy);

        self.beat = self.detector.step(levels, dt);
        if self.beat {
            self.beats += 1;
            self.beat_pulse = 1.0;
        } else if dt > 0.0 {
            self.beat_pulse *= (-dt / PULSE_TAU).exp();
        }
        self.time += dt;
        self.flow += dt * (0.35 + 1.6 * self.energy.min(1.5));
    }

    /// Resolve a named band — `bass`, `mid`, `treble`, `energy`, `beat` (the
    /// decaying pulse), or `bar:N` for a single bar — to its current level.
    /// Unknown names read as silence. Used by scene audio bindings.
    pub fn band(&self, name: &str, bars: &[f32]) -> f32 {
        match name {
            "bass" => self.bass,
            "mid" => self.mid,
            "treble" => self.treble,
            "energy" => self.energy,
            "beat" | "pulse" => self.beat_pulse,
            other => other
                .strip_prefix("bar:")
                .and_then(|i| i.trim().parse::<usize>().ok())
                .and_then(|i| bars.get(i).copied())
                .unwrap_or(0.0),
        }
    }
}

/// Mean level of each band, plus the overall mean. Bars are low → high.
pub(crate) fn band_levels(bars: &[f32]) -> (f32, f32, f32, f32) {
    let n = bars.len();
    if n == 0 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let bass_end = ((n as f32 * BASS_END).ceil() as usize).clamp(1, n);
    let mid_end = ((n as f32 * MID_END).ceil() as usize).clamp(bass_end, n);
    let mean = |s: &[f32]| {
        if s.is_empty() {
            0.0
        } else {
            s.iter().map(|v| v.max(0.0)).sum::<f32>() / s.len() as f32
        }
    };
    (
        mean(&bars[..bass_end]),
        mean(&bars[bass_end..mid_end]),
        mean(&bars[mid_end..]),
        mean(bars),
    )
}

/// Rising-edge low-band onset detector (in dB) with hysteresis and a
/// refractory window.
#[derive(Clone, Debug)]
struct BeatDetector {
    /// Valley-following floor of the low-band level, dB.
    floor: f32,
    /// Ready to fire (the level has dropped back under the re-arm margin).
    armed: bool,
    /// Seconds since the last beat.
    since: f32,
    /// Whether `floor` has been seeded yet.
    primed: bool,
}

impl Default for BeatDetector {
    fn default() -> Self {
        Self {
            floor: SILENCE_DB,
            armed: true,
            since: REFRACTORY,
            primed: false,
        }
    }
}

impl BeatDetector {
    /// Feed one frame; true when a beat onset fires this frame. Frames that
    /// brought no new audio only advance the clock.
    fn step(&mut self, levels: AudioLevels, dt: f32) -> bool {
        self.since += dt;
        if !levels.fresh {
            return false;
        }
        let db = 10.0 * levels.low.max(1e-12).log10();
        if !self.primed {
            self.primed = true;
            self.floor = db;
            return false;
        }
        let rise = db - self.floor;
        let fired = self.armed && self.since >= REFRACTORY && db > SILENCE_DB && rise > TRIGGER_DB;
        if fired {
            self.armed = false;
            self.since = 0.0;
        } else if !self.armed && rise < REARM_DB {
            self.armed = true;
        }
        // Move the floor *after* the comparison so the onset frame is judged
        // against the level that preceded it: down to any new trough at once,
        // up toward the current level slowly.
        if db < self.floor {
            self.floor = db;
        } else if dt > 0.0 {
            let k = 1.0 - (-dt / FLOOR_TAU).exp();
            self.floor += (db - self.floor) * k;
        }
        fired
    }
}

/// Installs [`AudioFeatures`] and keeps it current.
pub struct FeaturesPlugin;

impl Plugin for FeaturesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioFeatures>().add_systems(
            Update,
            update_audio_features
                .in_set(FeaturesSet)
                .after(CavaAnalysisSet),
        );
    }
}

/// [`update_audio_features`]; every effect reading [`AudioFeatures`] orders
/// itself after this so offline renders stay deterministic.
#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeaturesSet;

fn update_audio_features(
    time: Res<Time>,
    cava: Res<Cava>,
    levels: Res<AudioLevels>,
    mut features: ResMut<AudioFeatures>,
) {
    features.update(&cava.mono(), *levels, time.delta_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    /// Low-band levels for a kick drum on a fixed tempo over a sustained bass:
    /// the kick's energy jumps on each beat and decays, like a real mix.
    fn kick_track(bpm: f32, seconds: f32, bed: f32, kick: f32) -> Vec<AudioLevels> {
        let period = 60.0 / bpm;
        let frames = (seconds / DT) as usize;
        (0..frames)
            .map(|f| {
                let phase = (f as f32 * DT) % period;
                let amp = bed + kick * (-phase / 0.12).exp();
                AudioLevels {
                    low: amp * amp * 0.5,
                    full: amp * amp,
                    fresh: true,
                }
            })
            .collect()
    }

    fn level(amp: f32) -> AudioLevels {
        AudioLevels {
            low: amp * amp * 0.5,
            full: amp * amp,
            fresh: true,
        }
    }

    fn run(features: &mut AudioFeatures, track: &[AudioLevels]) -> u64 {
        let start = features.beats;
        for &l in track {
            features.update(&[0.8, 0.8, 0.3, 0.3, 0.3, 0.3, 0.2, 0.2], l, DT);
        }
        features.beats - start
    }

    #[test]
    fn band_levels_split_low_to_high() {
        let bars = [1.0, 1.0, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0];
        let (bass, mid, treble, energy) = band_levels(&bars);
        assert!((bass - 1.0).abs() < 1e-6, "bass {bass}");
        assert!(mid > 0.4 && mid <= 0.5 + 1e-6, "mid {mid}");
        assert_eq!(treble, 0.0);
        assert!((energy - 0.35).abs() < 1e-6);
        assert_eq!(band_levels(&[]), (0.0, 0.0, 0.0, 0.0));
        // A single bar is all bass, and must not index out of range.
        let (b, m, t, _) = band_levels(&[0.7]);
        assert_eq!((b, m, t), (0.7, 0.0, 0.0));
    }

    #[test]
    fn detects_every_kick_at_120_bpm_over_a_loud_bass() {
        let mut f = AudioFeatures::default();
        run(&mut f, &kick_track(120.0, 2.0, 0.3, 0.9));
        let beats = run(&mut f, &kick_track(120.0, 8.0, 0.3, 0.9));
        assert!(
            (15..=17).contains(&beats),
            "expected ~16 beats, got {beats}"
        );
    }

    #[test]
    fn detection_is_loudness_independent() {
        // The same groove 30 dB quieter still beats the same.
        let mut quiet = AudioFeatures::default();
        run(&mut quiet, &kick_track(128.0, 2.0, 0.01, 0.03));
        let beats = run(&mut quiet, &kick_track(128.0, 6.0, 0.01, 0.03));
        let expected = 128.0 / 60.0 * 6.0;
        assert!(
            (beats as f32 - expected).abs() <= 1.5,
            "expected ~{expected}, got {beats}"
        );
    }

    #[test]
    fn fast_tempo_is_not_halved_or_doubled() {
        let mut f = AudioFeatures::default();
        run(&mut f, &kick_track(174.0, 2.0, 0.2, 0.8));
        let beats = run(&mut f, &kick_track(174.0, 6.0, 0.2, 0.8));
        let expected = 174.0 / 60.0 * 6.0;
        assert!(
            (beats as f32 - expected).abs() <= 2.0,
            "expected ~{expected}, got {beats}"
        );
    }

    #[test]
    fn steady_and_silent_signals_do_not_beat() {
        let mut f = AudioFeatures::default();
        assert_eq!(run(&mut f, &vec![level(0.0); 600]), 0, "silence");
        let mut f = AudioFeatures::default();
        assert_eq!(run(&mut f, &vec![level(0.6); 600]), 0, "steady tone");
    }

    #[test]
    fn stale_frames_only_advance_the_clock() {
        let mut f = AudioFeatures::default();
        run(&mut f, &vec![level(0.1); 60]);
        let stale = AudioLevels {
            fresh: false,
            ..level(1.0)
        };
        // A loud value that isn't fresh must not fire.
        assert_eq!(run(&mut f, &[stale; 10]), 0);
        assert_eq!(run(&mut f, &[level(1.0)]), 1, "the fresh loud frame does");
    }

    #[test]
    fn pulse_is_one_on_beat_and_decays() {
        let mut f = AudioFeatures::default();
        run(&mut f, &vec![level(0.05); 120]);
        f.update(&[1.0, 1.0, 0.0, 0.0], level(1.0), DT);
        assert!(f.beat);
        assert_eq!(f.beat_pulse, 1.0);
        let mut prev = f.beat_pulse;
        for _ in 0..30 {
            f.update(&[1.0, 1.0, 0.0, 0.0], level(1.0), DT);
            assert!(!f.beat, "held note must not retrigger");
            assert!(f.beat_pulse < prev);
            prev = f.beat_pulse;
        }
        assert!(prev < 0.1);
    }

    #[test]
    fn zero_dt_is_harmless() {
        // Bevy's first frame reports dt == 0; nothing may divide by it.
        let mut f = AudioFeatures::default();
        f.update(&[0.5; 8], level(0.5), 0.0);
        assert!(f.bass.is_finite() && f.beat_pulse.is_finite() && f.flow.is_finite());
    }

    #[test]
    fn flow_runs_faster_when_loud() {
        let mut quiet = AudioFeatures::default();
        let mut loud = AudioFeatures::default();
        for _ in 0..60 {
            quiet.update(&[0.05; 8], level(0.05), DT);
            loud.update(&[0.9; 8], level(0.9), DT);
        }
        assert!(loud.flow > quiet.flow * 2.0);
        assert!((quiet.time - 1.0).abs() < 1e-3);
    }

    #[test]
    fn named_bands_resolve() {
        let mut f = AudioFeatures::default();
        let bars = [0.8, 0.8, 0.4, 0.4, 0.2, 0.2, 0.1, 0.1];
        f.update(&bars, level(0.3), DT);
        assert_eq!(f.band("bass", &bars), f.bass);
        assert_eq!(f.band("treble", &bars), f.treble);
        assert_eq!(f.band("bar:2", &bars), 0.4);
        assert_eq!(f.band("bar:99", &bars), 0.0);
        assert_eq!(f.band("nonsense", &bars), 0.0);
    }
}
