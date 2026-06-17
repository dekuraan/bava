//! bava — a Bevy music visualizer driven by cavacore, MPRIS and spotifyd.
//!
//! Pipeline: PulseAudio monitor capture → cavacore analysis → the [`Cava`]
//! resource → visualizers. MPRIS supplies now-playing metadata and album art.
//!
//! [`Cava`]: cava::Cava

mod cava;
mod config;
mod mpris;
mod vis;

use bevy::prelude::*;
use clap::Parser;

use cava::CavaPlugin;
use config::{Cli, Config};
use mpris::MprisPlugin;
use vis::VisPlugin;

fn main() {
    let cli = Cli::parse();

    let path = cli
        .config
        .clone()
        .or_else(Config::default_path)
        .unwrap_or_else(|| std::path::PathBuf::from("config.toml"));

    let mut config = Config::load_or_create(&path);
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
    let vis_style = config.vis_style();

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "bava".into(),
            ..default()
        }),
        ..default()
    }))
    // Dark backdrop so the visualizer pops.
    .insert_resource(ClearColor(Color::srgb(0.02, 0.02, 0.04)))
    // Pipeline + vis config from CLI/TOML; inserted before the plugins so their
    // `init_resource` defaults don't override them.
    .insert_resource(settings)
    .insert_resource(vis_settings)
    .insert_resource(vis_style)
    .add_plugins((CavaPlugin, MprisPlugin, VisPlugin));

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
