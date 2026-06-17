//! Configuration: a TOML file at `~/.config/bava/config.toml`, with CLI flags
//! (via clap) layered on top. Precedence is **CLI > config file > defaults**.
//!
//! The config file is created with default values on first run if it is missing.

use std::path::PathBuf;

use bevy::prelude::Color;
use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::cava::CavaSettings;
use crate::vis::physics::PhysicsSettings;
use crate::vis::{VisSettings, VisStyle};

/// Command-line arguments. Anything provided here overrides the config file.
#[derive(Parser, Debug)]
#[command(name = "bava", version, about = "A cavacore-driven Bevy music visualizer")]
pub struct Cli {
    /// Path to the config file (default: ~/.config/bava/config.toml).
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Capture source, a PulseAudio monitor name (e.g. `alsa_output.….monitor`).
    /// Overrides `[audio] source`.
    #[arg(long, value_name = "NAME")]
    pub source: Option<String>,

    /// Bars per channel. Overrides `[cava] bars_per_channel`.
    #[arg(long)]
    pub bars: Option<usize>,

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

/// `[vis]` — visualizer styling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VisConfig {
    /// Active visualizer: `"bars"` or `"circle"` (toggle live with space).
    pub style: String,
    /// Monstercat neighbour-spread factor (1.5 ≈ smooth waves, higher = tighter,
    /// `<= 1` disables).
    pub monstercat: f32,
    /// Mirror bars from the vertical center instead of the bottom.
    pub mirror: bool,
    /// Foreground gradient `[r, g, b]` (0..1) at low amplitude.
    pub color_low: [f32; 3],
    /// Foreground gradient `[r, g, b]` (0..1) at full amplitude.
    pub color_high: [f32; 3],
    /// Circle: fill the ring interior with a translucent blob.
    pub fill: bool,
    /// Outline thickness in pixels.
    pub line_width: f32,
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
    /// Bar-platform smoothing time constant, in seconds (larger = smoother).
    pub bar_smoothing: f32,
    /// Restitution of the bar platforms.
    pub bar_restitution: f32,
}

impl Default for Config {
    fn default() -> Self {
        // Mirror the pipeline/vis defaults so the generated file documents them.
        let s = CavaSettings::default();
        let v = VisSettings::default();
        let p = PhysicsSettings::default();
        let lo = v.color_lo.to_srgba();
        let hi = v.color_hi.to_srgba();
        Self {
            audio: AudioConfig {
                source: s.source,
                rate: s.rate,
                channels: s.channels,
                frame_samples: s.frame_samples,
            },
            cava: CavaConfig {
                bars_per_channel: s.bars_per_channel,
                autosens: s.autosens,
                noise_reduction: s.noise_reduction,
                low_cutoff_freq: s.low_cutoff_freq,
                high_cutoff_freq: s.high_cutoff_freq,
            },
            vis: VisConfig {
                style: "bars".into(),
                monstercat: v.monstercat,
                mirror: v.mirror,
                color_low: [lo.red, lo.green, lo.blue],
                color_high: [hi.red, hi.green, hi.blue],
                fill: v.fill,
                line_width: v.line_width,
            },
            physics: PhysicsConfig {
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
            },
        }
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
    /// Default config path: `~/.config/bava/config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("bava").join("config.toml"))
    }

    /// Load the config at `path`, creating it with defaults if it doesn't exist.
    ///
    /// On a read or parse error, logs a warning and falls back to defaults so the
    /// app always starts.
    pub fn load_or_create(path: &PathBuf) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("bava: failed to parse {}: {e}; using defaults", path.display());
                    Config::default()
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

    /// Apply CLI overrides in place.
    pub fn apply_cli(&mut self, cli: &Cli) {
        if let Some(source) = &cli.source {
            self.audio.source = Some(source.clone());
        }
        if let Some(bars) = cli.bars {
            self.cava.bars_per_channel = bars;
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
        let lo = self.vis.color_low;
        let hi = self.vis.color_high;
        VisSettings {
            monstercat: self.vis.monstercat,
            mirror: self.vis.mirror,
            color_lo: Color::srgb(lo[0], lo[1], lo[2]),
            color_hi: Color::srgb(hi[0], hi[1], hi[2]),
            fill: self.vis.fill,
            line_width: self.vis.line_width,
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
        }
    }

    /// The initial [`VisStyle`] from `[vis] style`.
    pub fn vis_style(&self) -> VisStyle {
        match self.vis.style.to_ascii_lowercase().as_str() {
            "circle" => VisStyle::Circle,
            _ => VisStyle::Bars,
        }
    }
}
