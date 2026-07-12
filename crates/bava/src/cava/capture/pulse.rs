// SPDX-License-Identifier: MIT OR Apache-2.0
//! PulseAudio capture backend.
//!
//! Records the monitor source of the default sink via the PulseAudio "simple"
//! API. On a PipeWire system this transparently goes through `pipewire-pulse`.
//! Samples are captured as native-endian `f32` and converted to `f64` for
//! cavacore.

use std::cell::RefCell;
use std::rc::Rc;

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::introspect::ServerInfo;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::operation::State as OperationState;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use super::{AudioCapture, CaptureError};

/// A PulseAudio recording stream feeding interleaved `f64` samples.
pub struct PulseCapture {
    simple: Simple,
    rate: u32,
    channels: u32,
    /// Scratch byte buffer reused across reads to avoid per-call allocation.
    byte_buf: Vec<u8>,
}

impl PulseCapture {
    /// Open a capture stream on `device`, or the default sink's monitor when
    /// `device` is `None`. `frame_samples` is the per-channel read size the
    /// caller will use; it sets the server's record `fragsize` so audio is
    /// delivered one read at a time at a steady cadence (low latency, no bursty
    /// freeze-then-jump in the bars).
    pub fn open(
        device: Option<&str>,
        rate: u32,
        channels: u8,
        frame_samples: usize,
    ) -> Result<Self, CaptureError> {
        let spec = Spec {
            format: Format::F32le,
            channels,
            rate,
        };
        if !spec.is_valid() {
            return Err(CaptureError::Init(format!(
                "invalid sample spec: rate={rate} channels={channels}"
            )));
        }

        // Resolve the monitor source if the caller didn't pin one.
        let resolved;
        let device = match device {
            Some(d) => Some(d),
            None => {
                resolved = default_monitor_source()?;
                Some(resolved.as_str())
            }
        };

        // Deliver in fragments of exactly one read. With PulseAudio's default
        // attributes the monitor source is handed over in large bursts, which
        // at a high render rate shows up as the bars freezing for several frames
        // and then jumping. A read-sized `fragsize` keeps the cadence steady.
        // `u32::MAX` (i.e. `(uint32_t)-1`) means "let the server decide".
        let frag_bytes = frame_samples
            .saturating_mul(channels as usize)
            .saturating_mul(std::mem::size_of::<f32>())
            .max(1);
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: frag_bytes.min(u32::MAX as usize) as u32,
        };

        let simple = Simple::new(
            None,             // default server
            "bava",           // application name
            Direction::Record,
            device,           // monitor source
            "visualizer",     // stream description
            &spec,
            None,         // default channel map
            Some(&attr),  // low-latency, steady-cadence buffering
        )
        .map_err(|e| CaptureError::Init(format!("{e} (device={device:?})")))?;

        Ok(Self {
            simple,
            rate,
            channels: channels as u32,
            byte_buf: Vec::new(),
        })
    }
}

impl AudioCapture for PulseCapture {
    fn read(&mut self, buf: &mut [f64]) -> Result<(), CaptureError> {
        // Read exactly enough bytes to fill `buf` worth of f32 samples.
        let want_bytes = buf.len() * std::mem::size_of::<f32>();
        if self.byte_buf.len() != want_bytes {
            self.byte_buf.resize(want_bytes, 0);
        }

        // Note: PAErr's inherent `to_string` returns Option<String>, so format
        // via its Display impl instead.
        self.simple
            .read(&mut self.byte_buf)
            .map_err(|e| CaptureError::Read(format!("{e}")))?;

        for (i, chunk) in self.byte_buf.as_chunks::<4>().0.iter().enumerate() {
            let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            buf[i] = sample as f64;
        }
        Ok(())
    }

    fn rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> u32 {
        self.channels
    }
}

/// Resolve the monitor source name of the current default sink, e.g.
/// `alsa_output.pci-0000_00_1f.3.analog-stereo.monitor`.
///
/// Uses a short-lived PulseAudio mainloop to query the server's default sink
/// name and appends `.monitor`.
pub fn default_monitor_source() -> Result<String, CaptureError> {
    let mut mainloop = Mainloop::new()
        .ok_or_else(|| CaptureError::Init("failed to create pulse mainloop".into()))?;

    let mut context = Context::new(&mainloop, "bava-probe")
        .ok_or_else(|| CaptureError::Init("failed to create pulse context".into()))?;

    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|e| CaptureError::Init(format!("pulse connect failed: {e}")))?;

    // Pump the mainloop until the context is ready (or fails).
    loop {
        match mainloop.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) => {
                return Err(CaptureError::Init("pulse mainloop quit during connect".into()));
            }
            IterateResult::Err(e) => {
                return Err(CaptureError::Init(format!("pulse mainloop error: {e}")));
            }
        }
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                return Err(CaptureError::Init("pulse context failed to connect".into()));
            }
            _ => {}
        }
    }

    // Query the default sink name.
    let sink_name: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let sink_name_cb = sink_name.clone();
    let op = context.introspect().get_server_info(move |info: &ServerInfo| {
        if let Some(name) = &info.default_sink_name {
            *sink_name_cb.borrow_mut() = Some(name.to_string());
        }
    });

    while op.get_state() == OperationState::Running {
        match mainloop.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) | IterateResult::Err(_) => {
                return Err(CaptureError::Init("pulse mainloop error during query".into()));
            }
        }
    }

    let sink = sink_name
        .borrow_mut()
        .take()
        .ok_or_else(|| CaptureError::Init("no default sink reported by server".into()))?;

    Ok(format!("{sink}.monitor"))
}

/// Resolve the monitor source of the sink that currently has an *active* stream
/// — i.e. wherever sound is actually coming out, which need not be the default
/// sink (e.g. media routed to an HDMI/display output while headphones are
/// default). Returns `None` when nothing is actively playing, or on any query
/// error; the caller keeps its current source in that case (so a pause doesn't
/// yank capture away from the sink you were just listening to). When several
/// streams play at once the most recently started (highest sink-input index)
/// wins. Works through `pipewire-pulse` too, so it serves the native PipeWire
/// backend as well as PulseAudio.
///
/// This runs over a short-lived mainloop, so it's only suitable for the periodic
/// poll on the capture thread — not per read.
pub fn active_monitor_source() -> Option<String> {
    let mut mainloop = Mainloop::new()?;
    let mut context = Context::new(&mainloop, "bava-follow")?;
    context.connect(None, ContextFlagSet::NOFLAGS, None).ok()?;

    // Pump until the context is ready (or fails).
    loop {
        match mainloop.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) | IterateResult::Err(_) => return None,
        }
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => return None,
            _ => {}
        }
    }

    // Collect the (sink-input index, sink index) of every playing stream — one
    // that is neither corked (paused) nor muted, since neither emits sound.
    let playing: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
    let playing_cb = playing.clone();
    let op = context.introspect().get_sink_input_info_list(move |res| {
        if let ListResult::Item(info) = res
            && !info.corked
            && !info.mute
        {
            playing_cb.borrow_mut().push((info.index, info.sink));
        }
    });
    while op.get_state() == OperationState::Running {
        match mainloop.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) | IterateResult::Err(_) => return None,
        }
    }

    // Most recently started stream (highest index) wins the sink.
    let sink_idx = playing
        .borrow()
        .iter()
        .max_by_key(|(input_idx, _)| *input_idx)
        .map(|(_, sink_idx)| *sink_idx)?;

    // Resolve that sink's monitor source name.
    let monitor: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let monitor_cb = monitor.clone();
    let op = context.introspect().get_sink_info_by_index(sink_idx, move |res| {
        if let ListResult::Item(info) = res
            && let Some(name) = &info.monitor_source_name
        {
            *monitor_cb.borrow_mut() = Some(name.to_string());
        }
    });
    while op.get_state() == OperationState::Running {
        match mainloop.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) | IterateResult::Err(_) => return None,
        }
    }

    
    monitor.borrow_mut().take()
}
