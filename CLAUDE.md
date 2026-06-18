# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A cross-platform music visualizer: loopback audio capture → cavacore analysis → a Bevy `Cava` resource → visualizers, with now-playing + album art. Cargo workspace, Bevy **0.18.1**, edition 2024. Two `cfg(target_os)`-gated backends per OS-specific concern: **Linux** uses PulseAudio monitor capture + MPRIS (over D-Bus); **Windows** uses WASAPI loopback capture + GSMTC (`GlobalSystemMediaTransportControlsSessionManager`). Everything else (cavacore, vis, gui, config) is platform-agnostic.

## Commands

```sh
cargo run -p bava            # run the app (needs a Wayland/X11 display in env)
cargo build --release -p bava
cargo test -p cavacore-rs    # the rigorous cava safety/DSP suite
cargo test -p cavacore-rs --test dsp noise_reduction_slows_decay  # single test
```

Running needs the display env this shell may lack: `WAYLAND_DISPLAY=wayland-1 cargo run -p bava` (check `ls /run/user/1000/wayland-*` for the socket — it is **not** `wayland-0`).

Windows code can't be built natively from this Linux box, but it *can* be type-checked by cross-compiling (no link step): `cargo check --target x86_64-pc-windows-gnu -p bava`. `cavacore-sys`'s `build.rs` compiles `cavacore.c` with `cc`, which needs `fftw3.h` on the mingw include path — stage the system header and point `cc` at it: `cp /usr/include/fftw3.h /tmp/fftwinc/ && CFLAGS_x86_64_pc_windows_gnu=-I/tmp/fftwinc cargo check --target x86_64-pc-windows-gnu -p bava`. (Actually *linking* on a real Windows host still needs FFTW3 available, since `build.rs` falls back to `-lfftw3`.)

## Crates

- `crates/cavacore-sys` — FFI to vendored upstream `cavacore.c/.h` (in `vendor/cava/`); `build.rs` compiles it with `cc` and links `fftw3` + `m`. Hand-written bindings mirroring `struct cava_plan`.
- `crates/cavacore-rs` — safe `CavaConfig → CavaPlan` wrapper. `CavaPlan` is `Send` but **not `Sync`**; FFTW planning is serialized behind a process-wide lock. Tests live in `tests/` (`validation.rs`, `dsp.rs`, shared signal gen in `tests/common/`).
- `crates/bava` — the Bevy app. Three plugins, each in its own module: `cava` (capture + analysis), `mpris` (now-playing metadata + art; the module name is historical — it now covers both MPRIS and GSMTC), `vis` (visualizers + HUD). Platform-specific deps are split into `[target.'cfg(target_os = "…")'.dependencies]` (Linux: `libpulse-*`, `mpris`, `ureq`; Windows: the `windows` crate).

## Architecture notes (non-obvious)

- **cava runs on the Bevy main thread, per frame.** A capture thread (the backend chosen by `cava/capture/mod.rs`'s `open()`: `pulse.rs` on Linux, `wasapi.rs` on Windows) only reads audio into a ring buffer; the `feed_cava` system drains it and runs `cava_execute`. `CavaState` (holds `CavaPlan`) is a **NonSend resource** because `CavaPlan` is `!Sync`. Both backends implement the `AudioCapture` trait and hand back interleaved `f64` at the requested rate/channels — but only Pulse can *force* that format (it resamples server-side); WASAPI shared-mode loopback is locked to the device mix format, so `wasapi.rs` down/up-mixes and linearly resamples to match the rate/channels the `CavaPlan` was built for.
- **Feed cavacore *fixed-size* chunks, never "all available."** cavacore's framerate estimate / autosens assume a steady `new_samples` per call; variable counts stall the autosens ramp and bars stay at zero. `frame_samples` sets the chunk (and thus cava's update rate).
- **Visualizers share the `Cava` resource** (`left()`/`right()`/`mono()`). `vis::DrawingMode` (Space cycles) is the live-toggled resource for the active mode; it mirrors Cavalier's 11 modes (`VisShape` × `VisFamily`, Splitter box-only). `bars.rs` renders **all six box modes distinctly** by dispatching on `DrawingMode::shape()`: Bars/Levels/Particles/Spine are a one-mesh-per-bar pool of rounded-rect `Mesh2d`s (kept in sync with the live bar count by `reconcile_bars`, rounded per `items_roundness`), while Wave/Splitter are a single antialiased stroke `Mesh2d` (bar pool hidden). `circle.rs` still stands in for **every** `VisFamily::Circle` mode (per-shape circle rendering is the next increment). **All lines/fills are mesh-based — no gizmos.** `vis::stroke` builds feather-antialiased meshes: `apply_stroke` for polylines (Wave/Splitter/ring) and `apply_rounded_rect` for the bars; a solid core + a `STROKE_FEATHER`-px alpha ramp gives resolution-independent AA, and the mesh's `ATTRIBUTE_COLOR` × a white blend `ColorMaterial` carries the HDR gradient (which blooms via the HDR camera). `vis::VisSettings` + the `[vis]` config cover the full Cavalier option set (mirror/direction/theme, color profiles, fg/bg image layers). Renderers reuse `spread_monstercat` / `gradient_color` from `vis/mod.rs`. The circle fill is a per-frame-updated triangle-fan `Mesh2d`; the ring outline is a closed stroke mesh.
- **Config**: `~/.config/bava/config.toml` (auto-created), CLI via clap layered on top (CLI > file > defaults). `config.rs` is the single source mapping `[audio]/[cava]/[vis]` → the runtime `*Settings` resources, inserted in `main.rs` before the plugins. `Config::from_settings` is the inverse (live resources → TOML), used by the editor to save. **Profiles** are named full-config snapshots under `~/.config/bava/profiles/<name>.toml` (`Config::{list,load,save}_profile`); load one at startup with `--profile NAME`.
- **Settings editor** (`gui/mod.rs`, `bevy_egui` 0.39): a floating egui window toggled with the configurable `[gui] toggle_key` (default `p`; names parsed in `config.rs`'s `KEY_NAMES`/`parse_key`, stored on `EditorState.toggle_key`) — or `--gui` to start open, X / Esc to close. Its system runs in the `EguiPrimaryContextPass` schedule. It edits the `VisSettings` / `DrawingMode` resources **live** (reflected the same frame). Audio/DSP edits set [`CavaRebuild`](crate::cava::CavaRebuild): the `rebuild_cava` system rebuilds the `CavaPlan` on an explicit "Apply" press, keeping the running capture thread's rate/channels (those + source are restart-only). `EditorState.capture_keyboard` mirrors `ctx.wants_keyboard_input()` so `cycle_mode` (Space) suppresses itself while a text field has focus. egui auto-attaches its primary context to the first `Camera2d` (spawned in `bars.rs`).

## Bevy 0.18 gotchas

- Buffered events are **Messages**: `AppExit` is read with `MessageReader`, not `EventReader`.
- 2D mesh/material types come from `bevy::mesh` (`Indices`, `PrimitiveTopology`) and `bevy::prelude` (`Mesh2d`, `ColorMaterial`, `MeshMaterial2d`); `RenderAssetUsages` from `bevy::asset`.
- When recommending any Bevy API, verify it against 0.18.1 (the `bevy-verify` skill / the registry source under `~/.cargo/registry/.../bevy_*-0.18.1`) — memorized 0.13–0.16 shapes drift.

## Now-playing & album art

The `mpris` module is split: `mpris/mod.rs` holds the shared Bevy plugin, the `NowPlaying`/`AlbumArt` resources, the channel drain, and `decode_art_bytes` (encoded image bytes → RGBA8); each platform backend runs a poll loop on a background thread and sends `MprisMsg`s.

- **Linux** (`mpris/linux.rs`): polls the active MPRIS player over D-Bus. Album art requires the player to expose `mpris:artUrl` (spotifyd does; Firefox/YouTube does **not**) — fetched over HTTP/`file://`. There's a YouTube fallback that derives a thumbnail URL from `xesam:url`.
- **Windows** (`mpris/windows.rs`): polls the current `GlobalSystemMediaTransportControlsSession` (GSMTC). Art comes from the session's embedded **thumbnail stream** (read via `DataReader`), not a URL — so `NowPlaying.art_url` is always `None` on Windows.
