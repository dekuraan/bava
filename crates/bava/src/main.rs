// SPDX-License-Identifier: MIT OR Apache-2.0
//! bava — a cross-platform music visualizer driven by cavacore.
//!
//! Pipeline: loopback audio capture (PulseAudio / WASAPI / Core Audio, or a
//! tab-share `MediaStream` in the browser) → cavacore analysis → the [`Cava`]
//! resource → visualizers. OS media session integration (MPRIS / GSMTC /
//! MediaRemote) supplies now-playing metadata and album art.
//!
//! [`Cava`]: cava::Cava

mod cava;
mod config;
mod gui;
mod now_playing;
// Offline video rendering shells out to ffmpeg and decodes files off a
// filesystem — neither exists in the browser.
#[cfg(not(target_arch = "wasm32"))]
mod record;
mod vis;

use bevy::prelude::*;
use bevy_egui::EguiPlugin;

use cava::CavaPlugin;
use config::{Config, ConfigHandle};
use gui::{EditorState, GuiPlugin};
use now_playing::NowPlayingPlugin;
use vis::VisPlugin;

fn main() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    let cli = config::parse_cli();

    let path = cli
        .config
        .clone()
        .or_else(Config::default_path)
        .unwrap_or_else(|| std::path::PathBuf::from("config.toml"));

    let mut config = Config::load_or_create(&path);
    // A named profile, if requested, becomes the base before CLI overrides.
    if let Some(name) = &cli.profile {
        match Config::load_profile(name) {
            Some(profile) => config = profile,
            None => eprintln!("bava: profile '{name}' not found; using config file"),
        }
    }
    config.apply_cli(&cli);

    if cli.print_config {
        match toml::to_string_pretty(&config) {
            Ok(s) => println!("# resolved config (from {})\n\n{s}", path.display()),
            Err(e) => eprintln!("bava: could not render config: {e}"),
        }
        return;
    }

    // `--input song.mp3 --out video.mp4`: render a music video offline instead
    // of visualizing live audio.
    #[cfg(not(target_arch = "wasm32"))]
    if cli.input.is_some() {
        if let Err(e) = record::run(&cli, &config) {
            eprintln!("bava: {e}");
            std::process::exit(1);
        }
        return;
    }

    let settings = config.to_cava_settings(cli.debug);
    let vis_settings = config.to_vis_settings();
    let physics_settings = config.to_physics_settings();
    let vis_mode = config.vis_mode();

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "bava".into(),
            // In the browser the app lives in a `<canvas>` the page already
            // laid out; without this selector winit creates its own canvas and
            // appends it to `<body>`, escaping the page's styling.
            #[cfg(target_arch = "wasm32")]
            canvas: Some("#bava-canvas".into()),
            // Let the page own the canvas size (it tracks the viewport in CSS)
            // instead of winit forcing the requested resolution back onto it.
            // (`prevent_default_event_handling` is already Bevy's default, so
            // Space and the other hotkeys reach us rather than the browser.)
            #[cfg(target_arch = "wasm32")]
            fit_canvas_to_parent: true,
            ..default()
        }),
        ..default()
    }))
    .add_plugins(EguiPlugin::default())
    // Dark backdrop so the visualizer pops.
    .insert_resource(ClearColor(Color::srgb(0.02, 0.02, 0.04)))
    // Pipeline + vis config from CLI/TOML; inserted before the plugins so their
    // `init_resource` defaults don't override them.
    .insert_resource(settings)
    .insert_resource(vis_settings)
    .insert_resource(physics_settings)
    .insert_resource(vis_mode)
    // Where the editor saves/reloads, and whether it starts open.
    .insert_resource(ConfigHandle { path })
    .insert_resource(EditorState::new(cli.gui, config.gui_toggle_key()))
    .add_plugins((
        CavaPlugin::default(),
        NowPlayingPlugin::default(),
        VisPlugin,
        GuiPlugin,
    ));

    // `--debug` also logs FPS/frame time ~1×/s. The vis plugin already adds
    // `FrameTimeDiagnosticsPlugin` (for the F3 overlay), so adding it again here
    // would panic ("plugin was already added"); only the log sink is needed.
    if cli.debug {
        use bevy::diagnostic::LogDiagnosticsPlugin;
        app.add_plugins(LogDiagnosticsPlugin::default());
    }

    app.run();
}
