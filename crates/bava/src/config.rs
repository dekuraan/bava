// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration: a TOML file at `~/.config/bava/config.toml`, with CLI flags
//! (via clap) layered on top. Precedence is **CLI > config file > defaults**.
//!
//! The config file is created with default values on first run if it is missing.

use std::path::PathBuf;

use bevy::prelude::{Color, KeyCode, Resource, Vec2};
use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::cava::CavaSettings;
use crate::vis::physics::PhysicsSettings;
use crate::vis::{
    ColorProfile, Direction, DrawingMode, ImageLayer, MirrorMode, Theme, VisSettings,
};

/// Command-line arguments. Anything provided here overrides the config file.
#[derive(Parser, Debug)]
#[command(name = "bava", version, about = "A cavacore-driven Bevy music visualizer")]
pub struct Cli {
    /// Path to the config file (default: ~/.config/bava/config.toml).
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Load a named saved profile (`~/.config/bava/profiles/<NAME>.toml`) as the
    /// base config before applying any other overrides.
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Open the settings editor on startup (also toggled live with the
    /// `[gui] toggle_key`, default `p`).
    #[arg(long)]
    pub gui: bool,

    /// Capture source, a PulseAudio monitor name (e.g. `alsa_output.….monitor`).
    /// Overrides `[audio] source`.
    #[arg(long, value_name = "NAME")]
    pub source: Option<String>,

    /// Capture sample rate in Hz. Overrides `[audio] rate`.
    #[arg(long)]
    pub rate: Option<u32>,

    /// Channels to capture (1 or 2). Overrides `[audio] channels`.
    #[arg(long)]
    pub channels: Option<usize>,

    /// Samples per channel per cavacore execution. Overrides `[audio] frame_samples`.
    #[arg(long)]
    pub frame_samples: Option<usize>,

    /// Bars per channel. Overrides `[cava] bars_per_channel`.
    #[arg(long)]
    pub bars: Option<usize>,

    /// Smoothing factor 0..1. Overrides `[cava] noise_reduction`.
    #[arg(long)]
    pub noise_reduction: Option<f64>,

    /// Low edge of the visualized band, Hz. Overrides `[cava] low_cutoff_freq`.
    #[arg(long)]
    pub low_cutoff: Option<u32>,

    /// High edge of the visualized band, Hz. Overrides `[cava] high_cutoff_freq`.
    #[arg(long)]
    pub high_cutoff: Option<u32>,

    /// Initial drawing mode. Overrides `[vis] mode`.
    #[arg(long, value_enum)]
    pub mode: Option<DrawingMode>,

    /// Mirroring behaviour. Overrides `[vis] mirror`.
    #[arg(long, value_enum)]
    pub mirror: Option<MirrorMode>,

    /// Monstercat neighbour-spread factor. Overrides `[vis] monstercat`.
    #[arg(long)]
    pub monstercat: Option<f32>,

    /// Log input/output signal levels about once per second.
    #[arg(long)]
    pub debug: bool,

    /// Print the resolved configuration and exit.
    #[arg(long)]
    pub print_config: bool,
}

/// Top-level config file model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub audio: AudioConfig,
    pub cava: CavaConfig,
    pub vis: VisConfig,
    pub physics: PhysicsConfig,
    pub gui: GuiConfig,
}

/// `[gui]` — settings-editor preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiConfig {
    /// Key that toggles the settings editor. Accepts a single letter (`"p"`,
    /// `"a".."z"`), a digit, a function key (`"f1".."f12"`), or a named key
    /// (`"backquote"`/`"grave"`, `"tab"`, `"space"`, `"escape"`, …). Unknown
    /// names fall back to the default.
    pub toggle_key: String,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            toggle_key: DEFAULT_TOGGLE_KEY.into(),
        }
    }
}

/// `[audio]` — capture parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Capture source. When unset, bava records the default sink's monitor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Capture sample rate (Hz).
    pub rate: u32,
    /// Channels to capture (1 or 2).
    pub channels: usize,
    /// Samples per channel read per cavacore execution.
    pub frame_samples: usize,
}

/// `[cava]` — analysis parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CavaConfig {
    /// Bars per channel.
    pub bars_per_channel: usize,
    /// Auto-scale output into 0..1.
    pub autosens: bool,
    /// Smoothing factor 0..1 (cavacore recommends 0.77).
    pub noise_reduction: f64,
    /// Low edge of the visualized band (Hz).
    pub low_cutoff_freq: u32,
    /// High edge of the visualized band (Hz).
    pub high_cutoff_freq: u32,
}

/// `[vis]` — visualizer modes, geometry, colors and pictures. Mirrors the
/// [Cavalier](https://github.com/NickvisionApps/Cavalier) option set so the
/// config is forward-compatible with every drawing mode, even those not yet
/// rendered. Colors are ARGB/RGB hex strings (`"#rrggbb"` or `"#aarrggbb"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VisConfig {
    /// Active drawing mode (one of Cavalier's 11; toggle live with space).
    pub mode: DrawingMode,
    /// Monstercat neighbour-spread factor (1.5 ≈ smooth waves, higher = tighter,
    /// `<= 1` disables). bava-specific smoothing.
    pub monstercat: f32,
    /// Mirroring: `"off"`, `"full"` or `"split_channels"`.
    pub mirror: MirrorMode,
    /// Flip which side the mirrored copy is drawn on.
    pub reverse_mirror: bool,
    /// Box orientation / circle gradient direction.
    pub direction: Direction,
    /// Reverse bar order before drawing.
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
    /// Proportional shift of the draw region `[x, y]`.
    pub area_offset: [f32; 2],
    /// Index of the active color profile.
    pub active_profile: usize,
    /// Color schemes. A single fg/bg color is solid; two or more form a gradient.
    #[serde(rename = "profile")]
    pub profiles: Vec<ColorProfileConfig>,
    /// Background picture overlay.
    pub background: ImageConfig,
    /// Foreground picture overlay (masked by the visualization shape).
    pub foreground: ImageConfig,
}

/// `[[vis.profile]]` — a named color scheme.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorProfileConfig {
    /// Display name.
    pub name: String,
    /// Light/dark hint: `"light"` or `"dark"`.
    pub theme: Theme,
    /// Foreground color stops as hex strings.
    pub fg: Vec<String>,
    /// Background color stops as hex strings.
    pub bg: Vec<String>,
}

impl Default for ColorProfileConfig {
    fn default() -> Self {
        ColorProfileConfig::from(&ColorProfile::default())
    }
}

/// `[vis.background]` / `[vis.foreground]` — a picture overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    /// Image file to draw, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Scale multiplier applied to the source image.
    pub scale: f32,
    /// Opacity in `0..1`.
    pub alpha: f32,
}

impl Default for ImageConfig {
    fn default() -> Self {
        ImageConfig::from(&ImageLayer::default())
    }
}

/// `[physics]` — ball/bar simulation tunables.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PhysicsConfig {
    /// Enable the physics playground (click to spawn balls, bars bounce them).
    pub enabled: bool,
    /// Downward acceleration in px/s² (Box mode). ~980 ≈ earth at 100 px/m.
    pub gravity: f32,
    /// Default ball restitution (bounciness), 0..1.
    pub restitution: f32,
    /// Default ball air resistance (linear damping).
    pub air_resistance: f32,
    /// Default ball mass.
    pub mass: f32,
    /// Default ball radius, in pixels.
    pub radius: f32,
    /// Maximum live balls; oldest are evicted past this.
    pub max_balls: usize,
    /// Randomize each spawned ball's properties around the defaults.
    pub randomize: bool,
    /// Spectrum-surface smoothing time constant, in seconds (larger = smoother).
    pub bar_smoothing: f32,
    /// Restitution of the spectrum surface.
    pub bar_restitution: f32,
    /// Launch gain: how strongly a rising surface flings balls along its normal.
    pub bar_push: f32,
    /// Planet mode: radial acceleration pulling balls toward the center, px/s².
    pub central_gravity: f32,
    /// Draw the avian collider wireframes (toggle at runtime with F3).
    pub debug_draw: bool,
}

impl Default for Config {
    fn default() -> Self {
        // Mirror the pipeline/vis defaults so the generated file documents them.
        Config::from_settings(
            &CavaSettings::default(),
            &VisSettings::default(),
            DrawingMode::default(),
            &PhysicsSettings::default(),
        )
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Config::default().audio
    }
}

impl Default for CavaConfig {
    fn default() -> Self {
        Config::default().cava
    }
}

impl Default for VisConfig {
    fn default() -> Self {
        Config::default().vis
    }
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Config::default().physics
    }
}

impl Config {
    /// Build a config file model from the live runtime settings, so the editor
    /// can serialize the current in-app state back to TOML. Inverse of
    /// [`to_cava_settings`](Self::to_cava_settings) / [`to_vis_settings`](Self::to_vis_settings).
    pub fn from_settings(
        cava: &CavaSettings,
        vis: &VisSettings,
        mode: DrawingMode,
        physics: &PhysicsSettings,
    ) -> Self {
        Self {
            audio: AudioConfig {
                source: cava.source.clone(),
                rate: cava.rate,
                channels: cava.channels,
                frame_samples: cava.frame_samples,
            },
            cava: CavaConfig {
                bars_per_channel: cava.bars_per_channel,
                autosens: cava.autosens,
                noise_reduction: cava.noise_reduction,
                low_cutoff_freq: cava.low_cutoff_freq,
                high_cutoff_freq: cava.high_cutoff_freq,
            },
            vis: VisConfig {
                mode,
                monstercat: vis.monstercat,
                mirror: vis.mirror,
                reverse_mirror: vis.reverse_mirror,
                direction: vis.direction,
                reverse_order: vis.reverse_order,
                filling: vis.filling,
                line_thickness: vis.line_thickness,
                items_offset: vis.items_offset,
                items_roundness: vis.items_roundness,
                hearts: vis.hearts,
                inner_radius: vis.inner_radius,
                rotation: vis.rotation,
                area_margin: vis.area_margin,
                area_offset: vis.area_offset.to_array(),
                active_profile: vis.active_profile,
                profiles: vis.profiles.iter().map(ColorProfileConfig::from).collect(),
                background: ImageConfig::from(&vis.background),
                foreground: ImageConfig::from(&vis.foreground),
            },
            physics: PhysicsConfig {
                enabled: physics.enabled,
                gravity: physics.gravity,
                restitution: physics.restitution,
                air_resistance: physics.air_resistance,
                mass: physics.mass,
                radius: physics.radius,
                max_balls: physics.max_balls,
                randomize: physics.randomize,
                bar_smoothing: physics.bar_smoothing,
                bar_restitution: physics.bar_restitution,
                bar_push: physics.bar_push,
                central_gravity: physics.central_gravity,
                debug_draw: physics.debug_draw,
            },
            // The editor hotkey isn't derived from the runtime settings; callers
            // that have a live key (the editor's "Save") override it afterward
            // via [`set_gui_toggle_key`](Self::set_gui_toggle_key).
            gui: GuiConfig::default(),
        }
    }

    /// The settings-editor toggle key, parsed from `[gui] toggle_key` (falling
    /// back to the default on an unknown name).
    pub fn gui_toggle_key(&self) -> KeyCode {
        parse_key(&self.gui.toggle_key).unwrap_or_else(|| {
            eprintln!(
                "bava: unknown [gui] toggle_key {:?}; using {DEFAULT_TOGGLE_KEY:?}",
                self.gui.toggle_key
            );
            parse_key(DEFAULT_TOGGLE_KEY).expect("default toggle key must parse")
        })
    }

    /// Store `key` back into `[gui] toggle_key` as a name, so the editor's
    /// "Save" round-trips the current hotkey instead of resetting it.
    pub fn set_gui_toggle_key(&mut self, key: KeyCode) {
        self.gui.toggle_key = key_to_name(key).unwrap_or(DEFAULT_TOGGLE_KEY).to_string();
    }

    /// Default config path: `~/.config/bava/config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("bava").join("config.toml"))
    }

    /// Directory holding named profiles: `~/.config/bava/profiles/`.
    pub fn profiles_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("bava").join("profiles"))
    }

    /// File path for a named profile, with the name sanitized to a bare stem so
    /// it can't escape the profiles directory.
    pub fn profile_path(name: &str) -> Option<PathBuf> {
        let stem = sanitize_profile_name(name);
        if stem.is_empty() {
            return None;
        }
        Self::profiles_dir().map(|d| d.join(format!("{stem}.toml")))
    }

    /// Names of all saved profiles (`.toml` stems), sorted. Empty if the
    /// directory is missing or unreadable.
    pub fn list_profiles() -> Vec<String> {
        let Some(dir) = Self::profiles_dir() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| {
                let path = e.ok()?.path();
                if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        names.sort();
        names
    }

    /// Load a named profile, or `None` if it is missing or fails to parse.
    pub fn load_profile(name: &str) -> Option<Self> {
        let path = Self::profile_path(name)?;
        let text = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&text).ok()
    }

    /// Save this config as a named profile under [`profiles_dir`](Self::profiles_dir).
    pub fn save_profile(&self, name: &str) -> std::io::Result<PathBuf> {
        let path = Self::profile_path(name).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid profile name")
        })?;
        self.write(&path)?;
        Ok(path)
    }

    /// Load the config at `path`, creating it with defaults if it doesn't exist.
    ///
    /// On a read error, logs a warning and falls back to defaults so the app
    /// always starts. On a *parse* error the broken file is moved aside to
    /// `<name>.bak` and a fresh default is written in its place, so a stale or
    /// hand-broken config self-heals instead of silently using defaults forever
    /// (the old contents stay recoverable in the backup).
    pub fn load_or_create(path: &PathBuf) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    let backup = path.with_extension("toml.bak");
                    let where_to = match std::fs::rename(path, &backup) {
                        Ok(()) => format!("backed up to {}", backup.display()),
                        Err(be) => format!("could not back it up: {be}"),
                    };
                    eprintln!(
                        "bava: {} failed to parse ({e}); {where_to}, writing fresh defaults",
                        path.display()
                    );
                    let cfg = Config::default();
                    if let Err(we) = cfg.write(path) {
                        eprintln!("bava: could not write fresh config to {}: {we}", path.display());
                    }
                    cfg
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                if let Err(e) = cfg.write(path) {
                    eprintln!("bava: could not create {}: {e}", path.display());
                } else {
                    eprintln!("bava: wrote default config to {}", path.display());
                }
                cfg
            }
            Err(e) => {
                eprintln!("bava: could not read {}: {e}; using defaults", path.display());
                Config::default()
            }
        }
    }

    /// Serialize and write the config to `path`, creating parent dirs.
    pub fn write(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let text = format!("# bava configuration\n# https://github.com/dekuraan/bava\n\n{body}");
        std::fs::write(path, text)
    }

    /// Apply CLI overrides in place. Precedence is CLI > config file > defaults.
    pub fn apply_cli(&mut self, cli: &Cli) {
        if let Some(source) = &cli.source {
            self.audio.source = Some(source.clone());
        }
        if let Some(rate) = cli.rate {
            self.audio.rate = rate;
        }
        if let Some(channels) = cli.channels {
            self.audio.channels = channels;
        }
        if let Some(frame_samples) = cli.frame_samples {
            self.audio.frame_samples = frame_samples;
        }
        if let Some(bars) = cli.bars {
            self.cava.bars_per_channel = bars;
        }
        if let Some(nr) = cli.noise_reduction {
            self.cava.noise_reduction = nr;
        }
        if let Some(low) = cli.low_cutoff {
            self.cava.low_cutoff_freq = low;
        }
        if let Some(high) = cli.high_cutoff {
            self.cava.high_cutoff_freq = high;
        }
        if let Some(mode) = cli.mode {
            self.vis.mode = mode;
        }
        if let Some(mirror) = cli.mirror {
            self.vis.mirror = mirror;
        }
        if let Some(monstercat) = cli.monstercat {
            self.vis.monstercat = monstercat;
        }
    }

    /// Convert into the runtime [`CavaSettings`] resource.
    pub fn to_cava_settings(&self, debug: bool) -> CavaSettings {
        CavaSettings {
            bars_per_channel: self.cava.bars_per_channel,
            channels: self.audio.channels,
            rate: self.audio.rate,
            frame_samples: self.audio.frame_samples,
            autosens: self.cava.autosens,
            noise_reduction: self.cava.noise_reduction,
            low_cutoff_freq: self.cava.low_cutoff_freq,
            high_cutoff_freq: self.cava.high_cutoff_freq,
            source: self.audio.source.clone(),
            debug,
        }
    }

    /// Convert into the runtime [`VisSettings`] resource.
    pub fn to_vis_settings(&self) -> VisSettings {
        let v = &self.vis;
        let mut profiles: Vec<ColorProfile> = v.profiles.iter().map(ColorProfile::from).collect();
        if profiles.is_empty() {
            profiles.push(ColorProfile::default());
        }
        VisSettings {
            monstercat: v.monstercat,
            mirror: v.mirror,
            reverse_mirror: v.reverse_mirror,
            direction: v.direction,
            reverse_order: v.reverse_order,
            filling: v.filling,
            line_thickness: v.line_thickness,
            items_offset: v.items_offset,
            items_roundness: v.items_roundness,
            hearts: v.hearts,
            inner_radius: v.inner_radius,
            rotation: v.rotation,
            area_margin: v.area_margin,
            area_offset: Vec2::from(v.area_offset),
            active_profile: v.active_profile,
            profiles,
            background: ImageLayer::from(&v.background),
            foreground: ImageLayer::from(&v.foreground),
        }
    }

    /// Convert into the runtime [`PhysicsSettings`] resource.
    pub fn to_physics_settings(&self) -> PhysicsSettings {
        let p = &self.physics;
        PhysicsSettings {
            enabled: p.enabled,
            gravity: p.gravity,
            restitution: p.restitution,
            air_resistance: p.air_resistance,
            mass: p.mass,
            radius: p.radius,
            max_balls: p.max_balls,
            randomize: p.randomize,
            bar_smoothing: p.bar_smoothing,
            bar_restitution: p.bar_restitution,
            bar_push: p.bar_push,
            central_gravity: p.central_gravity,
            debug_draw: p.debug_draw,
        }
    }

    /// The initial [`DrawingMode`] from `[vis] mode`.
    pub fn vis_mode(&self) -> DrawingMode {
        self.vis.mode
    }
}

/// The active config file location, kept as a resource so the in-app editor can
/// save/reload to the same path the app launched with.
#[derive(Resource, Clone, Debug)]
pub struct ConfigHandle {
    /// Path of the main `config.toml` this session reads and writes.
    pub path: PathBuf,
}

/// Default settings-editor toggle key name. Must appear in [`KEY_NAMES`].
const DEFAULT_TOGGLE_KEY: &str = "p";

/// Lower-case key name ⇄ [`KeyCode`] table for the configurable editor hotkey.
/// The first entry for a given [`KeyCode`] is its canonical name (used when
/// writing the config back); later duplicates are accepted aliases.
const KEY_NAMES: &[(&str, KeyCode)] = &[
    ("a", KeyCode::KeyA), ("b", KeyCode::KeyB), ("c", KeyCode::KeyC), ("d", KeyCode::KeyD),
    ("e", KeyCode::KeyE), ("f", KeyCode::KeyF), ("g", KeyCode::KeyG), ("h", KeyCode::KeyH),
    ("i", KeyCode::KeyI), ("j", KeyCode::KeyJ), ("k", KeyCode::KeyK), ("l", KeyCode::KeyL),
    ("m", KeyCode::KeyM), ("n", KeyCode::KeyN), ("o", KeyCode::KeyO), ("p", KeyCode::KeyP),
    ("q", KeyCode::KeyQ), ("r", KeyCode::KeyR), ("s", KeyCode::KeyS), ("t", KeyCode::KeyT),
    ("u", KeyCode::KeyU), ("v", KeyCode::KeyV), ("w", KeyCode::KeyW), ("x", KeyCode::KeyX),
    ("y", KeyCode::KeyY), ("z", KeyCode::KeyZ),
    ("0", KeyCode::Digit0), ("1", KeyCode::Digit1), ("2", KeyCode::Digit2),
    ("3", KeyCode::Digit3), ("4", KeyCode::Digit4), ("5", KeyCode::Digit5),
    ("6", KeyCode::Digit6), ("7", KeyCode::Digit7), ("8", KeyCode::Digit8),
    ("9", KeyCode::Digit9),
    ("f1", KeyCode::F1), ("f2", KeyCode::F2), ("f3", KeyCode::F3), ("f4", KeyCode::F4),
    ("f5", KeyCode::F5), ("f6", KeyCode::F6), ("f7", KeyCode::F7), ("f8", KeyCode::F8),
    ("f9", KeyCode::F9), ("f10", KeyCode::F10), ("f11", KeyCode::F11), ("f12", KeyCode::F12),
    ("backquote", KeyCode::Backquote), ("grave", KeyCode::Backquote), ("tilde", KeyCode::Backquote),
    ("tab", KeyCode::Tab),
    ("space", KeyCode::Space),
    ("escape", KeyCode::Escape), ("esc", KeyCode::Escape),
    ("enter", KeyCode::Enter), ("return", KeyCode::Enter),
    ("backslash", KeyCode::Backslash),
    ("minus", KeyCode::Minus),
    ("equal", KeyCode::Equal),
    ("comma", KeyCode::Comma),
    ("period", KeyCode::Period),
    ("slash", KeyCode::Slash),
    ("semicolon", KeyCode::Semicolon),
];

/// Parse a key name (case-insensitive) into a [`KeyCode`], or `None` if unknown.
fn parse_key(name: &str) -> Option<KeyCode> {
    let n = name.trim().to_ascii_lowercase();
    KEY_NAMES.iter().find(|(k, _)| *k == n).map(|(_, kc)| *kc)
}

/// The canonical name for a [`KeyCode`], or `None` if it isn't in [`KEY_NAMES`].
fn key_to_name(key: KeyCode) -> Option<&'static str> {
    KEY_NAMES.iter().find(|(_, kc)| *kc == key).map(|(k, _)| *k)
}

/// Reduce a user-supplied profile name to a safe single-segment file stem:
/// trims whitespace and keeps only alphanumerics, space, `-` and `_`.
fn sanitize_profile_name(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .collect()
}

// --- DTO ⇄ runtime conversions (hex colors, image layers) -------------------

impl From<&ColorProfile> for ColorProfileConfig {
    fn from(p: &ColorProfile) -> Self {
        Self {
            name: p.name.clone(),
            theme: p.theme,
            fg: p.fg.iter().map(|c| color_to_hex(*c)).collect(),
            bg: p.bg.iter().map(|c| color_to_hex(*c)).collect(),
        }
    }
}

impl From<&ColorProfileConfig> for ColorProfile {
    fn from(c: &ColorProfileConfig) -> Self {
        Self {
            name: c.name.clone(),
            theme: c.theme,
            fg: c.fg.iter().filter_map(|s| hex_to_color(s)).collect(),
            bg: c.bg.iter().filter_map(|s| hex_to_color(s)).collect(),
        }
    }
}

impl From<&ImageLayer> for ImageConfig {
    fn from(l: &ImageLayer) -> Self {
        Self {
            path: l.path.clone(),
            scale: l.scale,
            alpha: l.alpha,
        }
    }
}

impl From<&ImageConfig> for ImageLayer {
    fn from(c: &ImageConfig) -> Self {
        Self {
            path: c.path.clone(),
            scale: c.scale,
            alpha: c.alpha,
        }
    }
}

/// Parse a `"#rgb"` / `"#rrggbb"` / `"#aarrggbb"` hex string into a [`Color`].
/// Returns `None` on malformed input (the stop is then skipped).
fn hex_to_color(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    let (a, r, g, b) = match h.len() {
        // `#rgb` shorthand: each nibble is doubled (`f08` → `ff0088`).
        3 => (255u8, nib(h, 0)?, nib(h, 1)?, nib(h, 2)?),
        6 => (255u8, u8h(h, 0)?, u8h(h, 2)?, u8h(h, 4)?),
        8 => (u8h(h, 0)?, u8h(h, 2)?, u8h(h, 4)?, u8h(h, 6)?),
        _ => return None,
    };
    Some(Color::srgba(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ))
}

/// Parse two hex digits at byte offset `i`.
fn u8h(h: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(h.get(i..i + 2)?, 16).ok()
}

/// Parse one hex digit at byte offset `i` and expand it to a full byte
/// (`f` → `0xff`), for `#rgb` shorthand.
fn nib(h: &str, i: usize) -> Option<u8> {
    let v = u8::from_str_radix(h.get(i..i + 1)?, 16).ok()?;
    Some(v * 17)
}

/// Quantize a `0.0..=1.0` color channel to a `0..=255` byte (clamped, rounded).
/// Shared by the hex writer here and the editor's egui color swatches.
pub(crate) fn channel_to_u8(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Format a [`Color`] as `"#rrggbb"`, or `"#aarrggbb"` when not fully opaque.
fn color_to_hex(c: Color) -> String {
    let s = c.to_srgba();
    let (r, g, b, a) = (
        channel_to_u8(s.red),
        channel_to_u8(s.green),
        channel_to_u8(s.blue),
        channel_to_u8(s.alpha),
    );
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{a:02x}{r:02x}{g:02x}{b:02x}")
    }
}
