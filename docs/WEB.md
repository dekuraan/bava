# The web build

bava compiles to `wasm32-unknown-unknown` and runs the same visualizers,
settings editor and physics in a browser. What it cannot bring along is the
loopback capture: no web API exposes "what this machine is playing". The closest
thing a page can get is **screen sharing with audio**, so that is what the web
build uses — you share a browser *tab*, and its audio output becomes the
visualizer's input.

That covers the interesting case directly: the page embeds a YouTube player, you
share *this* tab, and the embed's audio drives the visualizer. It works just as
well pointed at any other tab — Spotify Web, Bandcamp, SoundCloud, a second
YouTube.

## Building

```sh
trunk serve                    # http://localhost:8080, rebuilds on change
trunk build --release          # → dist/, ready to upload anywhere static
```

`Trunk.toml` at the repo root points at `crates/bava/web/index.html`; the page's
`rel="rust"` link points back at the crate manifest, so the html and Cargo.toml
do not have to be siblings.

For a project page under a path prefix, pass it — this is what CI does:

```sh
trunk build --release --public-url /bava/
```

Prerequisites: `rustup target add wasm32-unknown-unknown`, `trunk`, and
`wasm-opt` (from binaryen) for release builds. `wasm-bindgen` is downloaded by
trunk at the exact version the crate was built against.

## Deploying

`.github/workflows/pages.yml` builds and publishes `dist/` to GitHub Pages on
every push to `main`. It needs Pages set to the **GitHub Actions** source once,
under Settings → Pages. Pull requests build (unreleased, to keep them fast) but
never deploy.

The output is plain static files with no server requirements beyond HTTPS —
`getDisplayMedia` is gated on a secure context, which `*.github.io`, any HTTPS
host, and `localhost` all satisfy.

## How audio gets in

```
 tab audio ──getDisplayMedia──▶ MediaStreamTrack ──▶ AudioWorklet
                                                          │
                                        interleaved f32 blocks (postMessage)
                                                          ▼
                     Cava resource ◀── cavacore ◀── bava_push_audio (wasm export)
```

- **`web/bava.js`** requests the share, builds the audio graph, and forwards
  blocks into wasm. It also drives the embedded player and pushes the video's
  title and thumbnail as now-playing metadata.
- **`web/audio-worklet.js`** runs on the audio thread and interleaves each
  block. A worklet rather than an `AnalyserNode` because cavacore needs a
  *continuous* sample stream: its framerate estimate and auto-sensitivity assume
  a steady number of new samples per execute, and polling an analyser from
  `requestAnimationFrame` overlaps or skips samples depending on frame timing.
- **`src/cava/capture/web.rs`** owns the `bava_push_audio` export and buffers
  what arrives. **`src/cava/mod.rs`'s `pump_web_audio`** then moves it into the
  same `AudioRing` the native capture thread feeds, so chunking, the plan
  rebuild to the real sample rate, and `feed_cava` are shared with desktop.

The browser picks the `AudioContext` sample rate (48 kHz, typically) and honours
no request for another, exactly like WASAPI shared-mode loopback — so the same
`reconcile_capture_rate` path rebuilds the cavacore plan to match on the first
block that arrives.

## Sharing gotchas

These are Chrome's rules, not bava's, and they are the usual reason "it captured
but the bars are flat":

- **Only a *tab* carries audio reliably.** Sharing a window never does; sharing
  an entire screen carries system audio on Windows and ChromeOS only.
- **"Also share tab audio" must be ticked** in the picker. It is a checkbox in
  the corner of the tab list and it does not remember its state.
- **Chrome only.** Firefox's `getDisplayMedia` has no audio at all, and Safari's
  is screen-only. The page says so when the API is missing, but a Firefox user
  who shares successfully will still get silence.
- A video track is requested even though nothing draws it: Chrome rejects
  audio-only display capture outright.

## What is different from the desktop build

| | desktop | web |
|---|---|---|
| audio in | PipeWire / PulseAudio / WASAPI / Core Audio | tab share via `getDisplayMedia` |
| now playing | MPRIS / GSMTC / MediaRemote | YouTube IFrame API (title) + thumbnail (art) |
| config | `~/.config/bava/config.toml` | `localStorage`, same TOML under the same paths |
| CLI | `argv` | the page's query string, same flags |
| offline render | `--input song.mp3 --out video.mp4` | not built (needs ffmpeg and a filesystem) |
| renderer | Vulkan/Metal/DX12 | WebGL2 |

Everything else — every drawing mode, the physics balls, dynamic album colors,
the egui settings editor, profiles — is the same code.

Settings persist per browser origin. `Config::write` and friends go through a
small `store` module in `src/config.rs` that is the filesystem natively and
`localStorage` on the web, keyed by the very same paths (`/config/bava/…`), so
the editor's Save/Reload/Profiles work unchanged.

Query-string flags map onto the same `Cli` clap parses natively, so
`?bars=48&mode=wave-circle&monstercat=2&gui` is a shareable preset. `?video=<id>`
is consumed by the page rather than the app (see `PAGE_ONLY_PARAMS`).

## WebGL2 vs WebGPU

The build targets **WebGL2** (`bevy/webgl2`), which every current browser has.
WebGPU would be faster but is still desktop-Chrome/Edge only; switching is a
one-word change to the wasm `bevy` features in `crates/bava/Cargo.toml`. On
WebGL2 Bevy logs a few "not supported on this device" lines at startup (order
independent transparency, GPU preprocessing, depth of field) — none of them
affect this app, which is 2D meshes plus bloom.

## Size

A release build is on the order of 20 MB of wasm before compression and roughly
a third of that over the wire, since Pages serves it gzipped. Most of it is
Bevy's renderer. `data-wasm-opt="z"` in `index.html` and the workspace's
`lto = "thin"` / `codegen-units = 1` release profile are already doing the
obvious work; dropping the egui editor or the tonemapping LUTs would be the next
lever if it ever matters.
