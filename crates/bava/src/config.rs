//! Configuration: a TOML file at `~/.config/bava/config.toml`, with CLI flags
//! (via clap) layered on top. Precedence is **CLI > config file > defaults**.
//!
//! The config file is created with default values on first run if it is missing.

use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::cava::CavaSettings;

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

impl Default for Config {
    fn default() -> Self {
        // Mirror the pipeline defaults so the generated file documents them.
        let s = CavaSettings::default();
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
}
