//! Visualizers. Each reads the shared [`Cava`](crate::cava::Cava) resource, so
//! 2D bar styles, 3D scenes and (later) shader effects all consume the same
//! analyzed audio. [`VisPlugin`] wires up the default 2D bars style.

pub mod bars;
pub mod hud;

use bevy::prelude::*;

/// Tunables for the visualizers. Insert your own before adding [`VisPlugin`].
#[derive(Resource, Clone, Debug)]
pub struct VisSettings {
    /// Monstercat-style neighbor spreading. Each bar lifts its neighbours by
    /// `value / monstercat^distance`, turning spikes into smooth waves. `1.5`
    /// is a gentle wave, higher is tighter, `<= 1.0` disables the effect.
    pub monstercat: f32,
    /// Mirror bars symmetrically from the vertical center instead of growing
    /// from the bottom.
    pub mirror: bool,
    /// Foreground gradient: bar color at low amplitude…
    pub color_lo: Color,
    /// …and at full amplitude. Bars lerp between the two by height.
    pub color_hi: Color,
}

impl Default for VisSettings {
    fn default() -> Self {
        Self {
            monstercat: 1.5,
            mirror: false,
            color_lo: Color::srgb(0.13, 0.55, 0.95),
            color_hi: Color::srgb(0.98, 0.35, 0.70),
        }
    }
}

/// Selects and installs the active visualizer(s) and HUD.
pub struct VisPlugin;

impl Plugin for VisPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VisSettings>()
            .add_plugins((bars::BarsPlugin, hud::HudPlugin));
    }
}
