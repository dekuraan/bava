# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Linux music visualizer: PulseAudio monitor capture → cavacore analysis → a Bevy `Cava` resource → visualizers, with MPRIS now-playing + album art. Cargo workspace, Bevy **0.18.1**, edition 2024.

## Commands

```sh
cargo run -p bava            # run the app (needs a Wayland/X11 display in env)
cargo build --release -p bava
cargo test -p cavacore-rs    # the rigorous cava safety/DSP suite
cargo test -p cavacore-rs --test dsp noise_reduction_slows_decay  # single test
```

Running needs the display env this shell may lack: `WAYLAND_DISPLAY=wayland-1 cargo run -p bava` (check `ls /run/user/1000/wayland-*` for the socket — it is **not** `wayland-0`).

## Crates

- `crates/cavacore-sys` — FFI to vendored upstream `cavacore.c/.h` (in `vendor/cava/`); `build.rs` compiles it with `cc` and links `fftw3` + `m`. Hand-written bindings mirroring `struct cava_plan`.
- `crates/cavacore-rs` — safe `CavaConfig → CavaPlan` wrapper. `CavaPlan` is `Send` but **not `Sync`**; FFTW planning is serialized behind a process-wide lock. Tests live in `tests/` (`validation.rs`, `dsp.rs`, shared signal gen in `tests/common/`).
- `crates/bava` — the Bevy app. Three plugins, each in its own module: `cava` (capture + analysis), `mpris` (metadata + art), `vis` (visualizers + HUD).

## Architecture notes (non-obvious)

- **cava runs on the Bevy main thread, per frame.** A capture thread (`cava/capture/pulse.rs`) only reads audio into a ring buffer; the `feed_cava` system drains it and runs `cava_execute`. `CavaState` (holds `CavaPlan`) is a **NonSend resource** because `CavaPlan` is `!Sync`.
- **Feed cavacore *fixed-size* chunks, never "all available."** cavacore's framerate estimate / autosens assume a steady `new_samples` per call; variable counts stall the autosens ramp and bars stay at zero. `frame_samples` sets the chunk (and thus cava's update rate).
- **Visualizers share the `Cava` resource** (`left()`/`right()`/`mono()`). `vis::DrawingMode` (Space cycles) is the live-toggled resource for the active mode; it mirrors Cavalier's 11 modes (`VisShape` × `VisFamily`, Splitter box-only). Only two renderers exist so far — `bars.rs` stands in for every `VisFamily::Box` mode, `circle.rs` for every `VisFamily::Circle` mode — but `vis::VisSettings` + the `[vis]` config already cover the full Cavalier option set (mirror/direction/theme, color profiles, fg/bg image layers). Renderers reuse `spread_monstercat` / `gradient_color` from `vis/mod.rs`. The circle fill is a per-frame-updated triangle-fan `Mesh2d`; the ring outline is gizmos.
- **Config**: `~/.config/bava/config.toml` (auto-created), CLI via clap layered on top (CLI > file > defaults). `config.rs` is the single source mapping `[audio]/[cava]/[vis]` → the runtime `*Settings` resources, inserted in `main.rs` before the plugins. `Config::from_settings` is the inverse (live resources → TOML), used by the editor to save. **Profiles** are named full-config snapshots under `~/.config/bava/profiles/<name>.toml` (`Config::{list,load,save}_profile`); load one at startup with `--profile NAME`.
- **Settings editor** (`gui/mod.rs`, `bevy_egui` 0.39): a floating egui window toggled with the `` ` `` (backquote) key — or `--gui` to start open, X / Esc to close. Its system runs in the `EguiPrimaryContextPass` schedule. It edits the `VisSettings` / `DrawingMode` resources **live** (reflected the same frame). Audio/DSP edits set [`CavaRebuild`](crate::cava::CavaRebuild): the `rebuild_cava` system rebuilds the `CavaPlan` on an explicit "Apply" press, keeping the running capture thread's rate/channels (those + source are restart-only). `EditorState.capture_keyboard` mirrors `ctx.wants_keyboard_input()` so `cycle_mode` (Space) suppresses itself while a text field has focus. egui auto-attaches its primary context to the first `Camera2d` (spawned in `bars.rs`).

## Bevy 0.18 gotchas

- Buffered events are **Messages**: `AppExit` is read with `MessageReader`, not `EventReader`.
- 2D mesh/material types come from `bevy::mesh` (`Indices`, `PrimitiveTopology`) and `bevy::prelude` (`Mesh2d`, `ColorMaterial`, `MeshMaterial2d`); `RenderAssetUsages` from `bevy::asset`.
- When recommending any Bevy API, verify it against 0.18.1 (the `bevy-verify` skill / the registry source under `~/.cargo/registry/.../bevy_*-0.18.1`) — memorized 0.13–0.16 shapes drift.

## Album art

Requires the player to expose `mpris:artUrl` (spotifyd does; Firefox/YouTube does **not**). There's a YouTube fallback in `mpris/mod.rs` that derives a thumbnail from `xesam:url`.
