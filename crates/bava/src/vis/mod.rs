//! Visualizers. Each reads the shared [`Cava`](crate::cava::Cava) resource, so
//! 2D bar styles, 3D scenes and (later) shader effects all consume the same
//! analyzed audio. [`VisPlugin`] wires up the default 2D bars style.

pub mod bars;
pub mod hud;

use bevy::prelude::*;

/// Selects and installs the active visualizer(s) and HUD.
pub struct VisPlugin;

impl Plugin for VisPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((bars::BarsPlugin, hud::HudPlugin));
    }
}
