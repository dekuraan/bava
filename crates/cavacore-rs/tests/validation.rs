// SPDX-License-Identifier: GPL-3.0-or-later
//! Construction, validation, buffer-sizing, lifecycle and thread-safety tests.
//!
//! These exercise the *safety* contract of the wrapper: every invalid argument
//! must be rejected with a typed error (never UB), output buffers must be sized
//! exactly, plans must free cleanly, and the documented `Send` / concurrent-build
//! guarantees must hold.

mod common;

use cavacore_rs::{CavaConfig, CavaError, CavaPlan};

#[test]
fn default_config_builds() {
    let plan = CavaConfig::default().build().expect("default should build");
    assert_eq!(plan.bars(), 32);
    assert_eq!(plan.channels(), 2);
    assert_eq!(plan.rate(), 44_100);
    assert_eq!(plan.output_len(), 64);
    assert!(plan.max_input_samples() > 0);
}

#[test]
fn rejects_bad_channel_count() {
    for ch in [0, 3, 8] {
        let err = CavaConfig {
            channels: ch,
            ..Default::default()
        }
        .build()
        .unwrap_err();
        assert_eq!(err, CavaError::InvalidChannels(ch));
    }
}

#[test]
fn rejects_zero_bars() {
    let err = CavaConfig {
        bars: 0,
        ..Default::default()
    }
    .build()
    .unwrap_err();
    assert_eq!(err, CavaError::InvalidBars(0));
}

#[test]
fn rejects_zero_rate() {
    let err = CavaConfig {
        rate: 0,
        ..Default::default()
    }
    .build()
    .unwrap_err();
    assert_eq!(err, CavaError::InvalidRate(0));
}

#[test]
fn rejects_out_of_range_noise_reduction() {
    for nr in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        let err = CavaConfig {
            noise_reduction: nr,
            ..Default::default()
        }
        .build()
        .unwrap_err();
        assert!(matches!(err, CavaError::InvalidNoiseReduction(_)));
    }
}

#[test]
fn rejects_bad_cutoff_band() {
    // low == 0
    assert!(matches!(
        CavaConfig { low_cutoff_freq: 0, ..Default::default() }.build(),
        Err(CavaError::InvalidCutoff { .. })
    ));
    // high <= low
    assert!(matches!(
        CavaConfig { low_cutoff_freq: 5000, high_cutoff_freq: 5000, ..Default::default() }.build(),
        Err(CavaError::InvalidCutoff { .. })
    ));
    // high >= nyquist
    assert!(matches!(
        CavaConfig { rate: 44_100, high_cutoff_freq: 30_000, ..Default::default() }.build(),
        Err(CavaError::InvalidCutoff { .. })
    ));
}

#[test]
fn output_len_matches_mono_and_stereo() {
    let mono = CavaConfig { bars: 24, channels: 1, ..Default::default() }
        .build()
        .unwrap();
    assert_eq!(mono.output_len(), 24);

    let stereo = CavaConfig { bars: 24, channels: 2, ..Default::default() }
        .build()
        .unwrap();
    assert_eq!(stereo.output_len(), 48);
}

#[test]
fn execute_returns_exact_output_len_for_any_input_size() {
    let mut plan = CavaConfig { bars: 16, channels: 2, ..Default::default() }
        .build()
        .unwrap();
    let want = plan.output_len();

    // empty, tiny, normal, and larger-than-internal-buffer inputs must all be
    // accepted and return exactly `output_len` finite values.
    let huge = plan.max_input_samples() * 4;
    for len in [0usize, 1, 2, 512, 4096, huge] {
        let input = vec![0.0f64; len];
        let out = plan.execute(&input);
        assert_eq!(out.len(), want, "input len {len} produced wrong output len");
        assert!(out.iter().all(|v| v.is_finite()), "non-finite output for len {len}");
    }
}

#[test]
fn output_is_bounded_and_nonnegative_with_autosens() {
    let mut plan = CavaConfig { bars: 16, channels: 1, autosens: true, ..Default::default() }
        .build()
        .unwrap();
    // Drive a loud signal through many frames, then check the contract that
    // autosens keeps output in a sane bounded, non-negative range.
    let signal = common::sine_interleaved(440.0, 44_100, 512, &[0.9]);
    let mut last = vec![];
    for _ in 0..200 {
        last = plan.execute(&signal).to_vec();
    }
    for &v in &last {
        assert!(v.is_finite(), "non-finite bar: {v}");
        assert!(v >= 0.0, "negative bar: {v}");
        assert!(v <= 1.5, "bar exceeded expected bound: {v}");
    }
}

#[test]
fn build_drop_loop_does_not_crash() {
    // Exercises cava_init/cava_destroy (and the FFTW planner lock) repeatedly to
    // catch double-frees or teardown UB.
    for _ in 0..200 {
        let plan = CavaConfig::default().build().unwrap();
        drop(plan);
    }
}

#[test]
fn plan_is_send_to_another_thread() {
    let mut plan: CavaPlan = CavaConfig { bars: 12, channels: 1, ..Default::default() }
        .build()
        .unwrap();
    let handle = std::thread::spawn(move || {
        let input = common::sine_interleaved(1000.0, 44_100, 512, &[0.5]);
        let out = plan.execute(&input);
        assert_eq!(out.len(), 12);
        out.iter().all(|v| v.is_finite())
    });
    assert!(handle.join().unwrap());
}

#[test]
fn concurrent_builds_are_safe() {
    // Without serializing FFTW planning this races and can corrupt/crash.
    // With the wrapper's planner lock, all builds must succeed.
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                for _ in 0..25 {
                    let plan = CavaConfig {
                        bars: 8 + i,
                        ..Default::default()
                    }
                    .build()
                    .unwrap();
                    assert_eq!(plan.bars(), 8 + i as usize);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("builder thread panicked");
    }
}

#[test]
fn misaligned_stereo_input_never_poisons_the_plan() {
    // Feeding a partial stereo frame (odd sample count) used to make cavacore
    // emit NaN, which then propagates through the integral filter and ruins all
    // subsequent frames. The wrapper must floor to whole frames so this can't
    // happen — even interleaving deliberately odd-length chunks.
    let mut plan = CavaConfig { bars: 16, channels: 2, ..Default::default() }
        .build()
        .unwrap();
    let tone = common::sine_interleaved(440.0, 44_100, 256, &[0.6, 0.6]);

    for &odd in &[1usize, 3, 5, 511, 513] {
        let out = plan.execute(&vec![0.5f64; odd]);
        assert!(
            out.iter().all(|v| v.is_finite()),
            "odd input len {odd} produced non-finite output: {out:?}"
        );
    }
    // And a normal frame afterwards must still be finite (state not poisoned).
    for _ in 0..50 {
        let out = plan.execute(&tone);
        assert!(out.iter().all(|v| v.is_finite()), "plan state was poisoned");
    }
}

#[test]
fn varying_chunk_sizes_are_accepted() {
    // Async capture delivers irregular chunk sizes; the API must tolerate it.
    let mut plan = CavaConfig { bars: 16, channels: 2, ..Default::default() }
        .build()
        .unwrap();
    for &chunk in &[64usize, 128, 300, 512, 1024, 17] {
        let input = vec![0.01f64; chunk * 2];
        let out = plan.execute(&input);
        assert_eq!(out.len(), 32);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
