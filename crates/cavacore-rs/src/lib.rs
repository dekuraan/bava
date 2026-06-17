// SPDX-License-Identifier: GPL-3.0-or-later
//! Safe, idiomatic wrapper around [`cavacore-sys`].
//!
//! [`CavaPlan`] turns interleaved PCM audio into a small set of smoothed,
//! logarithmically-spaced frequency bars — the signal that drives a CAVA-style
//! visualization.
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
//! A [`CavaPlan`] owns its cavacore state exclusively and is [`Send`], so it can
//! be moved onto a dedicated audio thread. It is intentionally **not** [`Sync`]:
//! [`CavaPlan::execute`] mutates internal buffers and takes `&mut self`.
//!
//! Construction and destruction call into FFTW's planner, whose global state is
//! not thread-safe, so this crate serializes [`CavaConfig::build`] and the
//! [`Drop`] of [`CavaPlan`] behind a process-wide lock. You may therefore build
//! and drop plans from any thread freely.

use std::ffi::CStr;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use cavacore_sys::{cava_destroy, cava_execute, cava_init, cava_plan};

/// Serializes all FFTW planning/teardown (`cava_init` / `cava_destroy`), which
/// share non-reentrant global state inside FFTW.
fn fftw_planner_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

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

impl CavaConfig {
    /// Validate the configuration and construct a [`CavaPlan`].
    ///
    /// Parameters are checked in Rust first (so the errors are descriptive),
    /// then handed to `cava_init`; any remaining complaint from cavacore is
    /// surfaced as [`CavaError::Init`].
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

        // SAFETY: arguments are range-checked above. cava_init allocates and
        // returns an owned pointer. FFTW planning inside cava_init is not
        // thread-safe, so we hold the process-wide planner lock across the call.
        let plan = {
            let _guard = fftw_planner_lock().lock().unwrap_or_else(|e| e.into_inner());
            unsafe {
                cava_init(
                    self.bars as i32,
                    self.rate,
                    self.channels as i32,
                    self.autosens as i32,
                    self.noise_reduction,
                    self.low_cutoff_freq as i32,
                    self.high_cutoff_freq as i32,
                )
            }
        };

        if plan.is_null() {
            return Err(CavaError::NullPlan);
        }

        // SAFETY: plan is non-null and points at an initialized cava_plan.
        let status = unsafe { (*plan).status };
        if status != 0 {
            // SAFETY: error_message is a NUL-terminated buffer owned by the plan.
            let msg = unsafe { CStr::from_ptr((*plan).error_message.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            // Free the failed plan before returning.
            let _guard = fftw_planner_lock().lock().unwrap_or_else(|e| e.into_inner());
            unsafe { cava_destroy(plan) };
            return Err(CavaError::Init(msg));
        }

        // SAFETY: cavacore set input_buffer_size during a successful init.
        let max_input_samples = unsafe { (*plan).input_buffer_size } as usize;

        let bars = self.bars as usize;
        let channels = self.channels as usize;
        Ok(CavaPlan {
            plan,
            out: vec![0.0; bars * channels],
            bars,
            channels,
            rate: self.rate,
            max_input_samples,
        })
    }
}

/// A live cavacore visualization plan.
///
/// Feed it interleaved PCM with [`CavaPlan::execute`] and read back
/// `bars * channels` smoothed magnitudes. The plan owns its cavacore allocation
/// and frees it on drop.
pub struct CavaPlan {
    plan: *mut cava_plan,
    out: Vec<f64>,
    bars: usize,
    channels: usize,
    rate: u32,
    max_input_samples: usize,
}

// SAFETY: a CavaPlan has unique ownership of its cava_plan pointer and never
// shares it. cavacore's per-plan FFTW execute path is safe to run from a single
// thread at a time, and `execute` takes `&mut self`, so moving the whole plan to
// another thread and using it exclusively there is sound. We do NOT implement
// Sync: concurrent `&`-access is not permitted.
unsafe impl Send for CavaPlan {}

impl CavaPlan {
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
        self.max_input_samples
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
    /// [`channels`](Self::channels)) is ignored — feeding cavacore a half frame
    /// otherwise yields NaN output that would poison the filter state.
    pub fn execute(&mut self, input: &[f64]) -> &[f64] {
        // Only feed whole interleaved frames. A trailing sample that doesn't
        // complete a frame is dropped (at most `channels - 1` samples, which a
        // frame-aligned capture never produces).
        let usable = input.len() - (input.len() % self.channels);
        // cavacore reads at most `input_buffer_size` new samples; anything more
        // is discarded internally. We also clamp `new_samples` to i32::MAX to
        // keep the C `int` argument well-defined for absurd inputs.
        let new_samples = usable.min(i32::MAX as usize) as i32;

        // SAFETY:
        // - `self.plan` is a valid, non-null plan for the lifetime of `self`.
        // - cava_execute only *reads* `cava_in[0..new_samples]`; it never writes
        //   through that pointer, so casting a shared slice to `*mut` is sound
        //   (no aliased `&mut` is ever created). Passing a null/zero-length
        //   pointer is fine because new_samples is 0 in that case.
        // - `cava_out` points at `self.out`, whose length is exactly
        //   `bars * channels`, matching cavacore's write size.
        unsafe {
            cava_execute(
                input.as_ptr() as *mut f64,
                new_samples,
                self.out.as_mut_ptr(),
                self.plan,
            );
        }
        &self.out
    }

    /// The most recent output without running the filters again.
    pub fn last_output(&self) -> &[f64] {
        &self.out
    }
}

impl Drop for CavaPlan {
    fn drop(&mut self) {
        if !self.plan.is_null() {
            // SAFETY: we own `self.plan` and it has not been freed. FFTW teardown
            // shares global state with planning, so hold the planner lock.
            let _guard = fftw_planner_lock().lock().unwrap_or_else(|e| e.into_inner());
            unsafe { cava_destroy(self.plan) };
            self.plan = std::ptr::null_mut();
        }
    }
}

impl fmt::Debug for CavaPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CavaPlan")
            .field("bars", &self.bars)
            .field("channels", &self.channels)
            .field("rate", &self.rate)
            .field("max_input_samples", &self.max_input_samples)
            .finish()
    }
}
