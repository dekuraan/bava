# bava

A [Bevy](https://bevyengine.org/) music visualizer for Linux, driven by
[cavacore](https://github.com/karlstav/cava) (the analysis engine from CAVA),
with now-playing metadata and album art over MPRIS (e.g. spotifyd).

## Status

Working vertical slice:

- **Audio → cavacore → `Cava` resource**: PulseAudio captures the default sink's
  monitor (works through pipewire-pulse), cavacore turns it into smoothed,
  log-spaced frequency bars, published into a Bevy resource every frame.
- **2D bars / monstercat visualizer**: one sprite per bar, growing from the
  bottom with a cyan→magenta amplitude-lit gradient.
- **MPRIS**: a background thread tracks the active player, surfacing title /
  artist / album in a HUD label and fetching + decoding album art into a dimmed
  full-window backdrop.

Everything degrades gracefully: no audio server, no MPRIS player, or no album
art just leaves the corresponding piece idle — the app still runs.

## Workspace layout

```
crates/
  cavacore-sys/   # raw FFI + build.rs compiling vendored upstream cavacore.c (links fftw3)
  cavacore-rs/    # safe wrapper: CavaConfig -> CavaPlan, validated, Send, no-UB; rigorous tests
  bava/           # the Bevy app
    src/cava/     # CavaPlugin, Cava resource, capture thread
    src/cava/capture/  # AudioCapture trait + PulseAudio backend (trait-abstracted for future PipeWire)
    src/mpris/    # MprisPlugin: NowPlaying + AlbumArt resources
    src/vis/      # VisPlugin: bars visualizer + HUD
```

## Build & run

Requires (all present on a typical Arch/CachyOS desktop): a C compiler, `fftw3`,
`libpulse`, `dbus`, and the usual Bevy Linux deps (`alsa`, `udev`, Vulkan).

```sh
cargo run -p bava        # or just `cargo run`
cargo test -p cavacore-rs # 20 safety + DSP tests
```

Play something (Spotify via spotifyd, a browser, anything that hits the default
output) and the bars react. Album art and track info appear when an MPRIS player
is active.

## Configuration

Insert a `CavaSettings` resource before adding `CavaPlugin` to override defaults
(bars-per-channel, channels, sample rate, frame size, noise reduction, cut-off
band, or a pinned capture source).

## Extending: 2D, 3D and shader visualizers

Every visualizer reads the same `Cava` resource, so new styles are independent
plugins:

```rust
fn my_vis(cava: Res<bava::cava::Cava>, /* your query */) {
    let bars = cava.mono();      // Vec<f32>, per-bar magnitude (≈0..1)
    let left = cava.left();      // &[f32]   left channel
    let right = cava.right();    // &[f32]   right channel (empty in mono)
    // drive meshes, transforms, materials, or shader uniforms from these
}
```

Planned directions (the architecture already supports them):

- **3D scenes**: spawn a 3D camera + meshes in a plugin and modulate transforms /
  emissive materials from `Cava`.
- **Shader visualizers**: feed the bars into a custom `Material` / uniform buffer
  and render a fullscreen quad.
- **Native PipeWire capture**: add a second `AudioCapture` impl alongside the
  PulseAudio one and select by config.

## Credits

Vendors `cavacore.c` / `cavacore.h` from [karlstav/cava](https://github.com/karlstav/cava)
(MIT, see `crates/cavacore-sys/vendor/cava/LICENSE`).
