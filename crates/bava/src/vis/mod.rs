// SPDX-License-Identifier: MIT OR Apache-2.0
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

use bevy::color::{Mix, Oklcha};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::now_playing::AlbumArt;

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

/// HDR → display tone-mapping curve applied by the camera. The HDR camera lets
/// amplitude-boosted colors run past 1.0 so peaks bloom; the tone mapper decides
/// how those out-of-range values land on screen. `None` hard-clips per channel
/// (punchy neon blowout); the filmic curves roll highlights off smoothly.
///
/// Mirrors [`bevy::core_pipeline::tonemapping::Tonemapping`]. The `AgX`,
/// `TonyMcMapface` and `BlenderFilmic` curves need the `tonemapping_luts` cargo
/// feature (enabled); the rest are analytic.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ToneMap {
    /// No mapping — values are clamped to `[0, 1]` per channel (hard highlight clip).
    None,
    /// Simple Reinhard `c / (1 + c)` per channel; desaturates highlights.
    Reinhard,
    /// Reinhard applied to luminance only; preserves hue better than [`Self::Reinhard`].
    ReinhardLuminance,
    /// Fitted ACES filmic curve; contrasty, slightly crushed blacks.
    AcesFitted,
    /// AgX filmic curve (needs LUTs); gentle, film-like highlight rolloff.
    AgX,
    /// A neutral display transform; minimal look, mostly for reference.
    SomewhatBoringDisplayTransform,
    /// TonyMcMapface (needs LUTs); Bevy's default — natural, hue-preserving rolloff.
    #[default]
    TonyMcMapface,
    /// Blender's Filmic curve (needs LUTs).
    BlenderFilmic,
}

impl From<ToneMap> for bevy::core_pipeline::tonemapping::Tonemapping {
    fn from(t: ToneMap) -> Self {
        use bevy::core_pipeline::tonemapping::Tonemapping as B;
        match t {
            ToneMap::None => B::None,
            ToneMap::Reinhard => B::Reinhard,
            ToneMap::ReinhardLuminance => B::ReinhardLuminance,
            ToneMap::AcesFitted => B::AcesFitted,
            ToneMap::AgX => B::AgX,
            ToneMap::SomewhatBoringDisplayTransform => B::SomewhatBoringDisplayTransform,
            ToneMap::TonyMcMapface => B::TonyMcMapface,
            ToneMap::BlenderFilmic => B::BlenderFilmic,
        }
    }
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
    /// HDR → display tone-mapping curve applied by the camera.
    pub tonemapping: ToneMap,
    /// Bloom intensity on the HDR camera (0 = no bloom, 0.25 = subtle glow).
    pub bloom_intensity: f32,
    /// HDR glow multiplier: how far past 1.0 loud bars are pushed before tone
    /// mapping. `0.0` disables the per-amplitude brightness boost entirely.
    pub glow_gain: f32,
    /// When `true`, the foreground gradient follows colors extracted from the
    /// current track's album art instead of the active profile's `fg` stops.
    pub dynamic_colors: bool,
    /// How many album-art colors to use when [`dynamic_colors`](Self::dynamic_colors)
    /// is on (2..=[`MAX_DYNAMIC_COLORS`](crate::now_playing::MAX_DYNAMIC_COLORS)).
    pub dynamic_color_count: usize,
    /// Crossfade time constant (seconds) when the dynamic palette changes on a new
    /// track. Larger = slower, more gradual color transition; ~0 snaps instantly.
    pub dynamic_color_fade: f32,
    /// Runtime-only animated album colors (most vibrant first), eased toward the
    /// latest extracted set by [`animate_album_colors`]. Not serialized; when
    /// `Some` and [`dynamic_colors`](Self::dynamic_colors) is set it overrides the
    /// profile's `fg` stops for [`fg_lo`](Self::fg_lo) / [`fg_hi`](Self::fg_hi) /
    /// [`fg_stops`](Self::fg_stops), driving every renderer and the physics balls.
    pub dynamic_fg: Option<Vec<Color>>,
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
            inner_radius: 0.38,
            rotation: 0.0,
            area_margin: 0.0,
            area_offset: Vec2::ZERO,
            profiles: vec![ColorProfile::default()],
            active_profile: 0,
            background: ImageLayer::default(),
            foreground: ImageLayer::default(),
            tonemapping: ToneMap::default(),
            bloom_intensity: 0.25,
            glow_gain: 1.8,
            dynamic_colors: false,
            dynamic_color_count: 2,
            dynamic_color_fade: 0.4,
            dynamic_fg: None,
        }
    }
}

impl VisSettings {
    /// Active foreground color stops: the dynamic album-art palette (clamped to
    /// [`dynamic_color_count`](Self::dynamic_color_count)) when dynamic colors are
    /// on and a palette is available, else the active profile's `fg` stops. Always
    /// non-empty (falls back to white). Drives every renderer and the physics balls.
    pub fn fg_stops(&self) -> Vec<Color> {
        match self.active_fg() {
            Some(fg) => fg.to_vec(),
            None => vec![Color::WHITE],
        }
    }

    /// Borrow the active fg stops without allocating: the dynamic album-art palette
    /// (clamped to [`dynamic_color_count`](Self::dynamic_color_count)) when dynamic
    /// colors are on and available, else the active profile's `fg`. `None` means
    /// there are no stops and callers fall back to white. Lets `fg_lo`/`fg_hi` read
    /// a single stop without cloning a whole [`ColorProfile`] per call.
    fn active_fg(&self) -> Option<&[Color]> {
        if self.dynamic_colors
            && let Some(colors) = &self.dynamic_fg
            && !colors.is_empty()
        {
            let n = self.dynamic_color_count.clamp(2, colors.len());
            return Some(&colors[..n]);
        }
        if self.profiles.is_empty() {
            return None;
        }
        let i = self.active_profile.min(self.profiles.len() - 1);
        let fg = &self.profiles[i].fg;
        (!fg.is_empty()).then_some(fg.as_slice())
    }

    /// Low-amplitude foreground gradient end (the first active stop).
    pub fn fg_lo(&self) -> Color {
        self.active_fg().and_then(|s| s.first().copied()).unwrap_or(Color::WHITE)
    }

    /// Full-amplitude foreground gradient end (the last active stop).
    pub fn fg_hi(&self) -> Color {
        self.active_fg().and_then(|s| s.last().copied()).unwrap_or(Color::WHITE)
    }
}

/// Monstercat neighbour spreading shared by the visualizers: each bar raises the
/// others to at least `value / factor^distance`. Sources are the unsmoothed
/// values so the spread is order-independent. `factor <= 1` is a no-op.
pub(crate) fn spread_monstercat(values: &mut [f32], factor: f32) {
    if factor <= 1.0 {
        return;
    }
    // Each output is `max over z of values[z] / factor^|z - m|`, which
    // decomposes into a forward then a backward running-peak sweep (the peak
    // decays by one `factor` per step) — O(n) instead of the naive O(n²)
    // pairwise loop, and safe to run in place: a peak relayed through an
    // already-spread neighbour decays over a path at least as long as the
    // direct distance, so it can never exceed the direct spread.
    let mut running = 0.0f32;
    for v in values.iter_mut() {
        running = (running / factor).max(*v);
        *v = running;
    }
    let mut running = 0.0f32;
    for v in values.iter_mut().rev() {
        running = (running / factor).max(*v);
        *v = running;
    }
}

/// Linear gradient color by amplitude `t` (0..1) between two endpoints, boosted
/// into HDR range as `t` rises so loud bars bloom. `glow_gain` scales how far past
/// 1.0 the color is pushed; `0.0` disables HDR glow.
pub(crate) fn gradient_color(lo: Color, hi: Color, t: f32, glow_gain: f32) -> Color {
    let a = lo.to_srgba();
    let b = hi.to_srgba();
    let t = t.clamp(0.0, 1.0);
    let base = Color::srgba(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
        0.95,
    );
    let lin = base.to_linear();
    let glow = 1.0 + t * glow_gain;
    Color::linear_rgba(lin.red * glow, lin.green * glow, lin.blue * glow, lin.alpha)
}

/// Multi-stop gradient sample by amplitude `t` (0..1) across N evenly-spaced color
/// stops, HDR-boosted like [`gradient_color`]. With two stops it's identical to
/// `gradient_color`; with more it lets the dynamic album palette span every color.
pub(crate) fn sample_gradient(stops: &[Color], t: f32, glow_gain: f32) -> Color {
    match stops {
        [] => gradient_color(Color::WHITE, Color::WHITE, t, glow_gain),
        [only] => gradient_color(*only, *only, t, glow_gain),
        _ => {
            let t = t.clamp(0.0, 1.0);
            let span = (stops.len() - 1) as f32;
            let scaled = t * span;
            let i = (scaled.floor() as usize).min(stops.len() - 2);
            // Local 0..1 position within segment `i`, re-driving the HDR glow so the
            // brightness still tracks the global amplitude `t`.
            let local = scaled - i as f32;
            let seg = gradient_color(stops[i], stops[i + 1], local, 0.0).to_linear();
            let glow = 1.0 + t * glow_gain;
            Color::linear_rgba(seg.red * glow, seg.green * glow, seg.blue * glow, seg.alpha)
        }
    }
}

/// Selects and installs the visualizers and HUD.
pub struct VisPlugin;

impl Plugin for VisPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VisSettings>()
            .init_resource::<DrawingMode>()
            .init_resource::<AlbumPalette>()
            .add_systems(Update, (cycle_mode, animate_album_colors))
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
    // If the user rebound the editor toggle to Space ([gui] toggle_key), that
    // binding wins — otherwise every editor toggle would also cycle the mode.
    if editor.toggle_key == KeyCode::Space {
        return;
    }
    if keys.just_pressed(KeyCode::Space) {
        *mode = mode.next();
        info!("bava: drawing mode → {:?}", *mode);
    }
}

/// Album-art accent colors, smoothed across track changes.
///
/// `target` is the latest color set extracted from the cover; `current` chases it
/// so a song change eases the visualization's colors over a fraction of a second
/// instead of snapping. New balls spawn with whatever `current` is at spawn time,
/// so already-airborne balls keep their old color.
#[derive(Resource, Default)]
pub struct AlbumPalette {
    current: Option<Vec<Color>>,
    target: Option<Vec<Color>>,
}

/// Ease [`VisSettings::dynamic_fg`] toward the current album-art palette each
/// frame when [`VisSettings::dynamic_colors`] is on. Interpolates in Oklch so the
/// transition stays perceptually even; writes are change-guarded so a settled
/// palette doesn't mark `VisSettings` dirty every frame.
fn animate_album_colors(
    time: Res<Time>,
    art: Res<AlbumArt>,
    mut palette: ResMut<AlbumPalette>,
    mut vis: ResMut<VisSettings>,
) {
    if art.is_changed() {
        palette.target = art.colors.clone();
    }

    if !vis.dynamic_colors {
        if vis.dynamic_fg.is_some() {
            vis.dynamic_fg = None;
        }
        return;
    }

    let Some(target) = palette.target.clone() else {
        if vis.dynamic_fg.is_some() {
            vis.dynamic_fg = None;
        }
        return;
    };

    let current = palette.current.as_ref();
    // A near-zero fade snaps; otherwise ease exponentially with the configured tau.
    let f = if vis.dynamic_color_fade <= 1.0e-3 {
        1.0
    } else {
        1.0 - (-time.delta_secs() / vis.dynamic_color_fade).exp()
    };
    // Ease each stop toward its target; a stop the previous palette didn't have
    // (the new cover yields more colors) snaps in at the target.
    let mut next: Vec<Color> = target
        .iter()
        .enumerate()
        .map(|(i, &tgt)| {
            let from = current.and_then(|c| c.get(i)).copied().unwrap_or(tgt);
            mix_oklch(from, tgt, f)
        })
        .collect();
    // Snap once every stop is within a hair of its target, so the value settles
    // and the change-guard below stops re-marking VisSettings.
    if next.iter().zip(&target).all(|(c, t)| color_close(*c, *t)) {
        next = target;
    }

    palette.current = Some(next.clone());
    let next = Some(next);
    if vis.dynamic_fg != next {
        vis.dynamic_fg = next;
    }
}

/// Perceptually-even blend between two colors via Oklch.
fn mix_oklch(a: Color, b: Color, t: f32) -> Color {
    let a = Oklcha::from(a);
    let b = Oklcha::from(b);
    Color::Oklcha(a.mix(&b, t.clamp(0.0, 1.0)))
}

/// Whether two colors are within rounding distance in sRGB.
fn color_close(a: Color, b: Color) -> bool {
    let a = a.to_srgba();
    let b = b.to_srgba();
    (a.red - b.red).abs() + (a.green - b.green).abs() + (a.blue - b.blue).abs() < 0.004
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lum(c: Color) -> f32 {
        let l = c.to_linear();
        l.red + l.green + l.blue
    }

    #[test]
    fn drawing_mode_all_is_complete_and_unique() {
        assert_eq!(DrawingMode::ALL.len(), 11);
        // Every entry distinct.
        for (i, a) in DrawingMode::ALL.iter().enumerate() {
            for b in &DrawingMode::ALL[i + 1..] {
                assert_ne!(a, b, "duplicate mode {a:?}");
            }
        }
        // Splitter has no circle form → exactly one Splitter shape overall.
        let splitters = DrawingMode::ALL
            .iter()
            .filter(|m| m.shape() == VisShape::Splitter)
            .count();
        assert_eq!(splitters, 1);
    }

    #[test]
    fn drawing_mode_next_cycles_and_wraps() {
        let mut m = DrawingMode::ALL[0];
        let mut seen = vec![m];
        for _ in 0..DrawingMode::ALL.len() - 1 {
            m = m.next();
            seen.push(m);
        }
        assert_eq!(seen, DrawingMode::ALL.to_vec());
        // Wraps back to the start.
        assert_eq!(DrawingMode::ALL[10].next(), DrawingMode::ALL[0]);
    }

    #[test]
    fn drawing_mode_shape_and_family_mapping() {
        assert_eq!(DrawingMode::WaveBox.shape(), VisShape::Wave);
        assert_eq!(DrawingMode::WaveBox.family(), VisFamily::Box);
        assert_eq!(DrawingMode::BarsCircle.shape(), VisShape::Bars);
        assert_eq!(DrawingMode::BarsCircle.family(), VisFamily::Circle);
        // Box and circle share the shape but differ in family.
        for m in DrawingMode::ALL {
            assert_eq!(m.family() == VisFamily::Box, format!("{m:?}").ends_with("Box"));
        }
    }

    #[test]
    fn spread_monstercat_noop_when_factor_low() {
        let orig = vec![0.0, 1.0, 0.0, 0.2];
        let mut v = orig.clone();
        spread_monstercat(&mut v, 1.0);
        assert_eq!(v, orig);
        spread_monstercat(&mut v, 0.5);
        assert_eq!(v, orig);
    }

    #[test]
    fn spread_monstercat_lifts_neighbours_and_is_order_independent() {
        // A single spike should raise its neighbours by peak/factor^dist.
        let mut v = vec![0.0, 0.0, 1.0, 0.0, 0.0];
        spread_monstercat(&mut v, 2.0);
        assert!((v[2] - 1.0).abs() < 1e-6, "peak preserved");
        assert!((v[1] - 0.5).abs() < 1e-6, "dist 1 → 0.5");
        assert!((v[3] - 0.5).abs() < 1e-6);
        assert!((v[0] - 0.25).abs() < 1e-6, "dist 2 → 0.25");
        assert!((v[4] - 0.25).abs() < 1e-6);

        // Symmetric spikes give a symmetric result regardless of which is processed first.
        let mut a = vec![1.0, 0.0, 0.0, 0.0, 1.0];
        spread_monstercat(&mut a, 2.0);
        for i in 0..a.len() {
            assert!((a[i] - a[a.len() - 1 - i]).abs() < 1e-6, "asymmetry at {i}");
        }
    }

    #[test]
    fn spread_monstercat_matches_naive_pairwise_reference() {
        // The O(n) two-sweep must agree with the original O(n²) definition:
        // out[m] = max over z of src[z] / factor^|z-m|.
        fn naive(values: &mut [f32], factor: f32) {
            let src: Vec<f32> = values.to_vec();
            for (m, out) in values.iter_mut().enumerate() {
                for (z, &peak) in src.iter().enumerate() {
                    let dist = (z as i32 - m as i32).unsigned_abs() as f32;
                    let spread = peak / factor.powf(dist);
                    if spread > *out {
                        *out = spread;
                    }
                }
            }
        }
        // Deterministic pseudo-random spectrum (LCG), several factors.
        let mut seed = 0x2545F491u32;
        let src: Vec<f32> = (0..64)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        for factor in [1.1f32, 1.5, 2.0, 4.0] {
            let mut fast = src.clone();
            let mut slow = src.clone();
            spread_monstercat(&mut fast, factor);
            naive(&mut slow, factor);
            for i in 0..src.len() {
                assert!(
                    (fast[i] - slow[i]).abs() < 1e-4,
                    "factor {factor} bar {i}: fast {} vs naive {}",
                    fast[i],
                    slow[i]
                );
            }
        }
    }

    #[test]
    fn gradient_color_clamps_and_brightens_with_amplitude() {
        let lo = Color::srgb(0.1, 0.2, 0.3);
        let hi = Color::srgb(0.9, 0.8, 0.7);
        // Out-of-range t clamps to the endpoints.
        assert!((lum(gradient_color(lo, hi, -1.0, 1.8)) - lum(gradient_color(lo, hi, 0.0, 1.8))).abs() < 1e-5);
        assert!((lum(gradient_color(lo, hi, 2.0, 1.8)) - lum(gradient_color(lo, hi, 1.0, 1.8))).abs() < 1e-5);
        // Louder → brighter when glow is on.
        assert!(lum(gradient_color(lo, hi, 1.0, 1.8)) > lum(gradient_color(lo, hi, 0.0, 1.8)));
        // glow_gain 0 disables the HDR boost (still a valid color).
        let no_glow = gradient_color(lo, hi, 1.0, 0.0);
        assert!(no_glow.to_linear().red.is_finite());
    }

    #[test]
    fn active_profile_index_clamps_and_empty_is_safe() {
        let mut s = VisSettings::default();
        let red = Color::srgb(1.0, 0.0, 0.0);
        s.profiles = vec![
            ColorProfile { name: "a".into(), fg: vec![Color::BLACK], ..ColorProfile::default() },
            ColorProfile { name: "b".into(), fg: vec![red], ..ColorProfile::default() },
        ];
        s.active_profile = 99; // out of range → clamps to last ("b")
        assert_eq!(s.fg_lo(), red);

        s.profiles.clear();
        // Empty list → white fallback, never a panic.
        assert_eq!(s.fg_stops(), vec![Color::WHITE]);
    }

    #[test]
    fn fg_stops_fall_back_to_white_and_use_profile() {
        // Empty profile fg → a single white stop, and fg_lo/fg_hi both white.
        let mut s = VisSettings::default();
        s.profiles = vec![ColorProfile { fg: vec![], ..ColorProfile::default() }];
        assert_eq!(s.fg_stops(), vec![Color::WHITE]);
        assert_eq!(s.fg_lo(), Color::WHITE);
        assert_eq!(s.fg_hi(), Color::WHITE);

        // A two-stop profile drives the gradient ends.
        s.profiles = vec![ColorProfile {
            fg: vec![Color::BLACK, Color::WHITE],
            ..ColorProfile::default()
        }];
        assert_eq!(s.fg_lo(), Color::BLACK);
        assert_eq!(s.fg_hi(), Color::WHITE);
    }

    #[test]
    fn dynamic_color_count_clamps_active_stops() {
        let mut s = VisSettings::default();
        s.dynamic_colors = true;
        s.dynamic_fg = Some(vec![Color::BLACK, Color::WHITE, Color::srgb(1.0, 0.0, 0.0)]);
        // Count of 2 takes the first two extracted colors.
        s.dynamic_color_count = 2;
        assert_eq!(s.fg_stops().len(), 2);
        // Count of 3 takes all three.
        s.dynamic_color_count = 3;
        assert_eq!(s.fg_stops().len(), 3);
        // A count past what's available is clamped to the palette length.
        s.dynamic_color_count = 5;
        assert_eq!(s.fg_stops().len(), 3);
    }
}
