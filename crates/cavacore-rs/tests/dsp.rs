//! Functional / end-to-end tests: verify cavacore actually analyzes audio the
//! way a visualizer needs — silence is quiet, tones land in the right frequency
//! band, stereo channels stay separated, and a realistic streamed signal stays
//! well-behaved frame to frame.

mod common;

use cavacore_rs::CavaConfig;
use common::{argmax, sine_interleaved};

/// Run `frames_per_call`-sized chunks of `signal` through the plan `iterations`
/// times (looping the signal) and return the final bar frame, after the
/// auto-sensitivity filter has had time to settle.
fn settle(plan: &mut cavacore_rs::CavaPlan, signal: &[f64], iterations: usize) -> Vec<f64> {
    let mut last = vec![0.0; plan.output_len()];
    for _ in 0..iterations {
        last = plan.execute(signal).to_vec();
    }
    last
}

#[test]
fn silence_is_near_zero() {
    let mut plan = CavaConfig { bars: 16, channels: 1, ..Default::default() }
        .build()
        .unwrap();
    let silence = vec![0.0f64; 512];
    let out = settle(&mut plan, &silence, 300);
    let max = out.iter().cloned().fold(0.0f64, f64::max);
    assert!(max < 1e-3, "silence should yield ~0 bars, got max {max}");
}

#[test]
fn low_tone_peaks_lower_than_high_tone() {
    let rate = 44_100;
    // 16 bars across 50..10000 Hz, mono.
    let cfg = CavaConfig { bars: 16, channels: 1, rate, ..Default::default() };

    let mut low_plan = cfg.build().unwrap();
    let low = sine_interleaved(150.0, rate, 512, &[0.8]);
    let low_out = settle(&mut low_plan, &low, 400);

    let mut high_plan = cfg.build().unwrap();
    let high = sine_interleaved(7000.0, rate, 512, &[0.8]);
    let high_out = settle(&mut high_plan, &high, 400);

    let low_peak = argmax(&low_out);
    let high_peak = argmax(&high_out);

    // Each tone must produce a clear peak...
    assert!(low_out[low_peak] > 0.1, "low tone produced no clear peak: {low_out:?}");
    assert!(high_out[high_peak] > 0.1, "high tone produced no clear peak: {high_out:?}");
    // ...and the low tone's peak bar must be below the high tone's.
    assert!(
        low_peak < high_peak,
        "expected low tone peak ({low_peak}) below high tone peak ({high_peak})\nlow={low_out:?}\nhigh={high_out:?}"
    );
}

#[test]
fn stereo_channels_are_separated() {
    let rate = 44_100;
    let bars = 16;
    let mut plan = CavaConfig { bars, channels: 2, rate, ..Default::default() }
        .build()
        .unwrap();

    // Tone only in the LEFT channel; right channel silent.
    let signal = sine_interleaved(1000.0, rate, 512, &[0.8, 0.0]);
    let out = settle(&mut plan, &signal, 400);

    let (left, right) = out.split_at(bars as usize);
    let left_energy: f64 = left.iter().sum();
    let right_energy: f64 = right.iter().sum();

    assert!(left_energy > 0.1, "left channel should have energy, got {left_energy}");
    assert!(
        right_energy < left_energy * 0.1,
        "right channel should be near-silent: left={left_energy} right={right_energy}"
    );
}

#[test]
fn noise_reduction_slows_decay() {
    // The defining behavior of cavacore's smoothing/gravity filter: a higher
    // noise_reduction makes a bar fall more slowly once its input stops. We
    // settle on a tone, then cut to silence and measure the fraction of the
    // bar's peak that remains a few frames later. Normalizing by each config's
    // own settled peak removes any auto-sensitivity scale difference, isolating
    // the decay rate.
    let rate = 44_100;
    let tone = sine_interleaved(440.0, rate, 256, &[0.8]);
    let silence = vec![0.0f64; 256];

    // Number of silent frames to integrate the decay over. Long enough that the
    // gravity/integral filter difference dominates the (nr-independent) ~8-frame
    // flush of cavacore's internal FFT buffer.
    const SILENT_FRAMES: usize = 60;

    let decay_integral = |nr: f64| {
        // autosens off: expose the raw integral/gravity filter so the dynamic
        // gain loop doesn't confound the decay measurement.
        let mut plan = CavaConfig {
            bars: 12,
            channels: 1,
            rate,
            autosens: false,
            noise_reduction: nr,
            ..Default::default()
        }
        .build()
        .unwrap();
        for _ in 0..150 {
            plan.execute(&tone);
        }
        // Reference level: average the peak bar over the last few tone frames,
        // which averages out per-frame windowing ripple.
        let peak_bar = argmax(plan.last_output());
        let mut reference = 0.0;
        for _ in 0..10 {
            reference += plan.execute(&tone)[peak_bar];
        }
        reference /= 10.0;
        assert!(reference > 1e-6, "tone failed to settle for nr={nr}: ref={reference}");

        // Integrate the bar value as it decays through a long silence window.
        let mut area = 0.0;
        for _ in 0..SILENT_FRAMES {
            area += plan.execute(&silence)[peak_bar];
        }
        // Normalize so the comparison is about decay *shape*, not absolute scale.
        area / (reference * SILENT_FRAMES as f64)
    };

    let smooth = decay_integral(0.9);
    let rough = decay_integral(0.1);
    assert!(
        smooth > rough,
        "expected higher noise_reduction to decay slower (larger integral): smooth={smooth} rough={rough}"
    );
}

/// End-to-end: stream ~2 seconds of a stereo musical-ish signal (a low bass note
/// plus a high lead) through the plan in realistic 512-sample frames and assert
/// the visualizer output is, throughout, finite, bounded and non-negative, and
/// that it is actually reactive (not stuck flat), with energy present in both a
/// low and a high band.
#[test]
fn e2e_streamed_signal_is_well_behaved_and_reactive() {
    let rate = 44_100;
    let bars = 24usize;
    let mut plan = CavaConfig { bars: bars as u32, channels: 2, rate, ..Default::default() }
        .build()
        .unwrap();

    let frame = 512usize;
    let total_frames = rate as usize * 2; // ~2 seconds
    let n_calls = total_frames / frame;

    // Build the whole signal once: 80 Hz bass + 6000 Hz lead, both channels.
    let bass = sine_interleaved(80.0, rate, total_frames, &[0.5, 0.5]);
    let lead = sine_interleaved(6000.0, rate, total_frames, &[0.4, 0.4]);
    let signal: Vec<f64> = bass.iter().zip(&lead).map(|(a, b)| a + b).collect();

    let mut frames_out: Vec<Vec<f64>> = Vec::with_capacity(n_calls);
    for i in 0..n_calls {
        let start = i * frame * 2; // *2 for interleaved stereo
        let chunk = &signal[start..start + frame * 2];
        let out = plan.execute(chunk);

        assert_eq!(out.len(), bars * 2);
        for &v in out {
            assert!(v.is_finite(), "frame {i}: non-finite {v}");
            assert!(v >= 0.0, "frame {i}: negative {v}");
            assert!(v <= 2.0, "frame {i}: unbounded {v}");
        }
        frames_out.push(out.to_vec());
    }

    // Reactivity: the output must change over time (not a frozen buffer).
    let first = &frames_out[n_calls / 4];
    let last = &frames_out[n_calls - 1];
    let delta: f64 = first.iter().zip(last).map(|(a, b)| (a - b).abs()).sum();
    assert!(delta > 0.0, "output never changed across the stream");

    // After settling, both a low band and a high band should carry energy,
    // since the signal contains an 80 Hz and a 6000 Hz component.
    let final_left = &last[..bars];
    let low_band: f64 = final_left[..bars / 3].iter().sum();
    let high_band: f64 = final_left[2 * bars / 3..].iter().sum();
    assert!(low_band > 0.05, "expected low-band energy, got {low_band}");
    assert!(high_band > 0.05, "expected high-band energy, got {high_band}");
}
