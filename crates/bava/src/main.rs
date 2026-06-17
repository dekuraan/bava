//! bava — a Bevy music visualizer driven by cavacore, MPRIS and spotifyd.
//!
//! Pipeline: PulseAudio monitor capture → cavacore analysis → the [`Cava`]
//! resource → visualizers. MPRIS supplies now-playing metadata and album art.
//!
//! [`Cava`]: cava::Cava

mod cava;
mod mpris;
mod vis;

use bevy::prelude::*;

use cava::CavaPlugin;
use mpris::MprisPlugin;
use vis::VisPlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "bava".into(),
                ..default()
            }),
            ..default()
        }))
        // Dark backdrop so the visualizer pops.
        .insert_resource(ClearColor(Color::srgb(0.02, 0.02, 0.04)))
        .add_plugins((CavaPlugin, MprisPlugin, VisPlugin))
        .run();
}
