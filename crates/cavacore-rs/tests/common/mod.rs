// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared deterministic test-signal helpers.
#![allow(dead_code)] // each test binary uses a different subset

use std::f64::consts::TAU;

/// Generate `frames` of an interleaved sine tone.
///
/// `amplitudes` has one entry per channel; a channel with amplitude `0.0` is
/// silent. The output is `frames * channels` samples, interleaved L,R,L,R,...
pub fn sine_interleaved(freq: f64, rate: u32, frames: usize, amplitudes: &[f64]) -> Vec<f64> {
    let channels = amplitudes.len();
    let mut out = Vec::with_capacity(frames * channels);
    for n in 0..frames {
        let t = n as f64 / rate as f64;
        let s = (TAU * freq * t).sin();
        for &amp in amplitudes {
            out.push(s * amp);
        }
    }
    out
}

/// Index of the maximum value in a slice.
pub fn argmax(xs: &[f64]) -> usize {
    let mut best = 0;
    let mut best_v = f64::NEG_INFINITY;
    for (i, &v) in xs.iter().enumerate() {
        if v > best_v {
            best_v = v;
            best = i;
        }
    }
    best
}
