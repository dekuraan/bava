# Measuring the default visualizer

The default view is WaveCircle, with plasma, halo, starfield/nebula, bloom,
camera effects and three launch balls. A 60 Hz display has a 16.67 ms frame
budget; 120 FPS equivalent work means keeping the frame cost below 8.33 ms.
Measure CPU and GPU costs separately: vsync and offline encoding can obscure
the amount of work the visualizer actually does.

For uncapped throughput without readback or encoding, run the ignored GPU
benchmark. It warms the default view for 180 frames, feeds precomputed noise
and bass kicks, measures 600 frames at 1080p and waits for GPU completion:

```sh
cargo test --release -p bava --bin bava default_scene_render_benchmark -- --ignored --nocapture
```

It reports CPU/GPU completion time per frame. It uses the default three balls
and effects, with synthetic audio and no album-art texture or settings editor.

On alienware's GTX 860M, this measured 8.213 ms/frame (121.8 FPS uncapped) on
2026-09-29, versus 10.766 ms with the same shaders and 4× MSAA. These are
600-frame averages, not percentile guarantees; the margin below 8.33 ms is small.

`--debug` enables Bevy's GPU pass diagnostics and logs them about once per
second. Vulkan and DX12 support GPU timestamps; other backends may report only
CPU timings. Diagnostics are disabled during normal operation.

```sh
cargo run --release -p bava -- --debug --config benchmark.toml \
  --input song.wav --out benchmark.mp4 --duration 10 \
  --width 1920 --height 1080 --fps 60
```

Use an otherwise empty `benchmark.toml` to measure defaults. For reproducible
launch-ball sizes and colors, include:

```toml
[physics]
randomize = false
```

The `render/*/elapsed_gpu` values describe the effects pass, bloom, tonemapping,
postprocessing and upscaling. Their sum estimates GPU rendering work, excluding
readback and encoding. The logged values are smoothed timings and historical
averages, not worst-case or percentile frame times. Offline `frame_time` and
`fps` reflect the fixed simulation clock; they do not measure throughput.
The recorder's completion time includes readback, encoding and startup, so it
must not be reported as interactive frame time. Run comparisons without a
concurrent build or other heavy workload.

For CPU system costs, use the existing puffin integration:

```sh
BAVA_PROFILE_REPORT=300 cargo run --profile profiling --features profile -p bava -- \
  --config benchmark.toml --input song.wav --out benchmark.mp4 --duration 10
```

## Built-in effect costs

Built-in 2D effects share a 4 MiB, unfiltered float lookup texture. Its
512×512 texels store the four PCG corners of each noise cell. The shader still
interpolates noise and warps domains each frame. The noise lattice wraps every
512 cells and supports negative coordinates. Four noise octaves retain the main
structure while dropping the fifth octave's 1/32-amplitude detail.

The default bloom pyramid has a maximum dimension of 256 rather than 512;
the output and visualizer geometry remain at the requested resolution.
This trades a little fine glow detail for less intermediate pixel work.

The default 2D camera disables MSAA: its strokes and particles use feathered
meshes, and balls smooth their edges in the fragment shader. This avoids the
multisampled HDR target and resolve. Loading a 3D scene enables 4× MSAA on both
shared-target cameras; unloading restores the 2D setting.

Custom scene shaders retain the five-octave, non-periodic `bava::fx::fbm` helper
and the existing texture bindings. Custom backdrop shader parameters are not
used for the built-in star rotation uniforms.
