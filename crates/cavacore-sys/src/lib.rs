// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw FFI bindings to [cavacore], the core audio-visualization engine of CAVA.
//!
//! cavacore wraps FFTW to turn interleaved PCM audio into a small number of
//! smoothed, logarithmically-spaced frequency "bars". This crate exposes the
//! three C entry points and the `cava_plan` struct verbatim; see the
//! [`cavacore-rs`](https://docs.rs/cavacore-rs) crate for a safe wrapper.
//!
//! [cavacore]: https://github.com/karlstav/cava/blob/master/CAVACORE.md
#![allow(non_camel_case_types, non_snake_case)]

use std::os::raw::{c_char, c_double, c_float, c_int, c_uint, c_void};

/// FFTW plan handle (`fftw_plan`) — an opaque pointer on the C side.
type fftw_plan = *mut c_void;
/// `fftw_complex` is `double[2]`; we only ever hold pointers to it.
type fftw_complex = [c_double; 2];

/// Internal cavacore state, allocated and owned by the C library.
///
/// The layout mirrors `struct cava_plan` from `cavacore.h` exactly so that the
/// `status` and `error_message` fields can be read back after [`cava_init`].
/// All other fields are private cavacore bookkeeping and must not be modified.
#[repr(C)]
pub struct cava_plan {
    pub FFTbassbufferSize: c_int,
    pub FFTbufferSize: c_int,
    pub number_of_bars: c_int,
    pub audio_channels: c_int,
    pub input_buffer_size: c_int,
    pub rate: c_int,
    pub bass_cut_off_bar: c_int,
    pub sens_init: c_int,
    pub autosens: c_int,
    pub frame_skip: c_int,
    /// `0` on success, `-1` if `cava_init` was called with an illegal argument.
    pub status: c_int,
    /// NUL-terminated diagnostic string, valid when `status != 0`.
    pub error_message: [c_char; 1024],

    pub sens: c_double,
    pub framerate: c_double,
    pub noise_reduction: c_double,

    pub p_bass_l: fftw_plan,
    pub p_bass_r: fftw_plan,
    pub p_l: fftw_plan,
    pub p_r: fftw_plan,

    pub out_bass_l: *mut fftw_complex,
    pub out_bass_r: *mut fftw_complex,
    pub out_l: *mut fftw_complex,
    pub out_r: *mut fftw_complex,

    pub bass_multiplier: *mut c_double,
    pub multiplier: *mut c_double,

    pub in_bass_r_raw: *mut c_double,
    pub in_bass_l_raw: *mut c_double,
    pub in_r_raw: *mut c_double,
    pub in_l_raw: *mut c_double,
    pub in_bass_r: *mut c_double,
    pub in_bass_l: *mut c_double,
    pub in_r: *mut c_double,
    pub in_l: *mut c_double,
    pub prev_cava_out: *mut c_double,
    pub cava_mem: *mut c_double,
    pub input_buffer: *mut c_double,
    pub cava_peak: *mut c_double,

    pub eq: *mut c_double,

    pub cut_off_frequency: *mut c_float,
    pub FFTbuffer_lower_cut_off: *mut c_int,
    pub FFTbuffer_upper_cut_off: *mut c_int,
    pub cava_fall: *mut c_double,
}

unsafe extern "C" {
    /// Initialize a visualization plan. Returns an owned pointer that must be
    /// released with [`cava_destroy`]. Check `(*plan).status` for errors.
    ///
    /// - `number_of_bars`: bars per channel
    /// - `rate`: input sample rate (Hz)
    /// - `channels`: interleaved channel count, 1 or 2
    /// - `autosens`: 1 = auto-scale output to 0..1, 0 = raw values
    /// - `noise_reduction`: 0.0..=1.0 (recommended 0.77)
    /// - `low_cut_off` / `high_cut_off`: visualization band in Hz (e.g. 50, 10000)
    pub fn cava_init(
        number_of_bars: c_int,
        rate: c_uint,
        channels: c_int,
        autosens: c_int,
        noise_reduction: c_double,
        low_cut_off: c_int,
        high_cut_off: c_int,
    ) -> *mut cava_plan;

    /// Process `new_samples` interleaved samples from `cava_in`, writing
    /// `number_of_bars * channels` values to `cava_out` (left bars then right).
    pub fn cava_execute(
        cava_in: *mut c_double,
        new_samples: c_int,
        cava_out: *mut c_double,
        plan: *mut cava_plan,
    );

    /// Free a plan returned by [`cava_init`].
    pub fn cava_destroy(plan: *mut cava_plan);
}
