// SPDX-License-Identifier: MIT OR Apache-2.0
//! Offline music-video rendering (`--input song.mp3 --out video.mp4`).
//!
//! Instead of capturing live audio, the whole input file is decoded up front
//! ([`audio`]) and fed to cavacore one video frame at a time, while the vis
//! camera renders into a headless image that [`bevy_capture`] reads back and
//! streams to an ffmpeg subprocess ([`encoder`]). Time is stepped manually by
//! exactly `1/fps` per frame, so the result is deterministic and renders as
//! fast as the machine allows — faster than realtime on any reasonable GPU,
//! never slower-but-broken.
//!
//! `--headless` (the default when stdout isn't a terminal) skips winit
//! entirely and loops the schedule flat out; otherwise a preview window shows
//! frames as they're rendered (and keyboard/mouse still work, so you can cycle
//! modes or spawn physics balls *into* the video).

pub mod audio;
pub mod encoder;

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use bevy::app::{RunMode, ScheduleRunnerPlugin};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::time::TimeUpdateStrategy;
use bevy::window::{ExitCondition, PresentMode, WindowResolution};
use bevy::winit::WinitPlugin;
use bevy_capture::{Capture, CaptureBundle, CapturePlugin, RenderTargetHeadless};

use crate::cava::{AudioInjector, CavaPlugin};
use crate::config::{Cli, Config};
use crate::gui::EditorState;
use crate::now_playing::NowPlayingPlugin;
use crate::vis::bars::VisCamera;
use crate::vis::VisPlugin;

use audio::DecodedTrack;
use encoder::{EncoderStatus, FfmpegEncoder};

/// Everything the record driver needs, fixed at startup.
#[derive(Resource)]
struct Recording {
    /// Interleaved decoded samples.
    samples: Vec<f64>,
    rate: u32,
    channels: usize,
    /// PCM frames to feed (the whole track, or less under `--duration`).
    total_pcm: usize,
    fps: u32,
    width: u32,
    height: u32,
    headless: bool,
    out: std::path::PathBuf,
}

impl Recording {
    /// Total video frames the recording will contain.
    fn total_frames(&self) -> u64 {
        (self.total_pcm as u64 * self.fps as u64).div_ceil(self.rate.max(1) as u64)
    }
}

/// The ffmpeg encoder, parked until the capture components exist.
#[derive(Resource)]
struct PendingEncoder(Option<(FfmpegEncoder, std::sync::Arc<EncoderStatus>)>);

/// Progress of the drive loop.
#[derive(Resource, Default)]
struct RecordState {
    /// One no-capture frame after the camera is retargeted, so pipelines and
    /// the render target exist before the first captured frame.
    warmed_up: bool,
    capturing: bool,
    /// Video frames fed/captured so far.
    frame: u64,
    /// PCM frames fed so far.
    cursor: usize,
    /// Capture stopped; waiting for ffmpeg to finalize the file.
    stopped: bool,
    status: Option<std::sync::Arc<EncoderStatus>>,
    started_at: Option<Instant>,
    last_log: Option<Instant>,
}

/// Renders the visualization of a decoded track into a video file.
///
/// The track and encoder sit in `Mutex<Option<…>>` only because
/// [`Plugin::build`] gets `&self` — they're taken exactly once (a long track's
/// samples are hundreds of MB; cloning them would double peak memory).
struct RecordPlugin {
    track: std::sync::Mutex<Option<DecodedTrack>>,
    spec: RecordSpec,
    encoder: std::sync::Mutex<Option<(FfmpegEncoder, std::sync::Arc<EncoderStatus>)>>,
}

/// Validated recording parameters.
#[derive(Clone)]
struct RecordSpec {
    out: std::path::PathBuf,
    fps: u32,
    width: u32,
    height: u32,
    headless: bool,
    /// PCM frames to render (the whole track, or less under `--duration`).
    total_pcm: usize,
}

impl Plugin for RecordPlugin {
    fn build(&self, app: &mut App) {
        let track = self
            .track
            .lock()
            .unwrap()
            .take()
            .expect("RecordPlugin built twice");
        app.insert_resource(Recording {
            rate: track.rate,
            channels: track.channels,
            samples: track.samples,
            total_pcm: self.spec.total_pcm,
            fps: self.spec.fps,
            width: self.spec.width,
            height: self.spec.height,
            headless: self.spec.headless,
            out: self.spec.out.clone(),
        })
        .insert_resource(PendingEncoder(self.encoder.lock().unwrap().take()))
        .init_resource::<RecordState>()
        .add_systems(PostStartup, attach_capture)
        .add_systems(PreUpdate, drive_recording);

        if self.spec.headless {
            // No winit, so no OS window — but the vis layout, HUD, and physics
            // systems all read their dimensions from a `Window` entity. A bare
            // one (no `RawHandleWrapper`) is invisible to the render pipeline,
            // so it's pure data: exactly the recording resolution.
            let (w, h) = (self.spec.width, self.spec.height);
            app.add_systems(Startup, move |mut commands: Commands| {
                commands.spawn(Window {
                    resolution: WindowResolution::new(w, h).with_scale_factor_override(1.0),
                    ..default()
                });
            });
        }
    }
}

/// Retarget the vis camera from the window to a headless image of the video
/// resolution, attach the capture machinery, and — when a preview window is up
/// — mirror the image into it with a second minimal camera.
fn attach_capture(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    rec: Res<Recording>,
    camera: Query<Entity, With<VisCamera>>,
) {
    let Ok(cam) = camera.single() else {
        error!("bava: no vis camera to record from");
        return;
    };
    let target = RenderTarget::target_headless(rec.width, rec.height, &mut images);
    let RenderTarget::Image(ref image_target) = target else {
        unreachable!("target_headless always yields an image target");
    };
    let preview_image = image_target.handle.clone();
    commands.entity(cam).insert((target, CaptureBundle::default()));

    if !rec.headless {
        // The capture target doubles as a texture (it's created with
        // TEXTURE_BINDING), so the preview is just a sprite of it on a layer
        // only this camera renders. Tonemapping stays off — the image already
        // holds the vis camera's final tonemapped output.
        commands.spawn((
            Camera2d,
            Camera {
                order: 1,
                ..default()
            },
            Tonemapping::None,
            Msaa::Off,
            RenderLayers::layer(1),
        ));
        commands.spawn((Sprite::from_image(preview_image), RenderLayers::layer(1)));
    }
}

/// The per-frame heartbeat of a recording, in `PreUpdate` so the samples it
/// pushes are analyzed by `feed_cava` (and drawn) in the same frame:
/// warm up → start capture + feed → … → stop → wait for ffmpeg → exit.
fn drive_recording(
    mut state: ResMut<RecordState>,
    rec: Res<Recording>,
    mut pending: ResMut<PendingEncoder>,
    injector: Res<AudioInjector>,
    mut captures: Query<&mut Capture>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(mut capture) = captures.single_mut() else {
        return; // capture bundle not attached yet (pre-PostStartup)
    };
    if !state.warmed_up {
        state.warmed_up = true;
        return;
    }

    // ffmpeg dying mid-render (disk full, bad output path, …) is fatal now —
    // don't keep rendering thousands of frames into a dead pipe.
    if !state.stopped
        && state
            .status
            .as_ref()
            .is_some_and(|s| s.finished() == Some(false))
    {
        error!("bava: ffmpeg exited early; aborting recording");
        exit.write(AppExit::error());
        return;
    }

    // All audio fed: the last real frame was captured last update. Stop the
    // capture (next extract hands the encoder its EOF) and wait for ffmpeg.
    if state.cursor >= rec.total_pcm {
        if !state.stopped {
            capture.stop();
            state.stopped = true;
            return;
        }
        match state.status.as_ref().and_then(|s| s.finished()) {
            Some(true) => {
                let took = state
                    .started_at
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or_default();
                let video_secs = state.frame as f64 / rec.fps as f64;
                info!(
                    "bava: wrote {} — {} frames ({video_secs:.1}s) in {took:.1}s \
                     ({:.1}× realtime)",
                    rec.out.display(),
                    state.frame,
                    video_secs / took.max(1e-9),
                );
                exit.write(AppExit::Success);
            }
            Some(false) => {
                error!("bava: ffmpeg failed; {} may be incomplete", rec.out.display());
                exit.write(AppExit::error());
            }
            None => {} // ffmpeg still finalizing (e.g. the +faststart pass)
        }
        return;
    }

    if !state.capturing {
        let Some((enc, status)) = pending.0.take() else {
            error!("bava: recording encoder missing");
            exit.write(AppExit::error());
            return;
        };
        capture.start(enc);
        state.status = Some(status);
        state.capturing = true;
        state.started_at = Some(Instant::now());
        state.last_log = state.started_at; // first progress line ~2 s in
        info!(
            "bava: recording {} frames @ {}fps ({}x{}, {}) → {}",
            rec.total_frames(),
            rec.fps,
            rec.width,
            rec.height,
            if rec.headless { "headless" } else { "preview" },
            rec.out.display(),
        );
    }

    // Feed exactly this frame's slice of audio.
    let next = pcm_target(state.frame + 1, rec.rate, rec.fps, rec.total_pcm);
    injector.push(&rec.samples[state.cursor * rec.channels..next * rec.channels]);
    state.cursor = next;
    state.frame += 1;

    // Progress about every 2 s of wall clock.
    let now = Instant::now();
    if state.last_log.is_none_or(|t| now - t >= Duration::from_secs(2)) {
        if let Some(t0) = state.started_at {
            let elapsed = (now - t0).as_secs_f64().max(1e-9);
            let done = state.frame;
            let total = rec.total_frames().max(1);
            let wall_fps = done as f64 / elapsed;
            let eta = (total.saturating_sub(done)) as f64 / wall_fps.max(1e-9);
            info!(
                "bava: {done}/{total} frames ({:.0}%) — {wall_fps:.0} fps, \
                 {:.1}× realtime, ~{eta:.0}s left",
                done as f64 / total as f64 * 100.0,
                wall_fps / rec.fps as f64,
            );
        }
        state.last_log = Some(now);
    }
}

/// PCM frames (per channel) that must have been fed once `frames` video frames
/// are rendered. The cumulative rational cursor (`floor(frames·rate/fps)`)
/// never drifts, whatever the rate/fps ratio — successive differences are the
/// ideal `rate/fps` to within one sample, and it lands exactly on `total`.
fn pcm_target(frames: u64, rate: u32, fps: u32, total: usize) -> usize {
    ((frames * rate as u64 / fps.max(1) as u64) as usize).min(total)
}

/// Decode `--input`, build the offline app, render, encode. Returns an error
/// string for `main` to print; `Ok` means the video was written and verified
/// by ffmpeg's exit status.
pub fn run(cli: &Cli, config: &Config) -> Result<(), String> {
    let input = cli.input.clone().expect("run() requires --input");
    let out = cli.out.clone().expect("clap enforces --out with --input");

    if !encoder::ffmpeg_available() {
        return Err(
            "ffmpeg not found on PATH — it does the video encoding; install it and retry".into(),
        );
    }

    // x264 with yuv420p needs even dimensions.
    let width = cli.width & !1;
    let height = cli.height & !1;
    if width == 0 || height == 0 {
        return Err(format!("invalid video size {}x{}", cli.width, cli.height));
    }
    if width != cli.width || height != cli.height {
        eprintln!("bava: rounding video size down to even {width}x{height} (x264 requirement)");
    }
    let fps = cli.fps;
    if fps == 0 || fps > 240 {
        return Err(format!("--fps {fps} is out of range (1..=240)"));
    }

    let track = audio::decode(&input)?;
    let headless = cli
        .headless
        .unwrap_or_else(|| !std::io::stdout().is_terminal());
    let total_pcm = cli
        .duration
        .filter(|s| *s > 0.0)
        .map_or(track.pcm_frames(), |s| {
            ((s * track.rate as f64) as usize).min(track.pcm_frames())
        });
    // The video is a whole number of frames covering all fed audio; ffmpeg
    // trims both streams to exactly this length.
    let total_frames = (total_pcm as u64 * fps as u64).div_ceil(track.rate.max(1) as u64);
    let video_secs = total_frames as f64 / fps as f64;

    eprintln!(
        "bava: {} — {:.1}s @ {} Hz, {} ch{}",
        input.display(),
        track.duration_secs(),
        track.rate,
        track.channels,
        track
            .track
            .title
            .as_deref()
            .map(|t| format!(" — \"{t}\""))
            .unwrap_or_default(),
    );

    let (enc, status) = FfmpegEncoder::spawn(&out, &input, width, height, fps, video_secs)
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;

    // The cavacore plan must match the decoded stream exactly; capture-thread
    // options are meaningless offline.
    let mut settings = config.to_cava_settings(cli.debug);
    settings.rate = track.rate;
    settings.channels = track.channels;
    settings.source = None;
    settings.follow_active_sink = false;

    let offline_track = track.track.clone();
    let spec = RecordSpec {
        out,
        fps,
        width,
        height,
        headless,
        total_pcm,
    };

    let mut app = App::new();
    // Compile render pipelines synchronously so early frames aren't skipped
    // while shaders build in the background — every frame must be captured.
    let default_plugins = DefaultPlugins.set(RenderPlugin {
        synchronous_pipeline_compilation: true,
        ..default()
    });
    if headless {
        app.add_plugins((
            default_plugins
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    close_when_requested: false,
                    ..default()
                })
                .disable::<WinitPlugin>(),
            ScheduleRunnerPlugin {
                run_mode: RunMode::Loop { wait: None },
            },
        ));
    } else {
        app.add_plugins(default_plugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "bava — recording".into(),
                resolution: WindowResolution::new(width, height).with_scale_factor_override(1.0),
                resizable: false,
                // Never let the compositor pace us — render flat out.
                present_mode: PresentMode::AutoNoVsync,
                ..default()
            }),
            ..default()
        }));
    }

    app.add_plugins(CapturePlugin)
        .insert_resource(ClearColor(Color::srgb(0.02, 0.02, 0.04)))
        .insert_resource(settings)
        .insert_resource(config.to_vis_settings())
        .insert_resource(config.to_physics_settings())
        .insert_resource(config.vis_mode())
        // No settings editor while recording, but vis/physics systems read this.
        .insert_resource(EditorState::new(false, config.gui_toggle_key()))
        // Deterministic time: exactly one video frame per update, regardless
        // of how fast the machine renders.
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / fps as f64,
        )))
        .add_plugins((
            CavaPlugin { offline: true },
            NowPlayingPlugin {
                offline: Some(offline_track),
            },
            VisPlugin,
            RecordPlugin {
                track: std::sync::Mutex::new(Some(track)),
                spec,
                encoder: std::sync::Mutex::new(Some((enc, status))),
            },
        ));

    match app.run() {
        AppExit::Success => Ok(()),
        AppExit::Error(_) => Err("recording failed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeding frame by frame must cover every sample exactly once, in order,
    /// with steady per-frame chunks — cavacore's autosens depends on it.
    #[test]
    fn pcm_cursor_covers_everything_without_drift() {
        // Awkward ratios included: 44100/60 = 735 exact, 44100/30 = 1470,
        // 48000/24 = 2000, 44100/24 = 1837.5, 22050/60 = 367.5.
        for (rate, fps) in [(44_100, 60), (44_100, 30), (48_000, 24), (44_100, 24), (22_050, 60)] {
            let total = rate as usize * 3 + 217; // ~3 s, deliberately not round
            let ideal = rate as f64 / fps as f64;
            let mut cursor = 0usize;
            let mut frame = 0u64;
            while cursor < total {
                frame += 1;
                let next = pcm_target(frame, rate, fps, total);
                assert!(next >= cursor, "cursor went backwards at {rate}/{fps}");
                let chunk = next - cursor;
                // Every full frame feeds the ideal count ±1 sample (the final
                // partial frame may be short).
                if next < total {
                    assert!(
                        (chunk as f64 - ideal).abs() <= 1.0,
                        "uneven chunk {chunk} (ideal {ideal}) at {rate}/{fps} frame {frame}"
                    );
                }
                cursor = next;
            }
            assert_eq!(cursor, total, "did not land exactly on total at {rate}/{fps}");
            // Frame count matches the ceil the encoder was told to expect.
            let expected = (total as u64 * fps as u64).div_ceil(rate as u64);
            assert_eq!(frame, expected, "frame count mismatch at {rate}/{fps}");
        }
    }

    #[test]
    fn pcm_target_clamps_and_survives_zero_fps() {
        assert_eq!(pcm_target(1_000_000, 44_100, 60, 500), 500);
        // fps is validated at the CLI, but the helper must not divide by zero.
        assert_eq!(pcm_target(10, 44_100, 0, usize::MAX), 441_000);
    }
}
