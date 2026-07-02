# bava

A cross-platform music visualizer built with [Bevy](https://bevyengine.org/),
driven by [cavacore](https://github.com/karlstav/cava) (the DSP engine from CAVA).
Loopback audio capture feeds the analyzer, which publishes smoothed frequency bars
into a Bevy resource every frame; visualizers read that resource and render in
real-time. Now-playing metadata and album art are pulled from the OS media session.

| Platform | Audio capture | Now-playing |
|---|---|---|
| Linux | Native PipeWire monitor (default; PulseAudio fallback) | MPRIS over D-Bus |
| Windows | WASAPI shared-mode loopback | GlobalSystemMediaTransportControls (GSMTC) |
| macOS 14.2+ | Core Audio process tap (no extra install) | MediaRemote adapter |

## Features

- **11 visualizer modes** — Space cycles: Bars, Levels, Particles, Spine, Wave,
  Splitter (box family) and Circle variants. All modes support mirror/direction
  and a configurable color theme with HDR bloom.
- **Physics mode** — avian2d rigid bodies react to the frequency bars; balls
  spawn, collide, and fade with color trails.
- **In-app settings editor** — press `p` (configurable) for a live egui overlay
  covering all vis, DSP, and color options. Changes apply instantly; DSP/source
  changes need an explicit Apply.
- **Album art + now-playing HUD** — title, artist, and album art as a dimmed
  full-window backdrop. Degrades gracefully when no player is active.
- **Config file + profiles** — `~/.config/bava/config.toml` (auto-created on
  first run), with named profile snapshots under `~/.config/bava/profiles/`.
  CLI flags override file values. Load a profile with `--profile NAME`.

## Build & run

Requires [Rust stable](https://rustup.rs/) **1.95 or newer** (Bevy 0.19). bava is
pure Rust — no C toolchain or FFTW is needed on any platform.

### Linux

```sh
# Arch/CachyOS
sudo pacman -S pipewire libpulse dbus libxkbcommon wayland libx11 vulkan-icd-loader

cargo run -p bava
# If the shell lacks a Wayland socket: WAYLAND_DISPLAY=wayland-1 cargo run -p bava
# Pure-PulseAudio host (no libpipewire link): cargo run -p bava --no-default-features
```

### Windows

```sh
cargo build --release -p bava
```

### macOS (14.2+)

```sh
# The mediaremote-adapter is required for now-playing metadata. Install it and
# set BAVA_MEDIAREMOTE_ADAPTER_DIR to its directory before running:
# https://github.com/ungive/mediaremote-adapter
export BAVA_MEDIAREMOTE_ADAPTER_DIR=/path/to/adapter
cargo run -p bava
```

The first launch prompts for the **Audio Recording** permission (needed for the
Core Audio process tap). Without it the app runs without audio capture.

### Tests

```sh
cargo test -p cavacore-rs        # DSP safety and correctness suite
cargo test -p bava --bin bava    # app unit/system/physics tests (headless)
```

## Workspace layout

```
crates/
  cavacore-rs/         # pure-Rust cavacore port (realfft); CavaConfig → CavaPlan; rigorous test suite
  bava/
    src/cava/          # CavaPlugin, Cava resource, capture thread, feed_cava system
    src/cava/capture/  # AudioCapture trait; backends: pipewire.rs / pulse.rs / wasapi.rs / coreaudio.rs
    src/now_playing/   # NowPlaying + AlbumArt resources; backends: linux / windows / macos
    src/vis/           # VisPlugin: all visualizer modes, HUD, physics, stroke mesh helpers
    src/gui/           # in-app settings editor (bevy_egui)
    src/config.rs      # config.toml ↔ runtime *Settings resources; CLI via clap
```

## Key bindings

| Key | Action |
|---|---|
| Space | Cycle visualizer mode |
| p | Toggle settings editor (configurable via `[gui] toggle_key`) |
| F3 | Toggle avian2d collider debug overlay |

## Extending

Every visualizer reads the same `Cava` resource:

```rust
fn my_vis(cava: Res<bava::cava::Cava>) {
    let mono  = cava.mono();   // &[f32]  average of left+right
    let left  = cava.left();   // &[f32]  left channel
    let right = cava.right();  // &[f32]  right channel (empty in mono mode)
    // drive meshes, transforms, materials, or shader uniforms from these
}
```

New visualizer plugins are independent: spawn their own camera/mesh entities and
add a system that reads `Cava`. The existing modes in `vis/` are self-contained
examples.

## License

bava is licensed under the **GNU General Public License v3.0 or later**
(GPL-3.0-or-later); see [`LICENSE`](LICENSE).

The `cavacore-rs` crate is a pure-Rust reimplementation of the analysis engine
from [karlstav/cava](https://github.com/karlstav/cava) (MIT), built on the
[`realfft`](https://crates.io/crates/realfft) FFT crate — no C or FFTW is
linked.
