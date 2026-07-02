// SPDX-License-Identifier: MIT OR Apache-2.0
//! The cavacore subsystem.
//!
//! A background thread captures audio into a ring buffer; a Bevy system drains
//! that buffer and calls `cava_execute` **once per rendered frame**, so cavacore
//! runs at the render rate. Its framerate-adaptive smoothing then produces
//! native, low-latency motion at whatever FPS the window runs — no interpolation
//! needed. The result is published into the [`Cava`] resource for visualizers.

pub mod capture;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use bevy::prelude::*;
use cavacore_rs::{CavaConfig, CavaPlan};

/// Tunables for the cavacore pipeline. Insert your own before adding
/// [`CavaPlugin`] to override the defaults.
#[derive(Resource, Clone, Debug)]
pub struct CavaSettings {
    /// Bars per channel.
    pub bars_per_channel: usize,
    /// Channels to capture (1 or 2).
    pub channels: usize,
    /// Capture sample rate (Hz).
    pub rate: u32,
    /// Samples per channel per cavacore execution. cavacore needs a *steady*
    /// count per call for its framerate estimate / autosens, so this fixes the
    /// chunk size and thus the cava update rate: rate·channels / (this·channels)
    /// executions per second. Smaller = higher cava rate = smoother/snappier
    /// (128 @ 44100 ≈ 344 Hz); larger = slower. The render samples the latest
    /// bars regardless.
    pub frame_samples: usize,
    /// Auto-scale output into 0..1.
    pub autosens: bool,
    /// Smoothing factor 0..1 (cavacore recommends 0.77).
    pub noise_reduction: f64,
    /// Low edge of the visualized band (Hz).
    pub low_cutoff_freq: u32,
    /// High edge of the visualized band (Hz).
    pub high_cutoff_freq: u32,
    /// Optional explicit capture source; `None` resolves the default sink monitor.
    pub source: Option<String>,
    /// When `source` is unset, follow whichever sink is *actively playing* (the
    /// HDMI output you routed media to, say) instead of pinning the default
    /// sink's monitor. Re-checked periodically on the capture thread; a pinned
    /// `source` disables it. Linux only — ignored on Windows/macOS, which always
    /// loop back the default render endpoint / system mix.
    pub follow_active_sink: bool,
    /// Log input/output signal levels about once per second.
    pub debug: bool,
}

impl Default for CavaSettings {
    fn default() -> Self {
        Self {
            bars_per_channel: 24,
            channels: 2,
            rate: 44_100,
            frame_samples: 128,
            autosens: true,
            noise_reduction: 0.77,
            low_cutoff_freq: 50,
            high_cutoff_freq: 10_000,
            source: None,
            follow_active_sink: true,
            debug: false,
        }
    }
}

/// Request to rebuild the cavacore plan from the current [`CavaSettings`].
///
/// Set `.0 = true` (e.g. from the settings editor) after changing DSP-relevant
/// fields — `bars_per_channel`, `autosens`, `noise_reduction`, the cutoffs — and
/// [`rebuild_cava`] picks it up next frame. The capture thread's rate/channels
/// are fixed at startup, so those are kept as-is during a rebuild.
#[derive(Resource, Default)]
pub struct CavaRebuild(pub bool);

/// Latest visualization bars, refreshed each frame from the capture thread.
///
/// For stereo, [`bars`](Self::bars) is all left-channel bars (low→high) followed
/// by all right-channel bars. Use [`left`](Self::left) / [`right`](Self::right)
/// / [`mono`](Self::mono) for convenient access. Values are smoothed and, with
/// auto-sensitivity on, roughly in `0.0..=1.0`.
#[derive(Resource, Default, Debug)]
pub struct Cava {
    pub bars: Vec<f32>,
    pub bars_per_channel: usize,
    pub channels: usize,
}

impl Cava {
    /// Left-channel bars (or the only channel in mono).
    pub fn left(&self) -> &[f32] {
        let n = self.bars_per_channel.min(self.bars.len());
        &self.bars[..n]
    }

    /// Right-channel bars; empty for mono input.
    pub fn right(&self) -> &[f32] {
        if self.channels < 2 {
            return &[];
        }
        let start = self.bars_per_channel.min(self.bars.len());
        let end = (self.bars_per_channel * 2).min(self.bars.len());
        &self.bars[start..end]
    }

    /// Per-bar magnitude averaged across channels.
    pub fn mono(&self) -> Vec<f32> {
        let n = self.bars_per_channel;
        if n == 0 {
            return Vec::new();
        }
        let left = self.left();
        let right = self.right();
        (0..n)
            .map(|i| {
                let l = left.get(i).copied().unwrap_or(0.0);
                if right.is_empty() {
                    l
                } else {
                    (l + right.get(i).copied().unwrap_or(0.0)) * 0.5
                }
            })
            .collect()
    }
}

/// Ring buffer of captured interleaved samples, shared between the capture
/// thread (producer) and the per-frame [`feed_cava`] system (consumer).
#[derive(Resource, Clone)]
struct AudioRing {
    buf: Arc<Mutex<VecDeque<f64>>>,
    running: Arc<AtomicBool>,
    /// Maximum backlog; a render stall can't grow the buffer past this.
    cap: usize,
    /// The sample rate the capture backend actually negotiated, published by the
    /// capture thread once [`capture::open`] succeeds (0 until then). Backends
    /// like PipeWire/WASAPI deliver at the device's native rate rather than the
    /// requested one; cavacore's framerate-adaptive smoothing is tuned to
    /// `plan.rate / frame_samples`, so the plan must be rebuilt to the *real*
    /// delivery rate or the smoothing runs fast/slow by the rate ratio.
    negotiated_rate: Arc<AtomicU32>,
    /// The channel count the capture backend actually negotiated (0 until known).
    /// Like the rate, PipeWire's graph converter is *asked* for the exact channel
    /// count but may negotiate a different one (e.g. a mono or 5.1 monitor); the
    /// interleaved stream would then be deinterleaved with the wrong stride unless
    /// the plan is rebuilt to match. Published alongside `negotiated_rate`.
    negotiated_channels: Arc<AtomicU32>,
}

impl AudioRing {
    fn new(rate: u32, channels: usize) -> Self {
        // Bound the backlog to ~250 ms so latency stays low even if rendering
        // pauses (e.g. window minimized); oldest samples are dropped first.
        let cap = (rate as usize / 4).max(1) * channels.max(1);
        Self {
            buf: Arc::new(Mutex::new(VecDeque::with_capacity(cap))),
            running: Arc::new(AtomicBool::new(true)),
            cap,
            negotiated_rate: Arc::new(AtomicU32::new(0)),
            negotiated_channels: Arc::new(AtomicU32::new(0)),
        }
    }
}

/// cavacore state. Held as a **NonSend** resource: cava runs per-frame on the
/// Bevy main thread, so the plan lives and executes there exclusively (there's
/// no need to make it a `Send + Sync` resource just to keep it on one thread).
struct CavaState {
    plan: CavaPlan,
    /// Captured samples awaiting a full chunk. cavacore's framerate estimate and
    /// autosens assume a *steady* sample count per execute, so we feed it fixed
    /// chunks rather than "whatever arrived this frame".
    accum: VecDeque<f64>,
    /// Reused contiguous buffer for the current chunk handed to cavacore.
    scratch: Vec<f64>,
}

/// Offline analysis systems (`rebuild_cava` + `feed_cava`), which run in
/// `PreUpdate` when [`CavaPlugin::offline`] is set. The record driver orders
/// itself `.before()` this set so its injected samples are analyzed the same
/// frame, and every `Update` renderer then draws from that fresh [`Cava`] —
/// a scheduler-ambiguous feed/draw order would make output nondeterministic.
#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OfflineCavaSet;

/// Pushes decoded samples straight into the audio ring, for offline rendering
/// (`--input`). Inserted only by [`CavaPlugin`] in offline mode, where there is
/// no capture thread; the record driver pushes each video frame's worth of
/// samples before [`feed_cava`] drains them.
#[derive(Resource, Clone)]
pub struct AudioInjector {
    ring: AudioRing,
}

impl AudioInjector {
    /// Append interleaved samples for [`feed_cava`] to consume this frame.
    /// Unlike the capture thread, this never evicts a backlog — the consumer
    /// drains every frame and dropping samples would desync audio and video.
    pub fn push(&self, samples: &[f64]) {
        if let Ok(mut q) = self.ring.buf.lock() {
            q.extend(samples.iter().copied());
        }
    }
}

/// Drives audio capture → cavacore (at render rate) → the [`Cava`] resource.
///
/// With [`offline`](Self::offline) set (`--input` video rendering), no capture
/// thread is spawned and no rate reconciliation runs: the plan is built exactly
/// at [`CavaSettings`]'s rate/channels (the decoded file's format) and samples
/// arrive via [`AudioInjector`].
#[derive(Default)]
pub struct CavaPlugin {
    pub offline: bool,
}

impl Plugin for CavaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CavaSettings>()
            .init_resource::<Cava>()
            .init_resource::<CavaRebuild>();
        let settings = app.world().resource::<CavaSettings>().clone();

        // Size the Cava resource up front.
        {
            let mut cava = app.world_mut().resource_mut::<Cava>();
            cava.bars = vec![0.0; settings.bars_per_channel * settings.channels];
            cava.bars_per_channel = settings.bars_per_channel;
            cava.channels = settings.channels;
        }

        // Build the cavacore plan on the main thread and keep it there.
        let cfg = CavaConfig {
            bars: settings.bars_per_channel as u32,
            rate: settings.rate,
            channels: settings.channels as u32,
            autosens: settings.autosens,
            noise_reduction: settings.noise_reduction,
            low_cutoff_freq: settings.low_cutoff_freq,
            high_cutoff_freq: settings.high_cutoff_freq,
        };
        match cfg.build() {
            Ok(plan) => {
                info!(
                    "bava: cavacore ready — {} bars/ch @ {} Hz, driven at render rate",
                    plan.bars(),
                    settings.rate
                );
                app.insert_non_send(CavaState {
                    plan,
                    accum: VecDeque::new(),
                    scratch: Vec::new(),
                });
            }
            Err(e) => error!("bava: cavacore init failed: {e}; visualizer will be idle"),
        }

        let ring = AudioRing::new(settings.rate, settings.channels);

        if self.offline {
            // Offline rendering: samples are injected per video frame by the
            // record driver, and the plan's rate/channels are already exact
            // (they came from the decoded file), so no capture thread and no
            // negotiated-rate reconciliation.
            app.insert_resource(AudioInjector { ring: ring.clone() })
                .insert_resource(ring)
                .add_systems(
                    PreUpdate,
                    (rebuild_cava, feed_cava).chain().in_set(OfflineCavaSet),
                );
            return;
        }

        // Spawn the audio reader thread feeding the ring.
        let reader_ring = ring.clone();
        let reader_settings = settings.clone();
        thread::Builder::new()
            .name("bava-capture".into())
            .spawn(move || capture_reader(reader_settings, reader_ring))
            .expect("failed to spawn capture thread");
        app.insert_resource(ring);

        app.add_systems(
            Update,
            (reconcile_capture_rate, rebuild_cava, feed_cava).chain(),
        )
            .add_systems(Last, stop_on_exit);
    }
}

/// How often [`capture_reader`] re-checks which sink is actively playing when
/// `follow_active_sink` is on. Long enough to be negligible overhead, short
/// enough that starting playback on another output retargets capture promptly.
const FOLLOW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Monitor source of the currently-active sink, or `None` when nothing is
/// playing / on non-Linux (where capture always loops back the default output).
fn active_sink_monitor() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        capture::pulse::active_monitor_source()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Pure audio reader: pulls small chunks from the platform capture backend and
/// appends them to the ring. No cavacore here — analysis happens on the render
/// thread.
///
/// When no explicit `source` is pinned and `follow_active_sink` is set, the
/// reader periodically re-resolves the sink that is actually playing and
/// reopens the capture there — so audio routed to a non-default output (an HDMI
/// display, say) is visualized without the user pinning a source by hand.
fn capture_reader(settings: CavaSettings, ring: AudioRing) {
    // Follow the active sink only when the user hasn't pinned an explicit source.
    let follow = settings.source.is_none() && settings.follow_active_sink;

    let open = |dev: &Option<String>| {
        capture::open(
            dev.as_deref(),
            settings.rate,
            settings.channels as u8,
            settings.frame_samples,
        )
    };

    // Initial device: a pinned source wins; otherwise the active sink if one is
    // playing, else `None` (the backend resolves the default sink's monitor).
    let mut current_device = settings.source.clone();
    if follow && current_device.is_none() {
        current_device = active_sink_monitor();
    }

    let mut capture = match open(&current_device) {
        Ok(c) => c,
        Err(e) => {
            error!("bava: audio capture unavailable, visualizer will be idle: {e}");
            return;
        }
    };

    info!(
        "bava: capturing {} ch @ {} Hz{}",
        capture.channels(),
        capture.rate(),
        current_device
            .as_deref()
            .map(|d| format!(" from {d}"))
            .unwrap_or_default(),
    );

    // Publish the negotiated rate/channels so the main thread can rebuild the
    // cavacore plan to match the *actual* delivery format (see
    // `reconcile_capture_rate`).
    ring.negotiated_rate.store(capture.rate(), Ordering::Relaxed);
    ring.negotiated_channels.store(capture.channels(), Ordering::Relaxed);

    let chunk = settings.frame_samples.max(1) * settings.channels.max(1);
    let mut buf = vec![0.0f64; chunk];
    let mut last_follow_check = std::time::Instant::now();

    while ring.running.load(Ordering::Relaxed) {
        if let Err(e) = capture.read(&mut buf) {
            // Back off before retrying: if the server died or the source was
            // removed, read() errors immediately every call, which would spin
            // a core at 100% and flood the log without this pause.
            error!("bava: {e}");
            thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }
        if let Ok(mut q) = ring.buf.lock() {
            q.extend(buf.iter().copied());
            while q.len() > ring.cap {
                q.pop_front();
            }
        }

        // Periodically re-follow the active sink. Only switch when a *different*
        // sink is actively playing; when nothing plays we keep the current source
        // so pausing doesn't yank capture away from what you were just hearing.
        if follow && last_follow_check.elapsed() >= FOLLOW_INTERVAL {
            last_follow_check = std::time::Instant::now();
            if let Some(active) = active_sink_monitor()
                && current_device.as_deref() != Some(active.as_str())
            {
                let next = Some(active.clone());
                match open(&next) {
                    Ok(c) => {
                        capture = c;
                        ring.negotiated_rate.store(capture.rate(), Ordering::Relaxed);
                        ring.negotiated_channels.store(capture.channels(), Ordering::Relaxed);
                        current_device = next;
                        info!("bava: following active sink → {active}");
                    }
                    Err(e) => warn!("bava: could not switch to active sink {active}: {e}"),
                }
            }
        }
    }
}

/// Each rendered frame: accumulate newly captured audio and feed cavacore in
/// fixed-size chunks (steady `new_samples` → stable framerate/autosens),
/// processing every full chunk that has buffered, then publish the latest bars.
/// cava runs at a steady high rate (≈ rate·channels / chunk), so the bars the
/// render samples are always fresh and smooth.
fn feed_cava(
    ring: Res<AudioRing>,
    state: Option<NonSendMut<CavaState>>,
    mut cava: ResMut<Cava>,
    settings: Res<CavaSettings>,
    offline: Option<Res<AudioInjector>>,
    mut dbg: Local<FeedStats>,
    mut stall: Local<StallState>,
) {
    let Some(mut state) = state else {
        return; // cavacore failed to init; leave bars at zero
    };
    let state = &mut *state;
    // Stride comes from the *plan*, not live settings: the plan and capture
    // thread are pinned to the startup channel count, so an editor edit to
    // `CavaSettings.channels` must not change how we deinterleave (it would
    // desync the chunk from the plan and corrupt the analysis until restart).
    let chunk = (settings.frame_samples.max(1) * state.plan.channels().max(1)).max(1);

    // Accumulate whatever was captured since the last frame.
    if let Ok(mut q) = ring.buf.lock() {
        state.accum.extend(q.drain(..));
    }

    // Process every complete chunk; cavacore sees a constant sample count.
    let mut executed = 0u32;
    while state.accum.len() >= chunk {
        state.scratch.clear();
        state.scratch.extend(state.accum.drain(..chunk));
        state.plan.execute(&state.scratch);
        executed += 1;
        if settings.debug {
            dbg.max_in = dbg
                .max_in
                .max(state.scratch.iter().fold(0.0f64, |m, &s| m.max(s.abs())));
        }
    }

    // Stall safety net — live capture only. Offline (`AudioInjector` present),
    // audio arrives in *video* time while this timer measures *wall* time, so
    // on a slow render it would inject spurious silence between real chunks
    // (nondeterministic output); a recording can't stall anyway.
    //
    // Live: the platform backends pad an *idle* device with silence,
    // so a connected-but-quiet source keeps feeding zeros and the bars decay on
    // their own. But a hard failure — PulseAudio server death, a monitor source
    // that vanished, a capture thread backing off on repeated read errors —
    // delivers *nothing*, which would otherwise freeze the last bars on screen.
    // When no chunk has arrived for a short window, keep executing on silence so
    // autosens decays the bars to zero (matching cava's `reset_output_buffers`)
    // instead of holding a stale frame.
    if executed > 0 {
        stall.last_audio = Some(std::time::Instant::now());
    } else if offline.is_none() {
        let stalled = stall
            .last_audio
            .is_none_or(|t| t.elapsed() >= STALL_DECAY_AFTER);
        if stalled {
            state.scratch.clear();
            state.scratch.resize(chunk, 0.0);
            state.plan.execute(&state.scratch);
        }
    }

    // Publish the most recent analysis (unchanged if no chunk was ready).
    let bars = state.plan.last_output();
    cava.bars.clear();
    cava.bars.extend(bars.iter().map(|&v| v as f32));

    if settings.debug {
        let now = std::time::Instant::now();
        dbg.since.get_or_insert(now);
        dbg.frames += 1;
        dbg.executes += executed as u64;
        dbg.max_out = dbg.max_out.max(bars.iter().fold(0.0f64, |m, &b| m.max(b)));
        if dbg.frames >= 240 {
            let secs = now.duration_since(dbg.since.unwrap()).as_secs_f64().max(1e-6);
            info!(
                "bava: {} frames in {:.2}s | {:.0} cava executes/s | chunk={} | \
                 max input={:.3} | max bar={:.3}",
                dbg.frames,
                secs,
                dbg.executes as f64 / secs,
                chunk,
                dbg.max_in,
                dbg.max_out,
            );
            *dbg = FeedStats::default();
        }
    }
}

/// Rebuild the cavacore plan to match the rate the capture backend actually
/// negotiated, once it becomes known.
///
/// The plan is built up front at the *requested* [`CavaSettings::rate`], but
/// backends like PipeWire and WASAPI loopback can only deliver at the device's
/// native rate (commonly 48 kHz when 44.1 kHz was asked for). cavacore's
/// framerate-adaptive smoothing is tuned to `plan.rate / frame_samples`, while
/// executes actually happen at `delivered_rate / frame_samples` — so a mismatch
/// makes the bars decay/respond faster (or slower) by the rate ratio. We learn
/// the real rate from the capture thread and rebuild the plan to match, which
/// also corrects the FFT bin frequencies. This is a one-shot: once the plan's
/// rate equals the negotiated rate, it no-ops.
fn reconcile_capture_rate(
    ring: Res<AudioRing>,
    state: Option<NonSendMut<CavaState>>,
    mut settings: ResMut<CavaSettings>,
    mut cava: ResMut<Cava>,
) {
    let negotiated = ring.negotiated_rate.load(Ordering::Relaxed);
    if negotiated == 0 {
        return; // capture hasn't reported a rate yet
    }
    let Some(mut state) = state else {
        return; // cavacore never initialized
    };
    // cavacore only supports 1 or 2 channels; if the backend negotiated something
    // exotic (or hasn't reported yet), keep the plan's channel count — down-mixing
    // an N-channel monitor is out of scope for this path.
    let neg_channels = ring.negotiated_channels.load(Ordering::Relaxed);
    let plan_channels = state.plan.channels() as u32;
    let channels = if neg_channels == 1 || neg_channels == 2 {
        neg_channels as usize
    } else {
        plan_channels as usize
    };
    if state.plan.rate() == negotiated && plan_channels as usize == channels {
        return; // already matches (e.g. Pulse forced the requested format)
    }

    let requested = state.plan.rate();
    let requested_channels = plan_channels;
    // Clamp the high cutoff below the negotiated Nyquist (and keep it above the
    // low cutoff). A lower-than-requested negotiated rate simply cannot represent
    // the top of the requested band, and without this clamp the rebuild would
    // fail `CavaConfig` validation (`high_cutoff >= rate/2`) and strand us on the
    // stale requested-rate plan — the exact frequency/smoothing mismatch this
    // function exists to fix.
    let nyquist = negotiated / 2;
    let high_cutoff = settings
        .high_cutoff_freq
        .min(nyquist.saturating_sub(1))
        .max(settings.low_cutoff_freq.saturating_add(1));
    let cfg = CavaConfig {
        bars: settings.bars_per_channel as u32,
        rate: negotiated,
        channels: channels as u32,
        autosens: settings.autosens,
        noise_reduction: settings.noise_reduction,
        low_cutoff_freq: settings.low_cutoff_freq,
        high_cutoff_freq: high_cutoff,
    };
    match cfg.build() {
        Ok(plan) => {
            let bars = plan.bars();
            state.plan = plan;
            state.accum.clear();
            state.scratch.clear();
            cava.bars = vec![0.0; bars * channels];
            cava.bars_per_channel = bars;
            cava.channels = channels;
            // Keep settings in sync so a later editor "Apply"/config save uses
            // the real rate/channels rather than reintroducing the mismatch.
            settings.rate = negotiated;
            settings.channels = channels;
            info!(
                "bava: capture negotiated {negotiated} Hz / {channels} ch \
                 (requested {requested} Hz / {requested_channels} ch); \
                 rebuilt cavacore to match — smoothing/stride now correct"
            );
        }
        Err(e) => error!(
            "bava: failed to rebuild cavacore at negotiated {negotiated} Hz: {e}; \
             keeping {requested} Hz plan (smoothing may be off)"
        ),
    }
}

/// Rebuild the cavacore plan in place when a [`CavaRebuild`] is requested,
/// applying the DSP-relevant [`CavaSettings`] (bars, autosens, noise reduction,
/// cutoffs) live. Rate and channels stay pinned to the running capture thread,
/// so those edits only take effect on the next launch (or after re-saving).
fn rebuild_cava(
    mut request: ResMut<CavaRebuild>,
    state: Option<NonSendMut<CavaState>>,
    settings: Res<CavaSettings>,
    mut cava: ResMut<Cava>,
) {
    if !request.0 {
        return;
    }
    request.0 = false;

    let Some(mut state) = state else {
        return; // cavacore never initialized; nothing to rebuild
    };

    // Keep the capture thread's rate/channels; only the analysis params change.
    let channels = state.plan.channels();
    let cfg = CavaConfig {
        bars: settings.bars_per_channel as u32,
        rate: state.plan.rate(),
        channels: channels as u32,
        autosens: settings.autosens,
        noise_reduction: settings.noise_reduction,
        low_cutoff_freq: settings.low_cutoff_freq,
        high_cutoff_freq: settings.high_cutoff_freq,
    };
    match cfg.build() {
        Ok(plan) => {
            let bars = plan.bars();
            state.plan = plan;
            state.accum.clear();
            state.scratch.clear();
            // Resize the published bars to the new bar count.
            cava.bars = vec![0.0; bars * channels];
            cava.bars_per_channel = bars;
            cava.channels = channels;
            info!("bava: rebuilt cavacore — {bars} bars/ch");
        }
        Err(e) => error!("bava: cavacore rebuild failed: {e}; keeping previous plan"),
    }
}

/// How long the capture stream may deliver *no* samples before [`feed_cava`]
/// starts feeding silence to decay the bars to zero. Short enough that a dead
/// source visibly settles instead of freezing, long enough to ride out a normal
/// frame's worth of jitter between captures.
const STALL_DECAY_AFTER: std::time::Duration = std::time::Duration::from_millis(200);

/// Tracks the last time [`feed_cava`] saw real captured audio, so a stalled or
/// dead capture stream decays the bars instead of freezing them.
#[derive(Default)]
struct StallState {
    last_audio: Option<std::time::Instant>,
}

/// Rolling debug accumulator for [`feed_cava`].
#[derive(Default)]
struct FeedStats {
    frames: u64,
    executes: u64,
    max_in: f64,
    max_out: f64,
    /// Wall-clock start of the current window, so execute *rate* is per-second
    /// rather than per-window (the window is frame-counted, so its span varies
    /// with framerate — 240 frames is ~1 s at 240 fps but ~4 s at 60 fps).
    since: Option<std::time::Instant>,
}

/// Signal the capture thread to stop when the app is exiting.
fn stop_on_exit(mut exit: MessageReader<AppExit>, ring: Option<Res<AudioRing>>) {
    if exit.read().next().is_some() {
        if let Some(ring) = ring {
            ring.running.store(false, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_input_left_is_all_right_is_empty() {
        let cava = Cava {
            bars: vec![0.1, 0.2, 0.3, 0.4],
            bars_per_channel: 4,
            channels: 1,
        };
        assert_eq!(cava.left(), &[0.1, 0.2, 0.3, 0.4]);
        assert!(cava.right().is_empty());
        assert_eq!(cava.mono(), vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn stereo_splits_channels_and_averages_for_mono() {
        // 3 bars/channel: [L0,L1,L2, R0,R1,R2].
        let cava = Cava {
            bars: vec![1.0, 0.0, 0.5, 0.0, 1.0, 0.5],
            bars_per_channel: 3,
            channels: 2,
        };
        assert_eq!(cava.left(), &[1.0, 0.0, 0.5]);
        assert_eq!(cava.right(), &[0.0, 1.0, 0.5]);
        // mono is the per-bar average of the two channels.
        assert_eq!(cava.mono(), vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn accessors_tolerate_short_or_empty_buffers() {
        // Buffer shorter than declared (e.g. a frame mid-resize) must not panic.
        let cava = Cava {
            bars: vec![0.7],
            bars_per_channel: 4,
            channels: 2,
        };
        assert_eq!(cava.left(), &[0.7]);
        assert!(cava.right().is_empty());
        // mono fills missing bars with 0.0.
        assert_eq!(cava.mono(), vec![0.7, 0.0, 0.0, 0.0]);

        let empty = Cava::default();
        assert!(empty.mono().is_empty());
    }
}
