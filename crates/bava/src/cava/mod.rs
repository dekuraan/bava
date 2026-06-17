//! The cavacore subsystem.
//!
//! A background thread captures audio into a ring buffer; a Bevy system drains
//! that buffer and calls `cava_execute` **once per rendered frame**, so cavacore
//! runs at the render rate. Its framerate-adaptive smoothing then produces
//! native, low-latency motion at whatever FPS the window runs — no interpolation
//! needed. The result is published into the [`Cava`] resource for visualizers.

pub mod capture;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use bevy::prelude::*;
use cavacore_rs::{CavaConfig, CavaPlan};

use capture::pulse::PulseCapture;
use capture::AudioCapture;

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
    /// Samples per channel per capture read. This is just the feed granularity:
    /// smaller chunks fill the ring more finely so each rendered frame gets a
    /// roughly even slice of fresh audio (cava execute rate follows the render
    /// rate, not this). 128 @ 44100 ≈ 2.9 ms/read.
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
            debug: false,
        }
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
        }
    }
}

/// cavacore state. Held as a **NonSend** resource because [`CavaPlan`] is `Send`
/// but not `Sync`; it is executed exclusively on the main thread, once per frame.
struct CavaState {
    plan: CavaPlan,
    /// Reused buffer for the samples drained each frame.
    scratch: Vec<f64>,
}

/// Drives audio capture → cavacore (at render rate) → the [`Cava`] resource.
pub struct CavaPlugin;

impl Plugin for CavaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CavaSettings>().init_resource::<Cava>();
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
                app.insert_non_send_resource(CavaState {
                    plan,
                    scratch: Vec::new(),
                });
            }
            Err(e) => error!("bava: cavacore init failed: {e}; visualizer will be idle"),
        }

        // Spawn the audio reader thread feeding the ring.
        let ring = AudioRing::new(settings.rate, settings.channels);
        let reader_ring = ring.clone();
        let reader_settings = settings.clone();
        thread::Builder::new()
            .name("bava-capture".into())
            .spawn(move || capture_reader(reader_settings, reader_ring))
            .expect("failed to spawn capture thread");
        app.insert_resource(ring);

        app.add_systems(Update, feed_cava)
            .add_systems(Last, stop_on_exit);
    }
}

/// Pure audio reader: pulls small chunks from PulseAudio and appends them to the
/// ring. No cavacore here — analysis happens on the render thread.
fn capture_reader(settings: CavaSettings, ring: AudioRing) {
    let mut capture = match PulseCapture::open(
        settings.source.as_deref(),
        settings.rate,
        settings.channels as u8,
    ) {
        Ok(c) => c,
        Err(e) => {
            error!("bava: audio capture unavailable, visualizer will be idle: {e}");
            return;
        }
    };

    info!(
        "bava: capturing {} ch @ {} Hz",
        capture.channels(),
        capture.rate()
    );

    let chunk = settings.frame_samples.max(1) * settings.channels.max(1);
    let mut buf = vec![0.0f64; chunk];

    while ring.running.load(Ordering::Relaxed) {
        match capture.read(&mut buf) {
            Ok(0) => break, // end of stream
            Ok(_) => {}
            Err(e) => {
                error!("bava: {e}");
                continue;
            }
        }
        if let Ok(mut q) = ring.buf.lock() {
            q.extend(buf.iter().copied());
            while q.len() > ring.cap {
                q.pop_front();
            }
        }
    }
}

/// Once per rendered frame: drain all audio captured since the last frame, run a
/// single `cava_execute`, and publish the bars. Calling cava at the render rate
/// lets its framerate-adaptive smoothing produce native, low-latency motion.
fn feed_cava(
    ring: Res<AudioRing>,
    state: Option<NonSendMut<CavaState>>,
    mut cava: ResMut<Cava>,
    settings: Res<CavaSettings>,
    mut dbg: Local<FeedStats>,
) {
    let Some(mut state) = state else {
        return; // cavacore failed to init; leave bars at zero
    };
    let state = &mut *state;

    // Drain everything captured since the previous frame (≈ rate / fps samples).
    state.scratch.clear();
    if let Ok(mut q) = ring.buf.lock() {
        state.scratch.extend(q.drain(..));
    }

    // Feeding 0 new samples is valid and just advances the smoothing filters.
    let bars = state.plan.execute(&state.scratch);

    cava.bars.clear();
    cava.bars.extend(bars.iter().map(|&v| v as f32));

    // Aggregate over a ~1s window so the stats reflect every frame, not a single
    // sampled one (which can repeatedly land on an empty-ring frame).
    if settings.debug {
        dbg.frames += 1;
        dbg.total_samples += state.scratch.len() as u64;
        dbg.max_in = dbg
            .max_in
            .max(state.scratch.iter().fold(0.0f64, |m, &s| m.max(s.abs())));
        dbg.max_out = dbg.max_out.max(bars.iter().fold(0.0f64, |m, &b| m.max(b)));
        if dbg.frames >= 240 {
            info!(
                "bava: {} frames | avg new_samples/frame={:.0} | max input={:.3} | max bar={:.3}",
                dbg.frames,
                dbg.total_samples as f64 / dbg.frames as f64,
                dbg.max_in,
                dbg.max_out,
            );
            *dbg = FeedStats::default();
        }
    }
}

/// Rolling debug accumulator for [`feed_cava`].
#[derive(Default)]
struct FeedStats {
    frames: u64,
    total_samples: u64,
    max_in: f64,
    max_out: f64,
}

/// Signal the capture thread to stop when the app is exiting.
fn stop_on_exit(mut exit: MessageReader<AppExit>, ring: Option<Res<AudioRing>>) {
    if exit.read().next().is_some() {
        if let Some(ring) = ring {
            ring.running.store(false, Ordering::Relaxed);
        }
    }
}
