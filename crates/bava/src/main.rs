// SPDX-License-Identifier: GPL-3.0-or-later
//! bava — a cross-platform music visualizer driven by cavacore.
//!
//! Pipeline: loopback audio capture (PulseAudio / WASAPI / Core Audio) →
//! cavacore analysis → the [`Cava`] resource → visualizers. OS media session
//! integration (MPRIS / GSMTC / MediaRemote) supplies now-playing metadata and
//! album art.
//!
//! [`Cava`]: cava::Cava

mod cava;
mod config;
mod gui;
mod now_playing;
mod vis;

use bevy::prelude::*;
use bevy_egui::EguiPlugin;
use clap::Parser;

use cava::CavaPlugin;
use config::{Cli, Config, ConfigHandle};
use gui::{EditorState, GuiPlugin};
use now_playing::NowPlayingPlugin;
use vis::VisPlugin;

fn main() {
    let cli = Cli::parse();

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

    let settings = config.to_cava_settings(cli.debug);
    let vis_settings = config.to_vis_settings();
    let physics_settings = config.to_physics_settings();
    let vis_mode = config.vis_mode();

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "bava".into(),
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
    .add_plugins((CavaPlugin, NowPlayingPlugin, VisPlugin, GuiPlugin));

    // `--debug` also enables frame-time diagnostics, logging FPS/frame time ~1×/s.
    if cli.debug {
        use bevy::diagnostic::{FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin};
        app.add_plugins((
            FrameTimeDiagnosticsPlugin::default(),
            LogDiagnosticsPlugin::default(),
        ));
    }

    app.run();
}
