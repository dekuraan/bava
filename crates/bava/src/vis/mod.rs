pub mod bars;
pub mod circle;
pub mod hud;
pub mod physics;

use bevy::prelude::*;

/// Which visualizer is currently drawn. Toggle live with the space bar.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VisStyle {
    #[default]
    Bars,
    Circle,
}

impl VisStyle {
    fn next(self) -> Self {
        match self {
            VisStyle::Bars => VisStyle::Circle,
            VisStyle::Circle => VisStyle::Bars,
        }
    }
}

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
    /// Circle visualizer: fill the ring's interior with a translucent blob.
    pub fill: bool,
    /// Line/outline thickness in pixels (circle ring, etc.).
    pub line_width: f32,
}

impl Default for VisSettings {
    fn default() -> Self {
        Self {
            monstercat: 1.5,
            mirror: false,
            color_lo: Color::srgb(0.13, 0.55, 0.95),
            color_hi: Color::srgb(0.98, 0.35, 0.70),
            fill: false,
            line_width: 6.0,
        }
    }
}

/// Monstercat neighbour spreading shared by the visualizers: each bar raises the
/// others to at least `value / factor^distance`. Sources are the unsmoothed
/// values so the spread is order-independent. `factor <= 1` is a no-op.
pub(crate) fn spread_monstercat(values: &mut [f32], factor: f32) {
    if factor <= 1.0 {
        return;
    }
    let n = values.len();
    let src: Vec<f32> = values.to_vec();
    for z in 0..n {
        let peak = src[z];
        if peak <= 0.0 {
            continue;
        }
        for (m, out) in values.iter_mut().enumerate() {
            if m == z {
                continue;
            }
            let dist = (z as i32 - m as i32).unsigned_abs() as f32;
            let spread = peak / factor.powf(dist);
            if spread > *out {
                *out = spread;
            }
        }
    }
}

/// Linear gradient color by amplitude `t` (0..1) between two `VisSettings` ends.
pub(crate) fn gradient_color(lo: Color, hi: Color, t: f32) -> Color {
    let a = lo.to_srgba();
    let b = hi.to_srgba();
    let t = t.clamp(0.0, 1.0);
    Color::srgba(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
        0.95,
    )
}

/// Selects and installs the visualizers and HUD.
pub struct VisPlugin;

impl Plugin for VisPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VisSettings>()
            .init_resource::<VisStyle>()
            .add_systems(Update, cycle_style)
            .add_plugins((
                bars::BarsPlugin,
                circle::CirclePlugin,
                hud::HudPlugin,
                physics::PhysicsPlugin,
            ));
    }
}

/// Space bar cycles the active visualizer.
fn cycle_style(keys: Res<ButtonInput<KeyCode>>, mut style: ResMut<VisStyle>) {
    if keys.just_pressed(KeyCode::Space) {
        *style = style.next();
        info!("bava: vis style → {:?}", *style);
    }
}
