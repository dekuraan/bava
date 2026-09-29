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
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use bevy::prelude::*;
// `std::time::Instant` panics on wasm32-unknown-unknown (no clock). Bevy's
// re-export is `web_time::Instant` there — `performance.now()` under the hood —
// and plain `std::time::Instant` everywhere else.
use bevy::platform::time::Instant;
use cavacore_rs::{CavaConfig, CavaPlan};

/// Tunables for the cavacore pipeline. Insert your own before adding
/// [`CavaPlugin`] to override the defaults.
#[derive(Resource, Clone, Debug, PartialEq)]
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

impl CavaSettings {
    pub fn plan_config(&self) -> CavaConfig {
        CavaConfig {
            bars: self.bars_per_channel as u32,
            rate: self.rate,
            channels: self.channels as u32,
            autosens: self.autosens,
            noise_reduction: self.noise_reduction,
            low_cutoff_freq: self.low_cutoff_freq,
            high_cutoff_freq: self.high_cutoff_freq,
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

/// Outcome of the most recent [`rebuild_cava`] run, for the settings editor's
/// status line — without it a failed rebuild is only visible on the console
/// and the editor stays stuck on "Rebuilding cavacore plan…" as if it worked.
/// The editor `take()`s the message once displayed.
#[derive(Resource, Default)]
pub struct CavaRebuildStatus(pub Option<String>);

/// Current native capture state, shared with the settings editor.
#[derive(Resource, Clone, Default)]
pub struct CaptureStatus(Arc<Mutex<String>>);

impl CaptureStatus {
    pub fn message(&self) -> String {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn set(&self, message: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }
}

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

/// Signal levels measured on the raw samples (before cavacore's smoothing),
/// refreshed by `feed_cava` whenever new audio arrives.
///
/// cavacore's bars are built to *look* good: integrated, gravity-smoothed and
/// autosens-scaled into `0..1`, so on a loud mix the low bars sit pinned near
/// 1.0 between kicks and an onset barely shows. Beat detection needs the
/// unsmoothed signal, so this meters the incoming audio directly.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioLevels {
    /// Running mean square of the low band (≲ 150 Hz, the kick and bass) as of
    /// the newest sample, averaged over the last ~25 ms of audio.
    pub low: f32,
    /// The highest `low` reached over the samples this frame brought, and
    /// its mean over them. The beat detector fires on the peak and follows
    /// the mean, so a kick's peak counts wherever the frame boundaries fell
    /// and the bass under it reads at its average rather than at whatever
    /// phase of its ripple a boundary caught: with coarse frames (30 fps, a
    /// 1024-sample PipeWire quantum) the end-of-block value alone lost most of
    /// a moderate kick's 3 dB.
    pub low_peak: f32,
    pub low_mean: f32,
    /// Running mean square of the full-band mono signal, over the same window.
    pub full: f32,
    /// True when this frame brought new samples (the values are fresh).
    pub fresh: bool,
}

/// Corner frequency of the low-band meter, Hz.
const LOW_BAND_HZ: f64 = 150.0;

/// Time constant of the meter's mean-square envelope, seconds.
///
/// The published level is this running average as of the newest sample, so it
/// depends only on the audio, never on how many samples a frame happened to
/// drain. A per-frame *block* average can't do that: once a frame is shorter
/// than a period of the bass note (≈ 7 ms at 144 Hz), a steady 40 Hz sine's
/// block mean square swings with its phase, and the beat detector's valley
/// follower reads every swing as an onset. 25 ms smooths a 20 Hz tone's ripple
/// to under 1.5 dB (half the trigger margin) and still follows a kick's attack.
const LEVEL_TAU: f64 = 0.025;

/// Two cascaded one-pole low-pass filters (12 dB/oct) metering the low band,
/// then per-sample mean-square envelopes of it and of the full-band signal.
#[derive(Default)]
struct LowBandMeter {
    a: f64,
    b: f64,
    /// Running mean square of the low band (`b²`).
    low: f64,
    /// Running mean square of the full-band mono signal.
    full: f64,
}

impl LowBandMeter {
    /// Meter `samples` (interleaved, `channels` wide) at `rate` Hz: the
    /// envelopes after the last frame and the low band's range over the
    /// block, or `None` for no frames.
    fn measure(
        &mut self,
        samples: impl Iterator<Item = f64>,
        channels: usize,
        rate: u32,
    ) -> Option<AudioLevels> {
        let channels = channels.max(1);
        let rate = rate.max(1) as f64;
        let k = 1.0 - (-std::f64::consts::TAU * LOW_BAND_HZ / rate).exp();
        let env = 1.0 - (-1.0 / (LEVEL_TAU * rate)).exp();
        let mut frames = 0usize;
        let (mut peak, mut sum) = (f64::MIN, 0.0f64);
        let (mut acc, mut n) = (0.0f64, 0usize);
        for s in samples {
            acc += s;
            n += 1;
            if n < channels {
                continue;
            }
            let mono = acc / channels as f64;
            acc = 0.0;
            n = 0;
            self.a += (mono - self.a) * k;
            self.b += (self.a - self.b) * k;
            self.low += (self.b * self.b - self.low) * env;
            self.full += (mono * mono - self.full) * env;
            peak = peak.max(self.low);
            sum += self.low;
            frames += 1;
        }
        // Digital silence would otherwise leave the filters decaying into
        // subnormals, where they stay (`x * k` rounds to zero) and every later
        // sample pays for subnormal arithmetic. Nothing decays from 1e-30 to
        // the subnormal range within one block.
        for v in [&mut self.a, &mut self.b, &mut self.low, &mut self.full] {
            if v.abs() < 1e-30 {
                *v = 0.0;
            }
        }
        (frames > 0).then(|| AudioLevels {
            low: self.low as f32,
            low_peak: peak as f32,
            low_mean: (sum / frames as f64) as f32,
            full: self.full as f32,
            fresh: true,
        })
    }
}

/// The system that publishes fresh bars into [`Cava`] (`feed_cava`), live or
/// offline. Anything deriving per-frame data from the bars — the audio
/// features the effects run on — orders itself `.after()` this so it reads
/// this frame's analysis rather than last frame's.
#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CavaAnalysisSet;

/// Pushes decoded samples straight into the audio ring, for offline rendering
/// (`--input`). Inserted only by [`CavaPlugin`] in offline mode, where there is
/// no capture thread; the record driver pushes each video frame's worth of
/// samples before [`feed_cava`] drains them.
///
/// Offline rendering is native-only, so nothing constructs this on the web —
/// but [`feed_cava`] still looks the resource up on every target, so the type
/// itself stays compiled there and only its unused innards are excused.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Resource, Clone)]
pub struct AudioInjector {
    ring: AudioRing,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
            .init_resource::<AudioLevels>()
            .init_resource::<CavaRebuild>()
            .init_resource::<CavaRebuildStatus>();
        let settings = app.world().resource::<CavaSettings>().clone();

        // Size the Cava resource up front.
        {
            let mut cava = app.world_mut().resource_mut::<Cava>();
            cava.bars = vec![0.0; settings.bars_per_channel * settings.channels];
            cava.bars_per_channel = settings.bars_per_channel;
            cava.channels = settings.channels;
        }

        // Build the cavacore plan on the main thread and keep it there.
        let cfg = settings.plan_config();
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
                    (rebuild_cava, feed_cava.in_set(CavaAnalysisSet))
                        .chain()
                        .in_set(OfflineCavaSet),
                );
            return;
        }

        // Spawn the audio reader thread feeding the ring.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let reader_ring = ring.clone();
            let reader_settings = settings.clone();
            let status = CaptureStatus::default();
            app.insert_resource(status.clone());
            thread::Builder::new()
                .name("bava-capture".into())
                .spawn(move || capture_reader(reader_settings, reader_ring, status))
                .expect("failed to spawn capture thread");
        }
        app.insert_resource(ring);

        // On the web the producer is the page, not a thread: `pump_web_audio`
        // stands in for the capture thread, moving what JS pushed into the same
        // ring before the rest of the chain reads it.
        #[cfg(target_arch = "wasm32")]
        app.add_systems(Update, pump_web_audio.before(reconcile_capture_rate));

        app.add_systems(
            Update,
            (
                reconcile_capture_rate,
                rebuild_cava,
                feed_cava.in_set(CavaAnalysisSet),
            )
                .chain(),
        )
        .add_systems(Last, stop_on_exit);
    }
}

/// The web build's stand-in for the capture thread: move whatever the page's
/// `AudioWorklet` pushed since last frame into the ring, and publish the
/// `AudioContext`'s format so [`reconcile_capture_rate`] can rebuild the plan
/// for it (the browser picks the rate — usually 48 kHz — and never honours a
/// request for another one).
#[cfg(target_arch = "wasm32")]
fn pump_web_audio(ring: Res<AudioRing>) {
    let mut incoming = VecDeque::new();
    let (rate, channels) = capture::web::take(&mut incoming);
    if rate > 0 {
        ring.negotiated_rate.store(rate, Ordering::Relaxed);
        ring.negotiated_channels.store(channels, Ordering::Relaxed);
    }
    if incoming.is_empty() {
        return;
    }
    if let Ok(mut q) = ring.buf.lock() {
        q.extend(incoming.drain(..));
        // Same bound as the native reader: a backgrounded tab stops rendering
        // while audio keeps arriving, and we want to resume live rather than
        // work through the backlog.
        while q.len() > ring.cap {
            q.pop_front();
        }
    }
}

/// How often [`capture_reader`] re-checks which sink is actively playing when
/// `follow_active_sink` is on. Long enough to be negligible overhead, short
/// enough that starting playback on another output retargets capture promptly.
#[cfg(not(target_arch = "wasm32"))]
const FOLLOW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Monitor source of the currently-active sink, or `None` when nothing is
/// playing / on non-Linux (where capture always loops back the default output).
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
fn capture_reader(mut settings: CavaSettings, ring: AudioRing, status: CaptureStatus) {
    // A single read must fit the ring even when the DSP chunk setting is huge.
    settings.frame_samples = settings
        .frame_samples
        .clamp(1, ring.cap / settings.channels);
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

    let reopen = |device: &mut Option<String>| {
        open_with_retry(
            &ring.running,
            &status,
            || {
                if follow {
                    *device = active_sink_monitor();
                }
                open(device)
            },
            || thread::sleep(std::time::Duration::from_millis(100)),
        )
    };
    let Some(mut capture) = reopen(&mut current_device) else {
        return;
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
    ring.negotiated_rate
        .store(capture.rate(), Ordering::Relaxed);
    ring.negotiated_channels
        .store(capture.channels(), Ordering::Relaxed);

    let chunk = settings.frame_samples.max(1) * settings.channels.max(1);
    let mut buf = vec![0.0f64; chunk];
    let mut last_follow_check = Instant::now();
    // A backend whose read() fails on every call (audio server restarted,
    // source removed) never heals on its own — the handle is dead. After ~1 s
    // of consecutive failures, reopen the backend instead of retrying the
    // corpse forever.
    const REOPEN_AFTER_FAILURES: u32 = 10;
    let mut consecutive_failures = 0u32;

    while ring.running.load(Ordering::Relaxed) {
        let read = capture.read(&mut buf);
        if let Err(e) = &read {
            // Back off before retrying: if the server died or the source was
            // removed, read() errors immediately every call, which would spin
            // a core at 100% and flood the log without this pause.
            status.set(format!("Audio interrupted: {e}. Reconnecting…"));
            error!("bava: {e}");
            thread::sleep(std::time::Duration::from_millis(100));
            consecutive_failures += 1;
            if consecutive_failures >= REOPEN_AFTER_FAILURES {
                consecutive_failures = 0;
                let Some(c) = reopen(&mut current_device) else {
                    return;
                };
                capture = c;
                if let Ok(mut q) = ring.buf.lock() {
                    q.clear();
                }
                ring.negotiated_rate
                    .store(capture.rate(), Ordering::Relaxed);
                ring.negotiated_channels
                    .store(capture.channels(), Ordering::Relaxed);
                info!("bava: audio capture reopened after repeated read failures");
            }
            continue;
        }
        if consecutive_failures > 0 {
            status.set("Capturing audio".into());
        }
        consecutive_failures = 0;
        if let Ok(mut q) = ring.buf.lock() {
            q.extend(buf[..read.unwrap()].iter().copied());
            while q.len() > ring.cap {
                q.pop_front();
            }
        }

        // Periodically re-follow the active sink. Only switch when a *different*
        // sink is actively playing; when nothing plays we keep the current source
        // so pausing doesn't yank capture away from what you were just hearing.
        if follow && last_follow_check.elapsed() >= FOLLOW_INTERVAL {
            last_follow_check = Instant::now();
            if let Some(active) = active_sink_monitor()
                && current_device.as_deref() != Some(active.as_str())
            {
                let next = Some(active.clone());
                match open(&next) {
                    Ok(c) => {
                        capture = c;
                        ring.negotiated_rate
                            .store(capture.rate(), Ordering::Relaxed);
                        ring.negotiated_channels
                            .store(capture.channels(), Ordering::Relaxed);
                        current_device = next;
                        info!("bava: following active sink → {active}");
                    }
                    Err(e) => warn!("bava: could not switch to active sink {active}: {e}"),
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn open_with_retry<T>(
    running: &AtomicBool,
    status: &CaptureStatus,
    mut open: impl FnMut() -> Result<T, capture::CaptureError>,
    mut wait: impl FnMut(),
) -> Option<T> {
    while running.load(Ordering::Relaxed) {
        match open() {
            Ok(capture) => {
                status.set("Capturing audio".into());
                return Some(capture);
            }
            Err(e) => {
                status.set(format!("Audio unavailable: {e}. Retrying…"));
                warn!("bava: {e}; retrying capture in one second");
            }
        }
        for _ in 0..10 {
            if !running.load(Ordering::Relaxed) {
                return None;
            }
            wait();
        }
    }
    None
}

/// Each rendered frame: accumulate newly captured audio and feed cavacore in
/// fixed-size chunks (steady `new_samples` → stable framerate/autosens),
/// processing every full chunk that has buffered, then publish the latest bars.
/// cava runs at a steady high rate (≈ rate·channels / chunk), so the bars the
/// render samples are always fresh and smooth.
#[allow(clippy::too_many_arguments)]
fn feed_cava(
    ring: Res<AudioRing>,
    state: Option<NonSendMut<CavaState>>,
    mut cava: ResMut<Cava>,
    settings: Res<CavaSettings>,
    offline: Option<Res<AudioInjector>>,
    mut dbg: Local<FeedStats>,
    mut stall: Local<StallState>,
    mut levels: ResMut<AudioLevels>,
    mut meter: Local<LowBandMeter>,
) {
    let Some(mut state) = state else {
        return; // cavacore failed to init; leave bars at zero
    };
    let state = &mut *state;
    // Stride comes from the *plan*, not live settings: the plan and capture
    // thread are pinned to the startup channel count, so an editor edit to
    // `CavaSettings.channels` must not change how we deinterleave (it would
    // desync the chunk from the plan and corrupt the analysis until restart).
    let chunk = settings
        .frame_samples
        .max(1)
        .min(state.plan.max_input_samples() / state.plan.channels())
        * state.plan.channels();

    // Accumulate whatever was captured since the last frame, metering the new
    // samples on the way in (see [`AudioLevels`]).
    let before = state.accum.len();
    if let Ok(mut q) = ring.buf.lock() {
        state.accum.extend(q.drain(..));
    }
    let received = state.accum.len() > before;
    let measured = meter.measure(
        state.accum.range(before..).copied(),
        state.plan.channels(),
        state.plan.rate(),
    );
    let next = match measured {
        Some(levels) => levels,
        None => AudioLevels {
            fresh: false,
            ..*levels
        },
    };
    levels.set_if_neq(next);

    // Process every complete chunk; cavacore sees a constant sample count.
    let mut executed = 0u32;
    {
        crate::profile_scope!("cava_execute");
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
    }

    // Offline samples use video time, so wall-clock decay must stay disabled.
    if offline.is_none() {
        let silent_chunks = stall.silence_chunks(
            Instant::now(),
            received,
            state.plan.rate(),
            chunk / state.plan.channels(),
        );
        if silent_chunks > 0 {
            state.accum.clear();
            state.scratch.clear();
            state.scratch.resize(chunk, 0.0);
            for _ in 0..silent_chunks {
                state.plan.execute(&state.scratch);
            }
            if let Some(silent_levels) = meter.measure(
                std::iter::repeat_n(0.0, silent_chunks * chunk),
                state.plan.channels(),
                state.plan.rate(),
            ) {
                levels.set_if_neq(silent_levels);
            }
            executed += silent_chunks as u32;
        }
    }

    // Publish the most recent analysis (unchanged if no chunk was ready).
    let bars = state.plan.last_output();
    cava.bars.clear();
    cava.bars.extend(bars.iter().map(|&v| v as f32));

    if settings.debug {
        let now = Instant::now();
        dbg.since.get_or_insert(now);
        dbg.frames += 1;
        dbg.executes += executed as u64;
        dbg.max_out = dbg.max_out.max(bars.iter().fold(0.0f64, |m, &b| m.max(b)));
        if dbg.frames >= 240 {
            let secs = now
                .duration_since(dbg.since.unwrap())
                .as_secs_f64()
                .max(1e-6);
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
    // fail `CavaConfig` validation (`high_cutoff > rate/2`) and strand us on the
    // stale requested-rate plan — the exact frequency/smoothing mismatch this
    // function exists to fix.
    let high_cutoff = clamp_high_cutoff(
        settings.high_cutoff_freq,
        settings.low_cutoff_freq,
        negotiated,
    );
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
            // the real rate/channels — and the clamped cutoff — rather than
            // reintroducing the mismatch. Without the cutoff write-back, every
            // subsequent Apply would fail validation against the (lower)
            // negotiated Nyquist and silently keep the previous plan.
            settings.rate = negotiated;
            settings.channels = channels;
            settings.high_cutoff_freq = high_cutoff;
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

/// `high` clamped to at most `rate`'s Nyquist and above `low` — the band
/// `CavaConfig` validation accepts (unless `low` itself sits at Nyquist, which
/// no high cutoff can fix). The plan runs at the rate capture *negotiated*,
/// so a cutoff that was valid at the requested rate, or one restored from a
/// saved config or scene snapshot, may not be valid at the rate in use.
pub(crate) fn clamp_high_cutoff(high: u32, low: u32, rate: u32) -> u32 {
    high.min(rate / 2).max(low.saturating_add(1))
}

/// Rebuild the cavacore plan in place when a [`CavaRebuild`] is requested,
/// applying the DSP-relevant [`CavaSettings`] (bars, autosens, noise reduction,
/// cutoffs) live. Rate and channels stay pinned to the running capture thread,
/// so those edits only take effect on the next launch (or after re-saving).
fn rebuild_cava(
    mut request: ResMut<CavaRebuild>,
    mut status: ResMut<CavaRebuildStatus>,
    state: Option<NonSendMut<CavaState>>,
    mut settings: ResMut<CavaSettings>,
    mut cava: ResMut<Cava>,
) {
    if !request.0 {
        return;
    }
    request.0 = false;

    let Some(mut state) = state else {
        status.0 = Some("Rebuild skipped: audio capture never initialized".into());
        return; // cavacore never initialized; nothing to rebuild
    };

    // Keep the capture thread's rate/channels; only the analysis params change.
    // The high cutoff is clamped to that rate's Nyquist, as in
    // `reconcile_capture_rate`: an edited or restored cutoff above it would
    // otherwise fail validation and strand the previous plan (and every later
    // Apply with it) while `CavaSettings` claims the new values.
    let channels = state.plan.channels();
    let rate = state.plan.rate();
    let high_cutoff = clamp_high_cutoff(settings.high_cutoff_freq, settings.low_cutoff_freq, rate);
    let cfg = CavaConfig {
        bars: settings.bars_per_channel as u32,
        rate,
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
            // Resize the published bars to the new bar count.
            cava.bars = vec![0.0; bars * channels];
            cava.bars_per_channel = bars;
            cava.channels = channels;
            let mut message = format!("Rebuilt cavacore — {bars} bars/ch");
            // Write the clamp back so the editor and a later save show the
            // cutoff actually in use. Only when it bit: an unconditional write
            // would mark `CavaSettings` changed on every rebuild.
            if settings.high_cutoff_freq != high_cutoff {
                settings.high_cutoff_freq = high_cutoff;
                message.push_str(&format!(
                    " (high cutoff clamped to {high_cutoff} Hz for {rate} Hz audio)"
                ));
            }
            info!("bava: {message}");
            status.0 = Some(message);
        }
        Err(e) => {
            error!("bava: cavacore rebuild failed: {e}; keeping previous plan");
            status.0 = Some(format!("Rebuild failed: {e} — kept previous plan"));
        }
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
    last_audio: Option<Instant>,
    last_step: Option<Instant>,
    remainder: f64,
}

impl StallState {
    fn silence_chunks(&mut self, now: Instant, received: bool, rate: u32, frames: usize) -> usize {
        if received || self.last_audio.is_none() {
            self.last_audio = Some(now);
            self.last_step = Some(now);
            self.remainder = 0.0;
            return 0;
        }
        if now.duration_since(self.last_audio.unwrap()) < STALL_DECAY_AFTER {
            return 0;
        }
        // Bound catch-up after a suspended render loop while preserving fractional
        // chunks across ordinary frames. Decay follows sample time, not render FPS.
        let elapsed = now.duration_since(self.last_step.replace(now).unwrap());
        self.remainder += elapsed.as_secs_f64().min(0.25) * f64::from(rate);
        let chunks = (self.remainder / frames as f64) as usize;
        self.remainder -= (chunks * frames) as f64;
        chunks
    }
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
    since: Option<Instant>,
}

/// Signal the capture thread to stop when the app is exiting.
fn stop_on_exit(mut exit: MessageReader<AppExit>, ring: Option<Res<AudioRing>>) {
    if exit.read().next().is_some()
        && let Some(ring) = ring
    {
        ring.running.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_silence_tracks_sample_time_at_different_render_rates() {
        for fps in [30, 60, 144, 240] {
            let mut stall = StallState::default();
            let start = Instant::now();
            assert_eq!(stall.silence_chunks(start, true, 48_000, 128), 0);
            let mut chunks = 0;
            for frame in 1..=fps {
                let now = start + std::time::Duration::from_secs_f64(frame as f64 / fps as f64);
                chunks += stall.silence_chunks(now, false, 48_000, 128);
            }
            assert!((374..=375).contains(&chunks), "{fps} fps: {chunks}");
        }
    }

    #[test]
    fn packet_jitter_and_partial_audio_do_not_inject_silence() {
        let mut stall = StallState::default();
        let start = Instant::now();
        for millis in 0..1_000 {
            let now = start + std::time::Duration::from_millis(millis);
            // Even a partial real chunk resets the idle deadline.
            assert_eq!(stall.silence_chunks(now, millis % 180 == 0, 48_000, 128), 0);
        }
    }

    #[test]
    fn resumed_audio_discards_silence_debt_and_suspend_catchup_is_bounded() {
        let mut stall = StallState::default();
        let start = Instant::now();
        stall.silence_chunks(start, true, 48_000, 128);
        let later = start + std::time::Duration::from_secs(3_600);
        assert_eq!(stall.silence_chunks(later, false, 48_000, 128), 93);
        assert_eq!(stall.silence_chunks(later, true, 48_000, 128), 0);
        assert_eq!(
            stall.silence_chunks(later + STALL_DECAY_AFTER / 2, false, 48_000, 128),
            0
        );
    }

    #[test]
    fn warmed_spectrum_decays_after_one_second_without_capture() {
        let mut plan = CavaSettings::default().plan_config().build().unwrap();
        let frames = 128;
        let rate = plan.rate();
        let channels = plan.channels();
        for chunk in 0..rate as usize / frames {
            let samples: Vec<_> = (chunk * frames..(chunk + 1) * frames)
                .flat_map(|i| {
                    let sample = (std::f64::consts::TAU * 100.0 * i as f64 / rate as f64).sin();
                    std::iter::repeat_n(sample, channels)
                })
                .collect();
            plan.execute(&samples);
        }
        assert!(plan.last_output().iter().copied().fold(0.0, f64::max) > 0.5);
        let mut stall = StallState::default();
        let start = Instant::now();
        stall.silence_chunks(start, true, rate, frames);
        let zeros = vec![0.0; frames * channels];
        for frame in 1..=60 {
            let now = start + std::time::Duration::from_secs_f64(frame as f64 / 60.0);
            for _ in 0..stall.silence_chunks(now, false, rate, frames) {
                plan.execute(&zeros);
            }
        }
        assert!(plan.last_output().iter().all(|&level| level < 0.01));
    }

    #[test]
    fn low_band_meter_passes_bass_and_rejects_treble() {
        let rate = 48_000u32;
        let tone = |hz: f64| {
            (0..rate as usize / 2)
                .flat_map(move |i| {
                    let s = (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() * 0.5;
                    [s, s] // stereo, identical channels
                })
                .collect::<Vec<f64>>()
        };
        let AudioLevels { low, full, .. } = LowBandMeter::default()
            .measure(tone(50.0).into_iter(), 2, rate)
            .unwrap();
        assert!(low > full * 0.6, "50 Hz passes: {low} of {full}");
        let AudioLevels { low, full, .. } = LowBandMeter::default()
            .measure(tone(5_000.0).into_iter(), 2, rate)
            .unwrap();
        assert!(low < full * 0.01, "5 kHz is rejected: {low} of {full}");
        assert!(
            LowBandMeter::default()
                .measure(std::iter::empty(), 2, rate)
                .is_none()
        );
    }

    const RATE: u32 = 48_000;

    /// `secs` seconds of mono audio at [`RATE`], sample `i` = `f(i / RATE)`.
    fn signal(secs: f64, f: impl Fn(f64) -> f64) -> Vec<f64> {
        (0..(secs * RATE as f64) as usize)
            .map(|i| f(i as f64 / RATE as f64))
            .collect()
    }

    /// Meter `signal` as a `fps` render loop drains it into the beat
    /// detector, the audio arriving in blocks of `quantum` samples (`1`: as
    /// fast as it plays); the beats after the first second.
    fn beats_in(signal: &[f64], fps: u64, quantum: usize) -> u64 {
        let mut meter = LowBandMeter::default();
        let mut features = crate::vis::features::AudioFeatures::default();
        let (mut start, mut warm) = (0, 0);
        for frame in 1u64.. {
            let due = (frame * RATE as u64 / fps) as usize;
            if due > signal.len() {
                break;
            }
            let end = (due / quantum * quantum).max(start);
            let levels = meter
                .measure(signal[start..end].iter().copied(), 1, RATE)
                .unwrap_or_default();
            start = end;
            features.update(&[0.5; 8], levels, 1.0 / fps as f32);
            if frame == fps {
                warm = features.beats;
            }
        }
        features.beats - warm
    }

    fn beats_at(signal: &[f64], fps: u64) -> u64 {
        beats_in(signal, fps, 1)
    }

    /// Render rates and capture quanta audio really arrives at.
    const DELIVERIES: [(u64, usize); 6] = [
        (30, 1),
        (60, 1),
        (144, 1),
        (240, 1),
        (60, 1024),
        (144, 1024),
    ];

    #[test]
    fn low_band_level_does_not_depend_on_block_size() {
        // The published level is a function of the samples alone: draining
        // the same audio in 200- or 800-sample frames reads identically at
        // every shared boundary.
        let tone = signal(0.5, |t| 0.5 * (std::f64::consts::TAU * 40.0 * t).sin());
        let (mut whole, mut split) = (LowBandMeter::default(), LowBandMeter::default());
        for block in tone.chunks(800) {
            let a = whole.measure(block.iter().copied(), 1, RATE).unwrap();
            let mut parts = Vec::new();
            for part in block.chunks(200) {
                parts.push(split.measure(part.iter().copied(), 1, RATE).unwrap());
            }
            let b = parts.last().unwrap();
            assert_eq!((a.low, a.full), (b.low, b.full));
            // The block's peak is its parts' highest; its mean their average.
            let peak = parts.iter().map(|p| p.low_peak).fold(f32::MIN, f32::max);
            let mean = parts.iter().map(|p| p.low_mean).sum::<f32>() / parts.len() as f32;
            assert_eq!(a.low_peak, peak);
            assert!(
                (a.low_mean - mean).abs() <= mean * 1e-5,
                "{} vs {mean}",
                a.low_mean
            );
        }
    }

    #[test]
    fn steady_bass_never_beats_at_any_frame_rate() {
        // A frame shorter than a period of the note (≈ 4 ms at 240 fps) used
        // to catch the sine's power mid-swing; the valley follower read each
        // swing as a +3 dB onset and fired ~6.7 times a second.
        for hz in [20.0, 30.0, 35.0, 40.0, 45.0, 60.0, 80.0] {
            let tone = signal(6.0, |t| 0.5 * (std::f64::consts::TAU * hz * t).sin());
            for (fps, quantum) in DELIVERIES {
                assert_eq!(
                    beats_in(&tone, fps, quantum),
                    0,
                    "{hz} Hz tone at {fps} fps, {quantum}-sample quanta"
                );
            }
        }
    }

    #[test]
    fn moderate_kicks_beat_once_each_however_the_audio_arrives() {
        // Kicks only ~4 dB over the bass in the low band: judged on the last
        // sample of each drained block, coarse frames (30 fps, 1024-sample
        // quanta) caught a fraction of the rise and missed most of them.
        let tau = std::f64::consts::TAU;
        // A 150 → 60 Hz swept kick over a 30 Hz bass, 128 BPM.
        let swept = |t: f64| {
            let s = t % (60.0 / 128.0);
            let phase = tau * (60.0 * s + 90.0 * 0.03 * (1.0 - (-s / 0.03).exp()));
            0.3 * (tau * 30.0 * t).sin() + 0.6 * (-s / 0.1).exp() * phase.sin()
        };
        // A short 60 Hz kick over a 45 Hz bass, 150 BPM.
        let short = |t: f64| {
            let s = t % 0.4;
            0.3 * (tau * 45.0 * t).sin() + 0.5 * (-s / 0.06).exp() * (tau * 60.0 * s).sin()
        };
        // The 150 BPM track below, with a kick about half as loud.
        let soft = |t: f64| {
            let s = t % 0.4;
            0.3 * (tau * 40.0 * t).sin() + 0.5 * (-s / 0.1).exp() * (tau * 55.0 * s).sin()
        };
        let tracks = [
            ("swept", 128.0, signal(7.0, swept)),
            ("short", 150.0, signal(7.0, short)),
            ("soft", 150.0, signal(7.0, soft)),
        ];
        for (name, bpm, track) in tracks {
            let expected = 6.0 * bpm / 60.0;
            for (fps, quantum) in DELIVERIES {
                let beats = beats_in(&track, fps, quantum);
                assert!(
                    (beats as f64 - expected).abs() <= 1.0,
                    "{name} at {fps} fps, {quantum}-sample quanta: {beats} beats, expected {expected}"
                );
            }
        }
    }

    #[test]
    fn kicks_over_a_sustained_bass_beat_once_each_at_any_frame_rate() {
        // (bpm, bass Hz, kick Hz): a decaying kick every beat over a steady
        // bass note, like an 808 under a four-on-the-floor kick.
        for (bpm, bass, kick) in [(120.0, 45.0, 60.0), (150.0, 40.0, 55.0)] {
            let period = 60.0 / bpm;
            let track = signal(7.0, |t| {
                let since = t % period;
                0.3 * (std::f64::consts::TAU * bass * t).sin()
                    + 0.9 * (-since / 0.1).exp() * (std::f64::consts::TAU * kick * since).sin()
            });
            let expected = 6.0 * bpm / 60.0;
            for fps in [60, 144, 240] {
                let beats = beats_at(&track, fps);
                assert!(
                    (beats as f64 - expected).abs() <= 1.0,
                    "{bpm} BPM at {fps} fps: {beats} beats, expected {expected}"
                );
            }
        }
    }

    #[test]
    fn high_cutoff_clamps_below_nyquist_and_above_low() {
        assert_eq!(clamp_high_cutoff(10_000, 50, 16_000), 8_000);
        assert_eq!(clamp_high_cutoff(10_000, 50, 44_100), 10_000);
        assert_eq!(clamp_high_cutoff(100, 200, 44_100), 201);
    }

    #[test]
    fn rebuild_clamps_a_cutoff_above_nyquist_instead_of_keeping_the_old_plan() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(CavaSettings {
            channels: 1,
            rate: 16_000,
            high_cutoff_freq: 7_000,
            ..default()
        });
        app.add_plugins(CavaPlugin { offline: true });
        // A restored snapshot or an edit asks for a band this rate can't hold.
        {
            let mut settings = app.world_mut().resource_mut::<CavaSettings>();
            settings.high_cutoff_freq = 10_000;
            settings.bars_per_channel = 12;
        }
        app.world_mut().resource_mut::<CavaRebuild>().0 = true;
        app.update();
        let status = app.world_mut().resource_mut::<CavaRebuildStatus>().0.take();
        assert!(
            status.as_deref().is_some_and(|s| s.starts_with("Rebuilt")),
            "{status:?}"
        );
        assert_eq!(app.world().resource::<Cava>().bars_per_channel, 12);
        assert_eq!(
            app.world().resource::<CavaSettings>().high_cutoff_freq,
            8_000
        );
    }

    #[test]
    fn oversized_chunks_preserve_signal_after_the_first_fft_buffer() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(CavaSettings {
            channels: 1,
            frame_samples: 65_536,
            ..default()
        });
        app.add_plugins(CavaPlugin { offline: true });
        let capacity = app.world().non_send::<CavaState>().plan.max_input_samples();
        let mut samples = vec![0.0; 65_536];
        for (i, sample) in samples.iter_mut().enumerate().skip(capacity) {
            *sample = (i as f64 * std::f64::consts::TAU * 440.0 / 44_100.0).sin() * 0.8;
        }
        app.world().resource::<AudioInjector>().push(&samples);
        app.update();
        assert!(app.world().resource::<Cava>().bars.iter().any(|&v| v > 0.0));
        assert!(app.world().non_send::<CavaState>().accum.is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn capture_open_recovers_after_startup_failures() {
        let running = AtomicBool::new(true);
        let status = CaptureStatus::default();
        let mut attempts = 0;
        let mut waits = 0;
        let result = open_with_retry(
            &running,
            &status,
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(capture::CaptureError::Init("server offline".into()))
                } else {
                    Ok(42)
                }
            },
            || waits += 1,
        );
        assert_eq!(result, Some(42));
        assert_eq!(waits, 20);
        assert_eq!(status.message(), "Capturing audio");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn capture_retry_stops_during_backoff() {
        let running = AtomicBool::new(true);
        let status = CaptureStatus::default();
        let result: Option<()> = open_with_retry(
            &running,
            &status,
            || Err(capture::CaptureError::Init("offline".into())),
            || running.store(false, Ordering::Relaxed),
        );
        assert!(result.is_none());
        assert!(status.message().contains("offline"));
    }

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
