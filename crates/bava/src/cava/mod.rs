//! The cavacore subsystem: captures audio on a background thread, runs it
//! through [`cavacore_rs`], and publishes the latest bar values into the
//! [`Cava`] resource for visualizers to read.

pub mod capture;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use bevy::prelude::*;
use cavacore_rs::CavaConfig;

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
    /// Samples per channel read per cavacore execution. Lower = higher frame
    /// rate / lower latency; 512 @ 44100 ≈ 86 Hz.
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
}

impl Default for CavaSettings {
    fn default() -> Self {
        Self {
            bars_per_channel: 24,
            channels: 2,
            rate: 44_100,
            frame_samples: 512,
            autosens: true,
            noise_reduction: 0.77,
            low_cutoff_freq: 50,
            high_cutoff_freq: 10_000,
            source: None,
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

/// Double-buffered hand-off from the capture thread to the Bevy world.
struct SharedBars {
    bars: Vec<f32>,
    generation: u64,
}

/// Handle shared with the capture thread; lives as a Bevy resource so the thread
/// can be told to stop on exit.
#[derive(Resource, Clone)]
struct CavaLink {
    shared: Arc<Mutex<SharedBars>>,
    running: Arc<AtomicBool>,
}

/// Drives audio capture → cavacore → the [`Cava`] resource.
pub struct CavaPlugin;

impl Plugin for CavaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CavaSettings>()
            .init_resource::<Cava>()
            .add_systems(Startup, start_capture)
            .add_systems(Update, pull_bars)
            .add_systems(Last, stop_on_exit);
    }
}

/// Startup: size the [`Cava`] resource and spawn the capture thread.
fn start_capture(mut commands: Commands, settings: Res<CavaSettings>, mut cava: ResMut<Cava>) {
    let settings = settings.clone();
    let output_len = settings.bars_per_channel * settings.channels;

    cava.bars = vec![0.0; output_len];
    cava.bars_per_channel = settings.bars_per_channel;
    cava.channels = settings.channels;

    let shared = Arc::new(Mutex::new(SharedBars {
        bars: vec![0.0; output_len],
        generation: 0,
    }));
    let running = Arc::new(AtomicBool::new(true));

    let link = CavaLink {
        shared: shared.clone(),
        running: running.clone(),
    };

    thread::Builder::new()
        .name("bava-capture".into())
        .spawn(move || capture_loop(settings, shared, running))
        .expect("failed to spawn capture thread");

    commands.insert_resource(link);
}

/// Background capture + analysis loop. All PulseAudio/cavacore state lives here
/// so nothing non-`Send` ever crosses into Bevy.
fn capture_loop(settings: CavaSettings, shared: Arc<Mutex<SharedBars>>, running: Arc<AtomicBool>) {
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

    let cfg = CavaConfig {
        bars: settings.bars_per_channel as u32,
        rate: capture.rate(),
        channels: capture.channels(),
        autosens: settings.autosens,
        noise_reduction: settings.noise_reduction,
        low_cutoff_freq: settings.low_cutoff_freq,
        high_cutoff_freq: settings.high_cutoff_freq,
    };
    let mut plan = match cfg.build() {
        Ok(p) => p,
        Err(e) => {
            error!("bava: cavacore init failed: {e}");
            return;
        }
    };

    info!(
        "bava: capturing {} ch @ {} Hz, {} bars/ch",
        capture.channels(),
        capture.rate(),
        plan.bars()
    );

    let frame_len = settings.frame_samples * settings.channels;
    let mut samples = vec![0.0f64; frame_len];

    while running.load(Ordering::Relaxed) {
        match capture.read(&mut samples) {
            Ok(0) => break, // end of stream
            Ok(_) => {}
            Err(e) => {
                error!("bava: {e}");
                continue;
            }
        }

        let bars = plan.execute(&samples);

        if let Ok(mut guard) = shared.lock() {
            guard.bars.clear();
            guard.bars.extend(bars.iter().map(|&v| v as f32));
            guard.generation = guard.generation.wrapping_add(1);
        }
    }
}

/// Each frame: copy the latest bars from the capture thread into [`Cava`].
fn pull_bars(link: Res<CavaLink>, mut cava: ResMut<Cava>, mut last_gen: Local<u64>) {
    if let Ok(guard) = link.shared.lock() {
        if guard.generation != *last_gen {
            *last_gen = guard.generation;
            cava.bars.clear();
            cava.bars.extend_from_slice(&guard.bars);
        }
    }
}

/// Signal the capture thread to stop when the app is exiting.
fn stop_on_exit(mut exit: MessageReader<AppExit>, link: Option<Res<CavaLink>>) {
    if exit.read().next().is_some() {
        if let Some(link) = link {
            link.running.store(false, Ordering::Relaxed);
        }
    }
}
