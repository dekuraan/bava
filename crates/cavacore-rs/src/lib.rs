// SPDX-License-Identifier: GPL-3.0-or-later
//! A pure-Rust port of [cavacore], the core audio-visualization engine of CAVA.
//!
//! [`CavaPlan`] turns interleaved PCM audio into a small set of smoothed,
//! logarithmically-spaced frequency bars — the signal that drives a CAVA-style
//! visualization. This crate has **no C dependency**: it reimplements
//! `cava_init`/`cava_execute` in safe Rust and uses [`realfft`] (a pure-Rust,
//! unnormalized real→complex FFT) in place of FFTW. `realfft`'s transform has
//! the same scale as FFTW's `fftw_plan_dft_r2c_1d`, so cavacore's hard-coded eq
//! constants carry over unchanged and the output matches the C original.
//!
//! ```no_run
//! use cavacore_rs::CavaConfig;
//!
//! let mut plan = CavaConfig::default().build().unwrap();
//! let samples = vec![0.0f64; 1024]; // interleaved L,R,L,R, ...
//! let bars = plan.execute(&samples);
//! assert_eq!(bars.len(), plan.output_len());
//! ```
//!
//! # Thread safety
//!
//! A [`CavaPlan`] owns its state exclusively. It is [`Send`] (and [`Sync`], since
//! `realfft` plans are), so it can be moved onto a dedicated audio thread.
//! [`CavaPlan::execute`] mutates internal buffers and takes `&mut self`.
//!
//! [cavacore]: https://github.com/karlstav/cava/blob/master/CAVACORE.md

use std::fmt;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Configuration for a [`CavaPlan`]. Use [`CavaConfig::default`] for sane
/// defaults and override fields as needed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CavaConfig {
    /// Number of output bars **per channel**. Must be >= 1.
    pub bars: u32,
    /// Input sample rate in Hz (e.g. 44100).
    pub rate: u32,
    /// Number of interleaved input channels. Must be 1 or 2.
    pub channels: u32,
    /// Auto-scale output into the `0.0..=1.0` range when `true`; otherwise emit
    /// raw magnitudes whose scale depends on the input level.
    pub autosens: bool,
    /// Smoothing factor in `0.0..=1.0`. Higher is smoother/slower; cavacore
    /// recommends `0.77`.
    pub noise_reduction: f64,
    /// Low edge of the visualized frequency band, in Hz.
    pub low_cutoff_freq: u32,
    /// High edge of the visualized frequency band, in Hz. Must exceed
    /// `low_cutoff_freq` and stay below the Nyquist frequency (`rate / 2`).
    pub high_cutoff_freq: u32,
}

impl Default for CavaConfig {
    fn default() -> Self {
        Self {
            bars: 32,
            rate: 44_100,
            channels: 2,
            autosens: true,
            noise_reduction: 0.77,
            low_cutoff_freq: 50,
            high_cutoff_freq: 10_000,
        }
    }
}

/// An error returned while constructing a [`CavaPlan`].
#[derive(Debug, Clone, PartialEq)]
pub enum CavaError {
    /// `channels` was not 1 or 2.
    InvalidChannels(u32),
    /// `bars` was 0.
    InvalidBars(u32),
    /// `rate` was 0.
    InvalidRate(u32),
    /// `noise_reduction` was outside `0.0..=1.0` (or not finite).
    InvalidNoiseReduction(f64),
    /// Cut-off band was empty, inverted, or beyond the Nyquist frequency.
    InvalidCutoff { low: u32, high: u32, rate: u32 },
    /// cavacore itself rejected the parameters; carries its diagnostic string.
    Init(String),
    /// `cava_init` returned a null pointer (allocation failure).
    NullPlan,
}

impl fmt::Display for CavaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CavaError::InvalidChannels(c) => {
                write!(f, "channels must be 1 or 2, got {c}")
            }
            CavaError::InvalidBars(b) => write!(f, "bars must be >= 1, got {b}"),
            CavaError::InvalidRate(r) => write!(f, "rate must be > 0, got {r}"),
            CavaError::InvalidNoiseReduction(n) => {
                write!(f, "noise_reduction must be in 0.0..=1.0, got {n}")
            }
            CavaError::InvalidCutoff { low, high, rate } => write!(
                f,
                "invalid cut-off band: low={low} high={high} (must satisfy 0 < low < high < rate/2 = {})",
                rate / 2
            ),
            CavaError::Init(msg) => write!(f, "cavacore rejected parameters: {msg}"),
            CavaError::NullPlan => write!(f, "cava_init returned null (out of memory)"),
        }
    }
}

impl std::error::Error for CavaError {}

/// Compute cavacore's FFT window size for a given sample rate. Mirrors the
/// hard-coded ladder in upstream `cava_init`: 512 at ≤8.125 kHz, doubling per
/// band up to 64× above 300 kHz.
fn fft_buffer_size(rate: u32) -> usize {
    let mut size = 512usize;
    if rate > 8125 && rate <= 16250 {
        size *= 2;
    } else if rate > 16250 && rate <= 32500 {
        size *= 4;
    } else if rate > 32500 && rate <= 75000 {
        size *= 8;
    } else if rate > 75000 && rate <= 150000 {
        size *= 16;
    } else if rate > 150000 && rate <= 300000 {
        size *= 32;
    } else if rate > 300000 {
        size *= 64;
    }
    size
}

impl CavaConfig {
    /// Validate the configuration and construct a [`CavaPlan`].
    ///
    /// Parameters are range-checked in Rust first (so the errors are
    /// descriptive); any remaining complaint cavacore's original init logic
    /// would raise is surfaced as [`CavaError::Init`].
    pub fn build(&self) -> Result<CavaPlan, CavaError> {
        if self.channels < 1 || self.channels > 2 {
            return Err(CavaError::InvalidChannels(self.channels));
        }
        if self.bars == 0 {
            return Err(CavaError::InvalidBars(self.bars));
        }
        if self.rate == 0 {
            return Err(CavaError::InvalidRate(self.rate));
        }
        if !self.noise_reduction.is_finite()
            || self.noise_reduction < 0.0
            || self.noise_reduction > 1.0
        {
            return Err(CavaError::InvalidNoiseReduction(self.noise_reduction));
        }
        if self.low_cutoff_freq == 0
            || self.high_cutoff_freq <= self.low_cutoff_freq
            || self.high_cutoff_freq >= self.rate / 2
        {
            return Err(CavaError::InvalidCutoff {
                low: self.low_cutoff_freq,
                high: self.high_cutoff_freq,
                rate: self.rate,
            });
        }

        CavaPlan::new(self).map_err(CavaError::Init)
    }
}

/// A live cavacore visualization plan.
///
/// Feed it interleaved PCM with [`CavaPlan::execute`] and read back
/// `bars * channels` smoothed magnitudes. The plan owns its allocation and
/// buffers.
pub struct CavaPlan {
    // --- config / geometry ---
    bars: usize,
    channels: usize,
    rate: u32,
    /// cavacore's internal input ring-buffer capacity (`FFTbassbufferSize * channels`).
    input_buffer_size: usize,
    fft_bass: usize,
    fft_treble: usize,
    bass_cut_off_bar: i32,

    // --- adaptive/smoothing state ---
    autosens: bool,
    sens_init: bool,
    frame_skip: i32,
    sens: f64,
    framerate: f64,
    noise_reduction: f64,

    // --- FFT plans (reused for both channels) ---
    bass_fft: Arc<dyn RealToComplex<f64>>,
    treble_fft: Arc<dyn RealToComplex<f64>>,

    // --- reusable scratch/IO buffers ---
    in_bass_l: Vec<f64>,
    in_bass_r: Vec<f64>,
    in_l: Vec<f64>,
    in_r: Vec<f64>,
    out_bass_l: Vec<Complex<f64>>,
    out_bass_r: Vec<Complex<f64>>,
    out_l: Vec<Complex<f64>>,
    out_r: Vec<Complex<f64>>,
    bass_scratch: Vec<Complex<f64>>,
    treble_scratch: Vec<Complex<f64>>,

    // --- precomputed window + band tables ---
    bass_multiplier: Vec<f64>,
    multiplier: Vec<f64>,
    eq: Vec<f64>,
    lower_cut_off: Vec<i32>,
    upper_cut_off: Vec<i32>,

    // --- per-bar filter memory ---
    input_buffer: Vec<f64>,
    cava_fall: Vec<f64>,
    cava_mem: Vec<f64>,
    cava_peak: Vec<f64>,
    prev_cava_out: Vec<f64>,

    // --- output ---
    out: Vec<f64>,
}

impl CavaPlan {
    /// Port of `cava_init`. Assumes the caller (`CavaConfig::build`) has already
    /// range-checked the obviously-invalid arguments; the remaining cavacore
    /// checks return an error string (surfaced as [`CavaError::Init`]).
    fn new(cfg: &CavaConfig) -> Result<Self, String> {
        let bars = cfg.bars as usize;
        let channels = cfg.channels as usize;
        let rate = cfg.rate;

        if rate > 384_000 {
            return Err(format!("cava_init called with illegal sample rate: {rate}\n"));
        }

        let base = fft_buffer_size(rate);
        let fft_treble = base;
        let fft_bass = base * 2;

        if bars as i32 > (fft_treble / 2 + 1) as i32 {
            return Err(format!(
                "cava_init called with illegal number of bars: {bars}, for {rate} sample rate \
                 number of bars can't be more than {}\n",
                fft_treble / 2 + 1
            ));
        }

        let input_buffer_size = fft_bass * channels;

        // Hann windows.
        let bass_multiplier: Vec<f64> = (0..fft_bass)
            .map(|i| 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / (fft_bass - 1) as f64).cos()))
            .collect();
        let multiplier: Vec<f64> = (0..fft_treble)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / (fft_treble - 1) as f64).cos())
            })
            .collect();

        // FFT plans.
        let mut planner = RealFftPlanner::<f64>::new();
        let bass_fft = planner.plan_fft_forward(fft_bass);
        let treble_fft = planner.plan_fft_forward(fft_treble);

        // --- cut-off frequencies and eq (verbatim port of cava_init) ---
        let lower = cfg.low_cutoff_freq as i32;
        let upper = cfg.high_cutoff_freq as i32;
        let bass_cut_off = 100i32;

        // Frequency constant used to distribute bars across the band.
        let denom = (1.0f32 / (bars as f32 + 1.0) - 1.0) as f64;
        let frequency_constant = ((lower as f32 / upper as f32) as f64).log10() / denom;

        let mut cut_off_frequency = vec![0f32; bars + 1];
        let mut relative_cut_off = vec![0f32; bars + 1];
        let mut lower_cut_off = vec![0i32; bars + 1];
        let mut upper_cut_off = vec![0i32; bars + 1];

        let mut bass_cut_off_bar: i32 = 0;
        let mut first_bar: i32 = 1;

        // Integer division, exactly as C (`p->rate / p->FFTbassbufferSize`).
        let min_bandwidth = (rate as usize / fft_bass) as f32;

        for n in 0..bars + 1 {
            let mut bar_distribution_coefficient = -frequency_constant;
            bar_distribution_coefficient +=
                ((n as f32 + 1.0) / (bars as f32 + 1.0)) as f64 * frequency_constant;
            cut_off_frequency[n] = (upper as f64 * 10f64.powf(bar_distribution_coefficient)) as f32;

            if n > 0 && cut_off_frequency[n - 1] >= cut_off_frequency[n] {
                cut_off_frequency[n] = cut_off_frequency[n - 1] + min_bandwidth;
            }

            // remember nyquist! (rate / 2 is integer division in C)
            relative_cut_off[n] = cut_off_frequency[n] / (rate / 2) as f32;

            if cut_off_frequency[n] < bass_cut_off as f32 {
                // BASS
                lower_cut_off[n] = (relative_cut_off[n] * (fft_bass / 2) as f32) as i32;
                bass_cut_off_bar += 1;
                if bass_cut_off_bar > 1 {
                    first_bar = 0;
                }
                if lower_cut_off[n] > (fft_bass / 2) as i32 {
                    lower_cut_off[n] = (fft_bass / 2) as i32;
                }
            } else {
                // MID + TREBLE
                lower_cut_off[n] = (relative_cut_off[n] * (fft_treble / 2) as f32).ceil() as i32;
                if n as i32 == bass_cut_off_bar {
                    first_bar = 1;
                    if n > 0 {
                        upper_cut_off[n - 1] =
                            (relative_cut_off[n] * (fft_bass / 2) as f32 - 1.0) as i32;
                    }
                } else {
                    first_bar = 0;
                }
                if lower_cut_off[n] > (fft_treble / 2) as i32 {
                    lower_cut_off[n] = (fft_treble / 2) as i32;
                }
            }

            if n > 0 {
                if first_bar == 0 {
                    upper_cut_off[n - 1] = lower_cut_off[n] - 1;

                    // push the spectrum up if the exponential clumps in the bass
                    if lower_cut_off[n] <= lower_cut_off[n - 1] {
                        let room_for_more = if (n as i32) < bass_cut_off_bar {
                            lower_cut_off[n - 1] + 1 < (fft_bass / 2 + 1) as i32
                        } else {
                            lower_cut_off[n - 1] + 1 < (fft_treble / 2 + 1) as i32
                        };
                        if room_for_more {
                            lower_cut_off[n] = lower_cut_off[n - 1] + 1;
                            upper_cut_off[n - 1] = lower_cut_off[n] - 1;
                        }
                    }
                } else if upper_cut_off[n - 1] < lower_cut_off[n - 1] {
                    upper_cut_off[n - 1] = lower_cut_off[n - 1] + 1;
                }
            }

            // actual cut off frequency (float division, unlike the earlier int one)
            if (n as i32) < bass_cut_off_bar {
                relative_cut_off[n] = lower_cut_off[n] as f32 / (fft_bass as f32 / 2.0);
            } else {
                relative_cut_off[n] = lower_cut_off[n] as f32 / (fft_treble as f32 / 2.0);
            }
            cut_off_frequency[n] = relative_cut_off[n] * (rate as f32 / 2.0);
        }

        // hard-coded eq
        let mut eq = vec![0f64; bars];
        for n in 0..bars {
            eq[n] = 1.0 / 2f64.powi(28);
            eq[n] *= (cut_off_frequency[n + 1] as f64).powf(0.85);
            if (n as i32) < bass_cut_off_bar {
                eq[n] /= (fft_bass as f64).log2();
            } else {
                eq[n] /= (fft_treble as f64).log2();
            }
            eq[n] /= (upper_cut_off[n] - lower_cut_off[n] + 1) as f64;
        }

        Ok(CavaPlan {
            bars,
            channels,
            rate,
            input_buffer_size,
            fft_bass,
            fft_treble,
            bass_cut_off_bar,
            autosens: cfg.autosens,
            sens_init: true,
            frame_skip: 1,
            sens: 1.0,
            framerate: 75.0,
            noise_reduction: cfg.noise_reduction,
            in_bass_l: vec![0.0; fft_bass],
            in_bass_r: vec![0.0; fft_bass],
            in_l: vec![0.0; fft_treble],
            in_r: vec![0.0; fft_treble],
            out_bass_l: vec![Complex::default(); fft_bass / 2 + 1],
            out_bass_r: vec![Complex::default(); fft_bass / 2 + 1],
            out_l: vec![Complex::default(); fft_treble / 2 + 1],
            out_r: vec![Complex::default(); fft_treble / 2 + 1],
            bass_scratch: bass_fft.make_scratch_vec(),
            treble_scratch: treble_fft.make_scratch_vec(),
            bass_fft,
            treble_fft,
            bass_multiplier,
            multiplier,
            eq,
            lower_cut_off,
            upper_cut_off,
            input_buffer: vec![0.0; input_buffer_size],
            cava_fall: vec![0.0; bars * channels],
            cava_mem: vec![0.0; bars * channels],
            cava_peak: vec![0.0; bars * channels],
            prev_cava_out: vec![0.0; bars * channels],
            out: vec![0.0; bars * channels],
        })
    }

    /// Bars per channel.
    pub fn bars(&self) -> usize {
        self.bars
    }

    /// Configured channel count (1 or 2).
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Configured input sample rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Length of the slice returned by [`execute`](Self::execute):
    /// `bars * channels`.
    pub fn output_len(&self) -> usize {
        self.bars * self.channels
    }

    /// cavacore's internal input ring-buffer capacity. Passing more than this
    /// many samples to [`execute`](Self::execute) in one call simply discards
    /// the excess, so callers should chunk their input below this bound to avoid
    /// dropping audio.
    pub fn max_input_samples(&self) -> usize {
        self.input_buffer_size
    }

    /// Process a chunk of interleaved PCM samples and return the updated bars.
    ///
    /// The returned slice has length [`output_len`](Self::output_len); for
    /// stereo it is all left-channel bars (low→high) followed by all
    /// right-channel bars. The slice is owned by `self` and stays valid until
    /// the next call to `execute`.
    ///
    /// `input` may be any length, including empty (which advances the smoothing
    /// filters without injecting new audio). For stereo the data is interleaved,
    /// so a trailing partial frame (an input length that is not a multiple of
    /// [`channels`](Self::channels)) is ignored — feeding a half frame otherwise
    /// yields NaN output that would poison the filter state.
    pub fn execute(&mut self, input: &[f64]) -> &[f64] {
        // Only feed whole interleaved frames. A trailing sample that doesn't
        // complete a frame is dropped (at most `channels - 1` samples, which a
        // frame-aligned capture never produces).
        let usable = input.len() - (input.len() % self.channels);
        // cavacore reads at most `input_buffer_size` new samples; anything more
        // is discarded.
        let new_samples = usable.min(self.input_buffer_size);
        let input = &input[..new_samples];

        let channels = self.channels;
        let mut silence = true;

        if new_samples > 0 {
            // Approximate the actual framerate (off by ~+10% at 60 fps, close
            // enough for autosens/smoothing to adapt).
            self.framerate -= self.framerate / 64.0;
            self.framerate += (self.rate as i64 * self.frame_skip as i64) as f64
                / (new_samples / channels) as f64
                / 64.0;
            self.frame_skip = 1;

            // Shift existing samples up by `new_samples` (memmove semantics).
            self.input_buffer
                .copy_within(0..self.input_buffer_size - new_samples, new_samples);

            // Fill the front of the buffer with the new samples, reversed.
            for (n, &v) in input.iter().enumerate() {
                self.input_buffer[new_samples - n - 1] = v;
                if v != 0.0 {
                    silence = false;
                }
            }
        } else {
            self.frame_skip += 1;
        }

        // Fill + Hann-window the bass and treble input buffers straight from the
        // ring buffer (cavacore keeps separate "raw" copies; the window is the
        // only thing done to them, so we fold it in).
        for n in 0..self.fft_bass {
            let (l, r) = if channels == 2 {
                (self.input_buffer[n * 2 + 1], self.input_buffer[n * 2])
            } else {
                (self.input_buffer[n], 0.0)
            };
            self.in_bass_l[n] = self.bass_multiplier[n] * l;
            if channels == 2 {
                self.in_bass_r[n] = self.bass_multiplier[n] * r;
            }
        }
        for n in 0..self.fft_treble {
            let (l, r) = if channels == 2 {
                (self.input_buffer[n * 2 + 1], self.input_buffer[n * 2])
            } else {
                (self.input_buffer[n], 0.0)
            };
            self.in_l[n] = self.multiplier[n] * l;
            if channels == 2 {
                self.in_r[n] = self.multiplier[n] * r;
            }
        }

        // Execute the FFTs. `process_with_scratch` may mutate the input buffers,
        // which is fine — they are fully rewritten every call above.
        self.bass_fft
            .process_with_scratch(&mut self.in_bass_l, &mut self.out_bass_l, &mut self.bass_scratch)
            .expect("bass fft (left) length invariant");
        self.treble_fft
            .process_with_scratch(&mut self.in_l, &mut self.out_l, &mut self.treble_scratch)
            .expect("treble fft (left) length invariant");
        if channels == 2 {
            self.bass_fft
                .process_with_scratch(
                    &mut self.in_bass_r,
                    &mut self.out_bass_r,
                    &mut self.bass_scratch,
                )
                .expect("bass fft (right) length invariant");
            self.treble_fft
                .process_with_scratch(&mut self.in_r, &mut self.out_r, &mut self.treble_scratch)
                .expect("treble fft (right) length invariant");
        }

        // Separate frequency bands: sum FFT magnitudes within each bar's bins.
        let bars = self.bars;
        for n in 0..bars {
            let mut temp_l = 0.0f64;
            let mut temp_r = 0.0f64;

            let lo = self.lower_cut_off[n];
            let hi = self.upper_cut_off[n];
            let mut i = lo;
            while i <= hi {
                let idx = i as usize;
                if (n as i32) < self.bass_cut_off_bar {
                    temp_l += self.out_bass_l[idx].re.hypot(self.out_bass_l[idx].im);
                    if channels == 2 {
                        temp_r += self.out_bass_r[idx].re.hypot(self.out_bass_r[idx].im);
                    }
                } else {
                    temp_l += self.out_l[idx].re.hypot(self.out_l[idx].im);
                    if channels == 2 {
                        temp_r += self.out_r[idx].re.hypot(self.out_r[idx].im);
                    }
                }
                i += 1;
            }

            temp_l *= self.eq[n];
            self.out[n] = temp_l;
            if channels == 2 {
                temp_r *= self.eq[n];
                self.out[n + bars] = temp_r;
            }
        }

        // Apply sensitivity.
        if self.autosens {
            for v in &mut self.out {
                *v *= self.sens;
            }
        }

        // Smoothing (gravity falloff + integral) and autosens overshoot check.
        let mut overshoot = false;
        let framerate_mod = 66.0 / self.framerate;
        let gravity_mod = framerate_mod.powf(2.5) * 2.0 / self.noise_reduction;
        let integral_mod = framerate_mod.powf(0.1);

        for n in 0..bars * channels {
            // falloff
            if self.out[n] < self.prev_cava_out[n] && self.noise_reduction > 0.1 {
                self.out[n] =
                    self.cava_peak[n] * (1.0 - (self.cava_fall[n] * self.cava_fall[n] * gravity_mod));
                if self.out[n] < 0.0 {
                    self.out[n] = 0.0;
                }
                self.cava_fall[n] += 0.028;
            } else {
                self.cava_peak[n] = self.out[n];
                self.cava_fall[n] = 0.0;
            }
            self.prev_cava_out[n] = self.out[n];

            // integral
            self.out[n] += self.cava_mem[n] * self.noise_reduction / integral_mod;
            self.cava_mem[n] = self.out[n];

            if self.autosens && self.out[n] > 1.0 {
                overshoot = true;
                self.out[n] = 1.0;
            }
        }

        // Automatic sensitivity adjustment.
        if self.autosens {
            if overshoot {
                self.sens *= 1.0 - 0.02 * framerate_mod;
                self.sens_init = false;
            } else if !silence {
                self.sens *= 1.0 + 0.001 * framerate_mod;
                if self.sens_init {
                    self.sens *= 1.0 + 0.1 * framerate_mod;
                }
            }
        }

        &self.out
    }

    /// The most recent output without running the filters again.
    pub fn last_output(&self) -> &[f64] {
        &self.out
    }
}

impl fmt::Debug for CavaPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CavaPlan")
            .field("bars", &self.bars)
            .field("channels", &self.channels)
            .field("rate", &self.rate)
            .field("max_input_samples", &self.input_buffer_size)
            .finish()
    }
}
