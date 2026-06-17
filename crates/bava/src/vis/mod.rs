// SPDX-License-Identifier: GPL-3.0-or-later
//! Visualizers and their configuration.
//!
//! Each visualizer reads the shared [`Cava`](crate::cava::Cava) resource, so all
//! styles consume the same analyzed audio. The option set mirrors
//! [Cavalier](https://github.com/NickvisionApps/Cavalier): a [`DrawingMode`]
//! (one of 11 — a [`VisShape`] laid out in a [`VisFamily`]) plus shared geometry,
//! color [`ColorProfile`]s and [`ImageLayer`] "picture" options carried on
//! [`VisSettings`]. Renderers live in submodules; only a couple are wired up so
//! far, but the config covers every mode/option for forward compatibility.

pub mod bars;
pub mod circle;
pub mod hud;
pub mod physics;
mod stroke;

use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Layout family a drawing mode belongs to: linear (box) or radial (circle).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VisFamily {
    /// Linear, drawn across an axis.
    Box,
    /// Radial, swept around a center.
    Circle,
}

/// The per-bar shape / algorithm, independent of layout family. The box family
/// renders all six distinctly; the circle family renders Wave/Levels/Particles/
/// Bars/Spine via the shared circle renderer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VisShape {
    /// Smooth Bézier waveform.
    Wave,
    /// Stacked discrete blocks (VU-meter style).
    Levels,
    /// One floating item per bar.
    Particles,
    /// Classic spectrum bars.
    Bars,
    /// Centered squares/hearts scaling with level.
    Spine,
    /// Zig-zag oscillating around the axis (box only).
    Splitter,
}

/// Active drawing mode — Cavalier's 11 modes. Each is a [`VisShape`] laid out in
/// a [`VisFamily`]; `Splitter` has no circle form, which is why there are 11 and
/// not 12. Lives as its own resource so it can be toggled live with the space bar.
#[derive(
    Resource, Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum DrawingMode {
    #[default]
    WaveBox,
    LevelsBox,
    ParticlesBox,
    BarsBox,
    SpineBox,
    SplitterBox,
    WaveCircle,
    LevelsCircle,
    ParticlesCircle,
    BarsCircle,
    SpineCircle,
}

impl DrawingMode {
    /// Every mode, in Cavalier's enum order. Used for live cycling.
    pub const ALL: [DrawingMode; 11] = [
        DrawingMode::WaveBox,
        DrawingMode::LevelsBox,
        DrawingMode::ParticlesBox,
        DrawingMode::BarsBox,
        DrawingMode::SpineBox,
        DrawingMode::SplitterBox,
        DrawingMode::WaveCircle,
        DrawingMode::LevelsCircle,
        DrawingMode::ParticlesCircle,
        DrawingMode::BarsCircle,
        DrawingMode::SpineCircle,
    ];

    /// The shape/algorithm of this mode.
    pub fn shape(self) -> VisShape {
        use DrawingMode::*;
        match self {
            WaveBox | WaveCircle => VisShape::Wave,
            LevelsBox | LevelsCircle => VisShape::Levels,
            ParticlesBox | ParticlesCircle => VisShape::Particles,
            BarsBox | BarsCircle => VisShape::Bars,
            SpineBox | SpineCircle => VisShape::Spine,
            SplitterBox => VisShape::Splitter,
        }
    }

    /// Whether this mode is linear (box) or radial (circle).
    pub fn family(self) -> VisFamily {
        use DrawingMode::*;
        match self {
            WaveBox | LevelsBox | ParticlesBox | BarsBox | SpineBox | SplitterBox => VisFamily::Box,
            WaveCircle | LevelsCircle | ParticlesCircle | BarsCircle | SpineCircle => {
                VisFamily::Circle
            }
        }
    }

    /// Next mode for live cycling (wraps around).
    fn next(self) -> Self {
        let i = Self::ALL.iter().position(|&m| m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// Mirroring behaviour (Cavalier `Mirror`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum MirrorMode {
    /// Single visualization, no mirroring.
    #[default]
    Off,
    /// Whole visualization duplicated and reflected; the copy uses reversed order.
    Full,
    /// First half of bars → one side, second half → the mirrored side (stereo L/R).
    SplitChannels,
}

/// Orientation of box modes; also steers the gradient of circle modes
/// (Cavalier `DrawingDirection`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Vertical, origin at the top.
    TopBottom,
    /// Vertical, origin at the bottom (default).
    #[default]
    BottomTop,
    /// Horizontal, origin at the left.
    LeftRight,
    /// Horizontal, origin at the right.
    RightLeft,
}

/// Light/dark association of a [`ColorProfile`] (Cavalier `Theme`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    #[default]
    Dark,
}

/// A named color scheme. A single fg/bg color is a solid; **two or more** form a
/// gradient (linear, or radial for `WaveCircle`). Mirrors Cavalier `ColorProfile`.
#[derive(Clone, Debug)]
pub struct ColorProfile {
    /// Display name.
    pub name: String,
    /// Light/dark hint for any chrome that follows the profile.
    pub theme: Theme,
    /// Foreground color stops (the visualization itself).
    pub fg: Vec<Color>,
    /// Background color stops.
    pub bg: Vec<Color>,
}

impl Default for ColorProfile {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            theme: Theme::Dark,
            // bava's signature blue→pink foreground over a near-black backdrop.
            fg: vec![Color::srgb(0.13, 0.55, 0.95), Color::srgb(0.98, 0.35, 0.70)],
            bg: vec![Color::srgb(0.02, 0.02, 0.04)],
        }
    }
}

impl ColorProfile {
    /// First foreground stop (the "low amplitude" gradient end). White if empty.
    pub fn fg_lo(&self) -> Color {
        self.fg.first().copied().unwrap_or(Color::WHITE)
    }

    /// Last foreground stop (the "full amplitude" gradient end). White if empty.
    pub fn fg_hi(&self) -> Color {
        self.fg.last().copied().unwrap_or(Color::WHITE)
    }
}

/// A background or foreground image overlay — one of Cavalier's "picture"
/// options. `None` path means no image; the active color profile is used instead.
#[derive(Clone, Debug)]
pub struct ImageLayer {
    /// Image file to draw, if any.
    pub path: Option<PathBuf>,
    /// Scale multiplier applied to the source image.
    pub scale: f32,
    /// Opacity in `0..1`.
    pub alpha: f32,
}

impl Default for ImageLayer {
    fn default() -> Self {
        Self {
            path: None,
            scale: 1.0,
            alpha: 1.0,
        }
    }
}

/// Tunables shared by the visualizers. Insert your own before adding
/// [`VisPlugin`]; the active [`DrawingMode`] is a separate, live-toggled resource.
#[derive(Resource, Clone, Debug)]
pub struct VisSettings {
    /// Monstercat-style neighbour spreading (bava-specific smoothing). Each bar
    /// lifts its neighbours by `value / monstercat^distance`, turning spikes into
    /// smooth waves. `1.5` is a gentle wave, higher is tighter, `<= 1.0` disables.
    pub monstercat: f32,
    /// Mirroring behaviour.
    pub mirror: MirrorMode,
    /// Flip which side the mirrored copy is drawn on.
    pub reverse_mirror: bool,
    /// Orientation of box modes / gradient direction of circle modes.
    pub direction: Direction,
    /// Reverse the bar order before drawing.
    pub reverse_order: bool,
    /// Solid fill vs. stroked outline (Wave/Bars).
    pub filling: bool,
    /// Stroke width in pixels when not filling.
    pub line_thickness: f32,
    /// Spacing between discrete items (Levels/Particles), ~0..0.5.
    pub items_offset: f32,
    /// Corner-radius multiplier for items.
    pub items_roundness: f32,
    /// Spine modes draw hearts instead of squares.
    pub hearts: bool,
    /// Circle modes: inner radius as a ratio of the full radius (0..1).
    pub inner_radius: f32,
    /// Circle modes: angular offset in radians.
    pub rotation: f32,
    /// Padding around the whole drawing area, in pixels.
    pub area_margin: f32,
    /// Proportional shift of the draw region.
    pub area_offset: Vec2,
    /// Color schemes; the active one supplies fg/bg colors and gradients.
    pub profiles: Vec<ColorProfile>,
    /// Index of the active profile in [`profiles`](Self::profiles).
    pub active_profile: usize,
    /// Background picture overlay.
    pub background: ImageLayer,
    /// Foreground picture overlay (masked by the visualization shape).
    pub foreground: ImageLayer,
}

impl Default for VisSettings {
    fn default() -> Self {
        Self {
            monstercat: 1.5,
            mirror: MirrorMode::Off,
            reverse_mirror: false,
            direction: Direction::BottomTop,
            reverse_order: true,
            filling: true,
            line_thickness: 6.0,
            items_offset: 0.1,
            items_roundness: 0.5,
            hearts: false,
            inner_radius: 0.5,
            rotation: 0.0,
            area_margin: 0.0,
            area_offset: Vec2::ZERO,
            profiles: vec![ColorProfile::default()],
            active_profile: 0,
            background: ImageLayer::default(),
            foreground: ImageLayer::default(),
        }
    }
}

impl VisSettings {
    /// The active color profile, clamped to a valid index. Falls back to a
    /// default profile if the list is somehow empty.
    pub fn profile(&self) -> ColorProfile {
        if self.profiles.is_empty() {
            return ColorProfile::default();
        }
        let i = self.active_profile.min(self.profiles.len() - 1);
        self.profiles[i].clone()
    }

    /// Low-amplitude foreground gradient end from the active profile.
    pub fn fg_lo(&self) -> Color {
        self.profile().fg_lo()
    }

    /// Full-amplitude foreground gradient end from the active profile.
    pub fn fg_hi(&self) -> Color {
        self.profile().fg_hi()
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

/// Extra brightness applied to full-amplitude colors, pushing them past 1.0 into
/// HDR range so the camera's bloom makes peaks glow. `0.0` disables the glow.
const GLOW_GAIN: f32 = 1.8;

/// Linear gradient color by amplitude `t` (0..1) between two endpoints, boosted
/// into HDR range as `t` rises so loud bars bloom (see [`GLOW_GAIN`]). At `t = 0`
/// the color is unchanged.
pub(crate) fn gradient_color(lo: Color, hi: Color, t: f32) -> Color {
    let a = lo.to_srgba();
    let b = hi.to_srgba();
    let t = t.clamp(0.0, 1.0);
    let base = Color::srgba(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
        0.95,
    );
    // Scale linear RGB by an amplitude-driven gain; values > 1 bloom. Alpha is
    // left untouched so translucent fills stay translucent.
    let lin = base.to_linear();
    let glow = 1.0 + t * GLOW_GAIN;
    Color::linear_rgba(lin.red * glow, lin.green * glow, lin.blue * glow, lin.alpha)
}

/// Selects and installs the visualizers and HUD.
pub struct VisPlugin;

impl Plugin for VisPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VisSettings>()
            .init_resource::<DrawingMode>()
            .add_systems(Update, cycle_mode)
            .add_plugins((
                bars::BarsPlugin,
                circle::CirclePlugin,
                hud::HudPlugin,
                physics::PhysicsPlugin,
            ));
    }
}

/// Space bar cycles the active drawing mode, unless the settings editor is
/// holding keyboard focus (e.g. typing a profile name).
fn cycle_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mut mode: ResMut<DrawingMode>,
    editor: Res<crate::gui::EditorState>,
) {
    if editor.capture_keyboard {
        return;
    }
    if keys.just_pressed(KeyCode::Space) {
        *mode = mode.next();
        info!("bava: drawing mode → {:?}", *mode);
    }
}
