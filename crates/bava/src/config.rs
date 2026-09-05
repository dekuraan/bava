// SPDX-License-Identifier: MIT OR Apache-2.0
//! Configuration: a TOML file at `~/.config/bava/config.toml`, with CLI flags
//! (via clap) layered on top. Precedence is **CLI > config file > defaults**.
//!
//! The config file is created with default values on first run if it is missing.

use std::path::{Path, PathBuf};

use bevy::prelude::{Color, KeyCode, Resource, Vec2};
use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::cava::CavaSettings;
use crate::vis::physics::PhysicsSettings;
use crate::vis::{
    ColorProfile, Direction, DrawingMode, ImageLayer, MirrorMode, Theme, ToneMap, VisSettings,
};

/// Command-line arguments. Anything provided here overrides the config file.
#[derive(Parser, Debug)]
#[command(
    name = "bava",
    version,
    about = "A cavacore-driven Bevy music visualizer"
)]
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

    /// Render a music video offline: decode this audio file (mp3/flac/ogg/wav/
    /// m4a), drive the visualizer with it, and write the video to --out.
    #[arg(long, value_name = "AUDIO_FILE", requires = "out")]
    pub input: Option<PathBuf>,

    /// Output video file for --input (e.g. musicvideo.mp4).
    #[arg(long, value_name = "VIDEO_FILE", requires = "input")]
    pub out: Option<PathBuf>,

    /// Record without opening a window (`--headless` / `--headless=false`).
    /// Defaults to on when run non-interactively (stdout is not a terminal).
    #[arg(long, num_args = 0..=1, default_missing_value = "true", value_name = "BOOL")]
    pub headless: Option<bool>,

    /// Video framerate for --input (YouTube-friendly default).
    #[arg(long, default_value_t = 60)]
    pub fps: u32,

    /// Video width in pixels for --input.
    #[arg(long, default_value_t = 1920)]
    pub width: u32,

    /// Video height in pixels for --input.
    #[arg(long, default_value_t = 1080)]
    pub height: u32,

    /// Render only the first SECONDS of the track (for quick tests of --input).
    #[arg(long, value_name = "SECONDS")]
    pub duration: Option<f64>,

    /// Spawn N balls spread across the drawing area at launch, instead of
    /// starting empty and waiting for mouse clicks. Overrides
    /// `[physics] spawn_on_launch`.
    #[arg(long, value_name = "N")]
    pub spawn_balls: Option<usize>,
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
    /// When `source` is unset, follow the sink that is actively playing instead
    /// of the default sink (Linux only). A pinned `source` disables this.
    pub follow_active_sink: bool,
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
    /// HDR → display tone-mapping curve: `"none"`, `"reinhard"`,
    /// `"reinhard_luminance"`, `"aces_fitted"`, `"ag_x"`,
    /// `"somewhat_boring_display_transform"`, `"tony_mc_mapface"` or
    /// `"blender_filmic"`.
    pub tonemapping: ToneMap,
    /// HDR camera bloom intensity (0 = off, 0.25 = subtle glow).
    pub bloom_intensity: f32,
    /// HDR glow multiplier applied to loud bars (0 = no boost, 1.8 = default).
    pub glow_gain: f32,
    /// Derive the foreground gradient from the current track's album art instead
    /// of the active profile's colors. Off by default.
    pub dynamic_colors: bool,
    /// How many album-art colors to use for dynamic colors (2..=5).
    pub dynamic_color_count: usize,
    /// Crossfade time, in seconds, when dynamic colors change on a new track.
    pub dynamic_color_fade: f32,
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
    /// Minimum delay between right-click spray bursts while held, in milliseconds.
    pub spawn_debounce_ms: u64,
    /// Balls to spawn across the drawing area at launch (0 = start empty). Also
    /// what `--spawn-balls N` sets; the ball/trail simulation is bava's heaviest
    /// workload, so this is how you reproduce a loaded scene — for a benchmark or
    /// just to start with the playground already full.
    pub spawn_on_launch: usize,
    /// Spectrum-surface smoothing time constant, in seconds (larger = smoother).
    pub bar_smoothing: f32,
    /// Restitution of the spectrum surface.
    pub bar_restitution: f32,
    /// Launch gain: how strongly a rising surface flings balls along its normal.
    pub bar_push: f32,
    /// Planet mode: radial acceleration pulling balls toward the center, px/s².
    pub central_gravity: f32,
    /// Continuous collision detection for balls: stops very fast ones from
    /// passing through a bar or the floor. On by default. This is the most
    /// expensive part of the ball simulation, so turning it off is the biggest
    /// single physics saving if you run a lot of balls.
    pub ccd: bool,
    /// Draw a fading color trail behind each ball.
    pub trails: bool,
    /// Trail length: how many recent positions each trail keeps.
    pub trail_length: usize,
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
                follow_active_sink: cava.follow_active_sink,
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
                tonemapping: vis.tonemapping,
                bloom_intensity: vis.bloom_intensity,
                glow_gain: vis.glow_gain,
                dynamic_colors: vis.dynamic_colors,
                dynamic_color_count: vis.dynamic_color_count,
                dynamic_color_fade: vis.dynamic_color_fade,
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
                spawn_debounce_ms: physics.spawn_debounce_ms,
                spawn_on_launch: physics.spawn_on_launch,
                bar_smoothing: physics.bar_smoothing,
                bar_restitution: physics.bar_restitution,
                bar_push: physics.bar_push,
                central_gravity: physics.central_gravity,
                ccd: physics.ccd,
                trails: physics.trails,
                trail_length: physics.trail_length,
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
        store::config_dir().map(|d| d.join("bava").join("config.toml"))
    }

    /// Directory holding named profiles: `~/.config/bava/profiles/`.
    pub fn profiles_dir() -> Option<PathBuf> {
        store::config_dir().map(|d| d.join("bava").join("profiles"))
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
        let mut names = store::list_toml_stems(&dir);
        names.sort();
        names
    }

    /// Load a named profile, or `None` if it is missing or fails to parse.
    pub fn load_profile(name: &str) -> Option<Self> {
        let path = Self::profile_path(name)?;
        let text = store::read(&path).ok()?;
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

    /// Read and parse the config at `path`, or `None` if it is missing or does
    /// not parse. Used by the editor's "Reload", which wants to leave the live
    /// settings untouched on failure rather than reset them to defaults.
    pub fn load(path: &Path) -> Option<Self> {
        toml::from_str(&store::read(path).ok()?).ok()
    }

    /// Load the config at `path`, creating it with defaults if it doesn't exist.
    ///
    /// On a read error, logs a warning and falls back to defaults so the app
    /// always starts. On a *parse* error the broken file is moved aside to
    /// `<name>.bak` and a fresh default is written in its place, so a stale or
    /// hand-broken config self-heals instead of silently using defaults forever
    /// (the old contents stay recoverable in the backup).
    pub fn load_or_create(path: &Path) -> Self {
        match store::read(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    let backup = path.with_extension("toml.bak");
                    let where_to = match store::rename(path, &backup) {
                        Ok(()) => format!("backed up to {}", backup.display()),
                        Err(be) => {
                            eprintln!(
                                "bava: {} failed to parse ({e}); backup failed ({be}); leaving file untouched and using defaults",
                                path.display()
                            );
                            return Config::default();
                        }
                    };
                    eprintln!(
                        "bava: {} failed to parse ({e}); {where_to}, writing fresh defaults",
                        path.display()
                    );
                    let cfg = Config::default();
                    if let Err(we) = cfg.write(path) {
                        eprintln!(
                            "bava: could not write fresh config to {}: {we}",
                            path.display()
                        );
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
                eprintln!(
                    "bava: could not read {}: {e}; using defaults",
                    path.display()
                );
                Config::default()
            }
        }
    }

    /// Serialize and write the config to `path`, creating parent dirs.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let body = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let text = format!("# bava configuration\n# https://github.com/dekuraan/bava\n\n{body}");
        store::write(path, &text)
    }

    /// Apply CLI overrides in place. Precedence is CLI > config file > defaults.
    pub fn apply_cli(&mut self, cli: &Cli) {
        if let Some(source) = &cli.source {
            self.audio.source = Some(source.clone());
        }
        // These three feed buffer sizing / cavacore plan building and are used as
        // divisors downstream, so a zero would divide-by-zero or panic at startup.
        // Reject the override and keep the config/default value.
        if let Some(rate) = cli.rate {
            if rate == 0 {
                eprintln!("bava: --rate 0 is invalid; ignoring");
            } else {
                self.audio.rate = rate;
            }
        }
        if let Some(channels) = cli.channels {
            if channels == 0 || channels > 2 {
                eprintln!("bava: --channels {channels} is invalid (must be 1 or 2); ignoring");
            } else {
                self.audio.channels = channels;
            }
        }
        if let Some(frame_samples) = cli.frame_samples {
            if frame_samples == 0 {
                eprintln!("bava: --frame_samples 0 is invalid; ignoring");
            } else {
                self.audio.frame_samples = frame_samples;
            }
        }
        if let Some(bars) = cli.bars {
            if bars == 0 {
                eprintln!("bava: --bars 0 is invalid; ignoring");
            } else if bars > MAX_BARS_PER_CHANNEL {
                eprintln!(
                    "bava: --bars {bars} exceeds the maximum of {MAX_BARS_PER_CHANNEL}; clamping"
                );
                self.cava.bars_per_channel = MAX_BARS_PER_CHANNEL;
            } else {
                self.cava.bars_per_channel = bars;
            }
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
        if let Some(n) = cli.spawn_balls {
            self.physics.spawn_on_launch = n;
        }
    }

    /// Convert into the runtime [`CavaSettings`] resource.
    pub fn to_cava_settings(&self, debug: bool) -> CavaSettings {
        CavaSettings {
            // Clamp against a possibly hand-edited config: an unbounded
            // `bars_per_channel` sizes the Cava buffer and the per-bar mesh /
            // collider pools at startup (huge values OOM or overflow), and
            // cavacore requires exactly 1 or 2 channels.
            bars_per_channel: self.cava.bars_per_channel.clamp(1, MAX_BARS_PER_CHANNEL),
            channels: self.audio.channels.clamp(1, 2),
            // Same hand-edited-config hazard as bars: `rate` and
            // `frame_samples` size the capture ring buffer and per-read chunk
            // at startup, so unbounded values OOM (and rate feeds cavacore
            // validation, which rejects 0 / > 384 kHz with a dead vis).
            rate: self.audio.rate.clamp(8_000, 384_000),
            frame_samples: self.audio.frame_samples.clamp(16, 65_536),
            autosens: self.cava.autosens,
            noise_reduction: self.cava.noise_reduction,
            low_cutoff_freq: self.cava.low_cutoff_freq,
            high_cutoff_freq: self.cava.high_cutoff_freq,
            source: self.audio.source.clone(),
            follow_active_sink: self.audio.follow_active_sink,
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
            // Clamp against a possibly-stale / hand-edited config so a renderer
            // indexing `profiles[active_profile]` can't panic (`profiles` is
            // guaranteed non-empty above).
            active_profile: v.active_profile.min(profiles.len() - 1),
            profiles,
            background: ImageLayer::from(&v.background),
            foreground: ImageLayer::from(&v.foreground),
            tonemapping: v.tonemapping,
            bloom_intensity: v.bloom_intensity,
            glow_gain: v.glow_gain,
            dynamic_colors: v.dynamic_colors,
            dynamic_color_count: v
                .dynamic_color_count
                .clamp(2, crate::now_playing::MAX_DYNAMIC_COLORS),
            dynamic_color_fade: v.dynamic_color_fade.max(0.0),
            dynamic_fg: None,
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
            spawn_debounce_ms: p.spawn_debounce_ms,
            spawn_on_launch: p.spawn_on_launch,
            bar_smoothing: p.bar_smoothing,
            bar_restitution: p.bar_restitution,
            bar_push: p.bar_push,
            // Inward pull magnitude; negative values would make the orbit
            // launch speed `sqrt(central_gravity * r)` NaN (see `spawn_one_ball`).
            central_gravity: p.central_gravity.max(0.0),
            ccd: p.ccd,
            trails: p.trails,
            trail_length: p.trail_length,
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
/// Upper bound on `bars_per_channel`. The GUI slider already caps at 128; this
/// guards the CLI (`--bars`) and hand-edited config paths so an absurd value
/// can't OOM (millions of mesh/collider entities) or overflow the Cava buffer
/// allocation at startup. Generous — well above any sane visualizer use.
pub(crate) const MAX_BARS_PER_CHANNEL: usize = 1024;

const DEFAULT_TOGGLE_KEY: &str = "p";

/// Lower-case key name ⇄ [`KeyCode`] table for the configurable editor hotkey.
/// The first entry for a given [`KeyCode`] is its canonical name (used when
/// writing the config back); later duplicates are accepted aliases.
const KEY_NAMES: &[(&str, KeyCode)] = &[
    ("a", KeyCode::KeyA),
    ("b", KeyCode::KeyB),
    ("c", KeyCode::KeyC),
    ("d", KeyCode::KeyD),
    ("e", KeyCode::KeyE),
    ("f", KeyCode::KeyF),
    ("g", KeyCode::KeyG),
    ("h", KeyCode::KeyH),
    ("i", KeyCode::KeyI),
    ("j", KeyCode::KeyJ),
    ("k", KeyCode::KeyK),
    ("l", KeyCode::KeyL),
    ("m", KeyCode::KeyM),
    ("n", KeyCode::KeyN),
    ("o", KeyCode::KeyO),
    ("p", KeyCode::KeyP),
    ("q", KeyCode::KeyQ),
    ("r", KeyCode::KeyR),
    ("s", KeyCode::KeyS),
    ("t", KeyCode::KeyT),
    ("u", KeyCode::KeyU),
    ("v", KeyCode::KeyV),
    ("w", KeyCode::KeyW),
    ("x", KeyCode::KeyX),
    ("y", KeyCode::KeyY),
    ("z", KeyCode::KeyZ),
    ("0", KeyCode::Digit0),
    ("1", KeyCode::Digit1),
    ("2", KeyCode::Digit2),
    ("3", KeyCode::Digit3),
    ("4", KeyCode::Digit4),
    ("5", KeyCode::Digit5),
    ("6", KeyCode::Digit6),
    ("7", KeyCode::Digit7),
    ("8", KeyCode::Digit8),
    ("9", KeyCode::Digit9),
    ("f1", KeyCode::F1),
    ("f2", KeyCode::F2),
    ("f3", KeyCode::F3),
    ("f4", KeyCode::F4),
    ("f5", KeyCode::F5),
    ("f6", KeyCode::F6),
    ("f7", KeyCode::F7),
    ("f8", KeyCode::F8),
    ("f9", KeyCode::F9),
    ("f10", KeyCode::F10),
    ("f11", KeyCode::F11),
    ("f12", KeyCode::F12),
    ("backquote", KeyCode::Backquote),
    ("grave", KeyCode::Backquote),
    ("tilde", KeyCode::Backquote),
    ("tab", KeyCode::Tab),
    ("insert", KeyCode::Insert),
    ("space", KeyCode::Space),
    ("escape", KeyCode::Escape),
    ("esc", KeyCode::Escape),
    ("enter", KeyCode::Enter),
    ("return", KeyCode::Enter),
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

// --- Config store -----------------------------------------------------------
//
// Where configs and profiles live. Natively that is the filesystem under
// `~/.config/bava`; the browser has none, so the very same paths become
// `localStorage` keys. Both impls expose one path-shaped, synchronous API, which
// is what lets `Config` — and the settings editor's Save / Reload / Profiles —
// be the same code on every target.

/// Filesystem-backed store (every target except the web).
#[cfg(not(target_arch = "wasm32"))]
mod store {
    use std::path::{Path, PathBuf};

    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir()
    }

    pub fn read(path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    pub fn write(path: &Path, text: &str) -> std::io::Result<()> {
        use std::io::Write;

        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut pending = tempfile::NamedTempFile::new_in(parent)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            pending.as_file().set_permissions(metadata.permissions())?;
        }
        pending.write_all(text.as_bytes())?;
        pending.as_file().sync_all()?;
        pending.persist(path).map_err(|e| e.error)?;
        Ok(())
    }

    pub fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }

    /// `.toml` file stems directly inside `dir`.
    pub fn list_toml_stems(dir: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
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
            .collect()
    }
}

/// `localStorage`-backed store (the web). Settings persist per browser origin.
#[cfg(target_arch = "wasm32")]
mod store {
    use std::path::{Path, PathBuf};

    /// Prefix on every key we own, so a config can't collide with anything else
    /// the hosting page keeps in `localStorage`.
    const PREFIX: &str = "bava:";

    /// A synthetic root standing in for `~/.config`, so the paths built on top
    /// of it (`bava/config.toml`, `bava/profiles/x.toml`) read the same as the
    /// native ones — including in the editor's "Saved → …" status line.
    pub fn config_dir() -> Option<PathBuf> {
        Some(PathBuf::from("/config"))
    }

    fn key(path: &Path) -> String {
        format!("{PREFIX}{}", path.to_string_lossy())
    }

    /// `localStorage`, or `None` when the browser denies it (private-mode
    /// Safari, third-party-cookie blocking in an iframe). Callers degrade to
    /// in-memory defaults rather than failing to start.
    fn local_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    fn missing(what: &str) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::NotFound, what.to_string())
    }

    pub fn read(path: &Path) -> std::io::Result<String> {
        let storage = local_storage().ok_or_else(|| missing("localStorage unavailable"))?;
        storage
            .get_item(&key(path))
            .ok()
            .flatten()
            // A `NotFound` (rather than any other error) is what makes
            // `load_or_create` write fresh defaults instead of warning.
            .ok_or_else(|| missing("no such key"))
    }

    pub fn write(path: &Path, text: &str) -> std::io::Result<()> {
        let storage = local_storage().ok_or_else(|| missing("localStorage unavailable"))?;
        storage.set_item(&key(path), text).map_err(|_| {
            // The only realistic failure is the ~5 MB per-origin quota.
            std::io::Error::other("localStorage write rejected (quota exceeded?)")
        })
    }

    pub fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
        let text = read(from)?;
        write(to, &text)?;
        if let Some(storage) = local_storage() {
            let _ = storage.remove_item(&key(from));
        }
        Ok(())
    }

    /// Keys under `dir` that look like `<stem>.toml`, which is how the flat
    /// `localStorage` namespace models "files in a directory".
    pub fn list_toml_stems(dir: &Path) -> Vec<String> {
        let Some(storage) = local_storage() else {
            return Vec::new();
        };
        let dir_key = format!("{PREFIX}{}/", dir.to_string_lossy());
        let len = storage.length().unwrap_or(0);
        (0..len)
            .filter_map(|i| storage.key(i).ok().flatten())
            .filter_map(|k| {
                let rest = k.strip_prefix(&dir_key)?;
                // Only direct children: no nesting below the profiles dir.
                if rest.contains('/') {
                    return None;
                }
                rest.strip_suffix(".toml").map(str::to_string)
            })
            .collect()
    }
}

// --- CLI entry --------------------------------------------------------------

/// Parse the effective command line.
///
/// Natively that is `argv`. In the browser there is no argv, so the page's query
/// string stands in for it: `?bars=32&mode=wave-circle&gui` becomes
/// `--bars 32 --mode wave-circle --gui`. Both go through the same [`Cli`], so a
/// shareable URL overrides settings exactly like a flag does.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_cli() -> Cli {
    Cli::parse()
}

#[cfg(target_arch = "wasm32")]
pub fn parse_cli() -> Cli {
    let query = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    match Cli::try_parse_from(query_to_args(&query)) {
        Ok(cli) => cli,
        Err(e) => {
            // A bad query string must not take the whole page down: report it
            // and start with defaults (which the in-app editor can still
            // change). This runs before the `App` exists, so Bevy's `warn!`
            // would have no subscriber to write to and vanish — go straight to
            // the console.
            web_sys::console::warn_1(&format!("bava: ignoring query string {query:?}: {e}").into());
            Cli::try_parse_from(["bava"]).expect("bava: empty arg list must parse")
        }
    }
}

/// Query parameters the hosting page consumes itself, which must not reach
/// clap — an unknown flag fails the whole parse, so `?video=abc&bars=48` would
/// otherwise lose the `bars` override too. Keep in sync with `web/bava.js`.
#[cfg(target_arch = "wasm32")]
const PAGE_ONLY_PARAMS: &[&str] = &["video"];

/// `?bars=32&gui&mode=wave-circle` → `["bava", "--bars", "32", "--gui", …]`.
///
/// A bare key becomes a bare flag, which is what clap wants for the `bool`
/// options; a `key=value` pair becomes two arguments so values containing `=`
/// or spaces survive without quoting rules.
#[cfg(target_arch = "wasm32")]
fn query_to_args(query: &str) -> Vec<String> {
    let mut args = vec!["bava".to_string()];
    for pair in query.trim_start_matches('?').split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (pair, None),
        };
        let key = percent_decode(key);
        if key.is_empty() || PAGE_ONLY_PARAMS.contains(&key.as_str()) {
            continue;
        }
        args.push(format!("--{key}"));
        if let Some(value) = value {
            args.push(percent_decode(value));
        }
    }
    args
}

/// Minimal `application/x-www-form-urlencoded` decoding: `%XX` escapes and `+`
/// for space. Enough for the flags a shareable bava URL carries, and avoids
/// pulling a URL-parsing crate into the wasm build for it.
#[cfg(target_arch = "wasm32")]
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not a real escape ("100%" in a value); keep it literal.
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn failed_backup_preserves_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[vis\nmy irreplaceable settings";
        std::fs::write(&path, original).unwrap();
        std::fs::create_dir(path.with_extension("toml.bak")).unwrap();
        Config::load_or_create(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn malformed_config_is_backed_up_before_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[broken").unwrap();
        Config::load_or_create(&path);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
            "[broken"
        );
        assert!(Config::load(&path).is_some());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn atomic_save_replaces_existing_config_and_cleans_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.write(&path).unwrap();
        cfg.cava.bars_per_channel = 37;
        cfg.write(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap().cava.bars_per_channel, 37);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    fn srgba(c: Color) -> (u8, u8, u8, u8) {
        let s = c.to_srgba();
        (
            channel_to_u8(s.red),
            channel_to_u8(s.green),
            channel_to_u8(s.blue),
            channel_to_u8(s.alpha),
        )
    }

    #[test]
    fn hex_parsing_handles_each_length() {
        // #rgb shorthand → each nibble doubled.
        assert_eq!(srgba(hex_to_color("#f08").unwrap()), (255, 0, 136, 255));
        // #rrggbb opaque.
        assert_eq!(srgba(hex_to_color("#ff0000").unwrap()), (255, 0, 0, 255));
        // #aarrggbb with alpha.
        assert_eq!(srgba(hex_to_color("#80ff0000").unwrap()), (255, 0, 0, 128));
        // Leading '#' optional, whitespace trimmed.
        assert_eq!(srgba(hex_to_color("  00ff00 ").unwrap()), (0, 255, 0, 255));
    }

    #[test]
    fn hex_parsing_rejects_garbage() {
        assert!(hex_to_color("#xyz").is_none());
        assert!(hex_to_color("#12345").is_none()); // unsupported length
        assert!(hex_to_color("").is_none());
    }

    #[test]
    fn color_hex_round_trips() {
        for hex in ["#ff0000", "#00ff00", "#0000ff", "#123456", "#80abcdef"] {
            let c = hex_to_color(hex).unwrap();
            let back = hex_to_color(&color_to_hex(c)).unwrap();
            assert_eq!(srgba(c), srgba(back), "round-trip changed {hex}");
        }
        // Opaque colors serialize without the alpha pair.
        assert_eq!(color_to_hex(hex_to_color("#abcdef").unwrap()), "#abcdef");
        // Non-opaque keeps the alpha pair.
        assert!(color_to_hex(hex_to_color("#80abcdef").unwrap()).starts_with("#80"));
    }

    #[test]
    fn channel_to_u8_clamps_and_rounds() {
        assert_eq!(channel_to_u8(-1.0), 0);
        assert_eq!(channel_to_u8(2.0), 255);
        assert_eq!(channel_to_u8(0.5), 128); // 127.5 rounds up
    }

    #[test]
    fn key_names_parse_and_round_trip() {
        assert_eq!(parse_key("p"), Some(KeyCode::KeyP));
        assert_eq!(parse_key("  F3 "), Some(KeyCode::F3)); // case-insensitive, trimmed
        assert_eq!(parse_key("grave"), Some(KeyCode::Backquote)); // alias
        assert_eq!(parse_key("nope"), None);

        // Canonical name round-trips; aliases resolve to the canonical one.
        assert_eq!(key_to_name(KeyCode::KeyP), Some("p"));
        assert_eq!(key_to_name(KeyCode::Backquote), Some("backquote"));
        assert_eq!(
            parse_key(key_to_name(KeyCode::Space).unwrap()),
            Some(KeyCode::Space)
        );
    }

    #[test]
    fn sanitize_profile_name_strips_path_chars() {
        assert_eq!(sanitize_profile_name("  my profile "), "my profile");
        assert_eq!(sanitize_profile_name("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_profile_name("a/b\\c:d"), "abcd");
        assert!(sanitize_profile_name("///").is_empty());
    }

    #[test]
    fn profile_path_rejects_empty_after_sanitizing() {
        // A name that sanitizes to nothing yields no path (can't escape the dir).
        assert!(Config::profile_path("///").is_none());
    }

    #[test]
    fn config_toml_round_trips() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        // Spot-check fields across every section survive a serialize/parse cycle.
        assert_eq!(back.audio.rate, cfg.audio.rate);
        assert_eq!(back.cava.bars_per_channel, cfg.cava.bars_per_channel);
        assert_eq!(back.vis.mode, cfg.vis.mode);
        assert_eq!(back.vis.mirror, cfg.vis.mirror);
        assert_eq!(back.physics.enabled, cfg.physics.enabled);
        assert_eq!(back.gui.toggle_key, cfg.gui.toggle_key);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults() {
        // `#[serde(default)]` means a sparse file still parses, filling the rest.
        let cfg: Config = toml::from_str("[cava]\nbars_per_channel = 7\n").unwrap();
        assert_eq!(cfg.cava.bars_per_channel, 7);
        assert_eq!(cfg.audio.rate, AudioConfig::default().rate);
        assert_eq!(cfg.vis.mode, VisConfig::default().mode);
    }

    #[test]
    fn settings_round_trip_through_config() {
        let cava = CavaSettings::default();
        let vis = VisSettings::default();
        let physics = PhysicsSettings::default();
        let cfg = Config::from_settings(&cava, &vis, DrawingMode::BarsCircle, &physics);

        let cava_back = cfg.to_cava_settings(false);
        assert_eq!(cava_back.bars_per_channel, cava.bars_per_channel);
        assert_eq!(cava_back.rate, cava.rate);
        assert_eq!(cava_back.channels, cava.channels);

        let vis_back = cfg.to_vis_settings();
        assert_eq!(vis_back.mirror, vis.mirror);
        assert_eq!(vis_back.monstercat, vis.monstercat);
        assert_eq!(vis_back.direction, vis.direction);

        let phys_back = cfg.to_physics_settings();
        assert_eq!(phys_back.max_balls, physics.max_balls);
        assert_eq!(phys_back.central_gravity, physics.central_gravity);

        assert_eq!(cfg.vis_mode(), DrawingMode::BarsCircle);
    }

    #[test]
    fn gui_toggle_key_round_trips_and_falls_back() {
        let mut cfg = Config::default();
        cfg.set_gui_toggle_key(KeyCode::F5);
        assert_eq!(cfg.gui.toggle_key, "f5");
        assert_eq!(cfg.gui_toggle_key(), KeyCode::F5);

        // An unknown name falls back to the default rather than panicking.
        cfg.gui.toggle_key = "definitely-not-a-key".into();
        assert_eq!(cfg.gui_toggle_key(), parse_key(DEFAULT_TOGGLE_KEY).unwrap());
    }
}
