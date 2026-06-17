# Cavalier Visualizer Report

> Research notes on **[NickvisionApps/Cavalier](https://github.com/NickvisionApps/Cavalier)** — a CAVA-driven music
> visualizer — focused on the rendering modes and the configuration that drives them. Intended as a spec reference for
> reimplementing Cavalier-style visuals (e.g. in the `bava` Rust/Bevy project).

## 1. What Cavalier is

- A desktop audio visualizer by Nickvision. Language: **C# / .NET 8**, UI: **GTK4 + libadwaita** (GNOME).
- Audio analysis is delegated to **CAVA** (`>= 0.9.1`) running as a **subprocess**, not a library. Cavalier writes a
  CAVA config, launches `cava`, and reads its raw binary stdout.
- Rendering is done with **SkiaSharp** (Skia 2D canvas). All shapes are built as `SKPath` and painted with `SKPaint`.
- Tagline features: **11 drawing modes**, backgrounds/foregrounds as **solid color, gradient, or image**, plus a handful
  of CAVA tuning settings (smoothing, noise reduction, sensitivity, etc.).

Key source files (all under `NickvisionCavalier.Shared/Models/`):

| File | Role |
|------|------|
| `CAVA.cs` | Spawns/manages the `cava` subprocess, parses its binary output into `float[]` samples |
| `Renderer.cs` | The whole drawing engine (Skia). Picks a draw method per mode; handles mirror, gradients, images |
| `Configuration.cs` | All persisted settings + defaults |
| `DrawingMode.cs` | Enum of the 11 modes |
| `Mirror.cs` | Mirror enum |
| `DrawingDirection.cs` | Direction enum |
| `ColorProfile.cs`, `ColorType.cs`, `Theme.cs` | Color/gradient profiles |

## 2. Audio data contract (CAVA → Renderer)

This is the most important interface to replicate.

- CAVA is configured with **`bars = BarPairs * 2`** (default `BarPairs = 6` → **12 bars**).
- CAVA output format: **raw binary**, 16-bit unsigned ints, little-endian, one value per bar per frame.
- Cavalier reads them via `BinaryReader` and **normalizes to `0.0–1.0`** by dividing by `65535`.
- The sample delivered to the renderer is a **`float[]` of length `BarPairs * 2`**, each in `[0, 1]`.
- Bar order may be reversed depending on `ReverseOrder` / stereo config.
- New samples are pushed via an `OutputReceived` event; the renderer draws at `Framerate` (default 60).

CAVA settings Cavalier exposes (written into the generated cava config):

| Setting | Source property | Notes |
|---------|-----------------|-------|
| Framerate | `Framerate` (60) | render + cava rate |
| Bars | `BarPairs * 2` (12) | must be even; pairs matter for split/mirror |
| Autosens | `Autosens` (true) | auto gain |
| Sensitivity | `Sensitivity` (10) | value is **squared** before use |
| Channels | `Stereo` (true) | stereo vs mono |
| Monstercat | `Monstercat` (true) | smoothing algorithm |
| Noise reduction | `NoiseReduction` (0.77) | 0–1, written on a 0–100 scale |
| Input | PulseAudio, source `auto` | default capture |

## 3. Drawing modes (the 11)

Defined in `DrawingMode.cs`, in this exact order. There are **6 "Box" (linear) modes** and **5 "Circle" (radial)
modes** — note Splitter has **no** circle variant, which is why it's 11 and not 12.

| # | Enum | Family | Shape |
|---|------|--------|-------|
| 0 | `WaveBox` | Wave | Smooth Bézier waveform across the axis |
| 1 | `LevelsBox` | Levels | Stacked discrete blocks (VU-meter style) |
| 2 | `ParticlesBox` | Particles | One floating item per bar, position = level |
| 3 | `BarsBox` | Bars | Classic spectrum bars |
| 4 | `SpineBox` | Spine | Centered squares/hearts scaling with level |
| 5 | `SplitterBox` | Splitter | Zig-zag oscillating around the center axis |
| 6 | `WaveCircle` | Wave | Wave wrapped radially around a center |
| 7 | `LevelsCircle` | Levels | Stacked blocks radiating outward |
| 8 | `ParticlesCircle` | Particles | Particles placed on a ring |
| 9 | `BarsCircle` | Bars | Bars radiating from an inner circle |
| 10 | `SpineCircle` | Spine | Squares/hearts around a ring |

Default mode: **`WaveBox`**.

### Per-mode drawing behavior

**Wave** (`DrawWaveBox` / `DrawWaveCircle`)
- Builds a smooth curve through the per-bar sample points using **cubic Bézier** interpolation; control points come from
  the gradient between neighboring samples.
- `Filling = true` closes the path to the baseline edge (filled wave); `false` strokes just the curve at
  `LinesThickness`.
- Circle variant maps each sample to a **radius between `InnerRadius` and the full radius**, swept around the center
  with `Rotation` applied. Its foreground gradient is **radial** (other circle modes use linear).

**Levels** (`DrawLevelsBox` / `DrawLevelsCircle`)
- Quantizes each sample to **10 levels**: draws `floor(sample[i] * 10)` stacked rounded rectangles per bar.
- Block size/spacing from `ItemsOffset` and `ItemsRoundness`.
- Circle variant stacks the blocks **outward from the inner radius**, one column per bar around the ring.

**Particles** (`DrawParticlesBox` / `DrawParticlesCircle`)
- Draws **one** rounded item per bar; its position along the axis (or radial distance) scales with the sample on an
  ~11-step scale (`(fullRadius - innerRadius)/10 * 9 * sample[i]` in the circle case).
- Same `ItemsOffset` / `ItemsRoundness` sizing as Levels.

**Bars** (`DrawBarsBox` / `DrawBarsCircle`)
- The classic spectrum: each bar extends from the axis by `width*sample[i]` or `height*sample[i]`.
- Honors `Filling` (solid vs outline) and `LinesThickness`.
- Circle variant: radial bars of length `(fullRadius - innerRadius) * sample[i]`, angularly spaced by bar index.

**Spine** (`DrawSpineBox` / `DrawSpineCircle`)
- Draws a **centered square per bar** that scales uniformly with the sample value, centered on the axis.
- If `Hearts = true`, draws **heart shapes** instead of squares (via `CreateHeart()`, positioned with canvas
  translate/scale).
- Circle variant places the scaling squares/hearts around the ring.

**Splitter** (`DrawSplitterBox` — box only)
- A **zig-zag** that alternates left/right (or top/bottom) by bar index parity, amplitude
  `height/2 * (1 + sample[i] * orient)`, connecting through center-axis points. No circle form.

## 4. Mirror modes

`Mirror.cs`:

| Value | Meaning |
|-------|---------|
| `Off` (0) | Single visualization, no mirroring |
| `Full` | Whole visualization duplicated and reflected across the axis; the mirrored copy uses **reversed** sample order |
| `SplitChannels` | First half of the bar array → one side, second half → the mirrored side (stereo L/R split) |

Related:
- `ReverseMirror` (bool, default false) flips which side the mirror copy goes to.
- Mirror affects geometry (`GetMirrorX/Y/Width/Height`, `GetMirrorDirection`) **and** gradients — the color list is
  **doubled and reversed** so the gradient is symmetric across the mirror.
- For `SplitChannels` to be meaningful, bars must be even (they always are: `BarPairs * 2`).

## 5. Drawing direction

`DrawingDirection.cs` — orientation of linear (Box) modes; for circle modes it influences gradient direction:

| Value | Meaning |
|-------|---------|
| `TopBottom` | Vertical, origin top |
| `BottomTop` | Vertical, origin bottom **(default)** |
| `LeftRight` | Horizontal, origin left |
| `RightLeft` | Horizontal, origin right |

The renderer uses a `FlipCoord()` helper to invert coordinates for the `BottomTop` / `RightLeft` cases.

## 6. Colors, gradients, themes

**`ColorProfile`** (a named, cloneable color scheme):
- `Name : string`
- `FgColors : List<string>` — foreground colors, ARGB hex strings `#aarrggbb`
- `BgColors : List<string>` — background colors, same format
- `Theme : Theme`
- Default profile: name "Default", **1 fg color `#ff3584e4`** (GNOME blue), **1 bg color `#ff242424`** (dark gray),
  `Theme.Dark`.

**Gradients**: there is no separate "gradient" object — a gradient is simply a `ColorProfile` whose `FgColors` /
`BgColors` list has **more than one** color. `Renderer.CreateGradient(...)` builds an `SKShader`:
- Background gradient is **linear**, oriented by `Direction`.
- Foreground gradient is **radial for `WaveCircle`**, linear otherwise.
- Under mirror, the color array is doubled+reversed for symmetry.

**`ColorType`**: `Foreground = 0`, `Background` — selects which list a color edits.

**`Theme`**: `Light = 0`, `Dark` — light vs dark scheme; on multiple profiles the app stores per-profile theme.

Multiple profiles are supported: `Configuration.ColorProfiles : List<ColorProfile>` with `ActiveProfile : int` index.

## 7. Full configuration reference (defaults)

From `Configuration.cs`:

| Property | Type | Default | Purpose |
|----------|------|---------|---------|
| `WindowWidth` / `WindowHeight` | uint | 500 / 300 | window size |
| `WindowMaximized` | bool | false | |
| `AreaMargin` | uint | 0 | padding around the whole drawing area |
| `AreaOffsetX` / `AreaOffsetY` | float | 0 / 0 | proportional shift of the draw region |
| `Borderless` | bool | false | window chrome |
| `SharpCorners` | bool | false | window corner style |
| `ShowControls` | bool | false | overlay controls |
| `AutohideHeader` | bool | false | |
| `Framerate` | uint | 60 | render + CAVA rate |
| `BarPairs` | uint | 6 | bars = `BarPairs * 2` (→ 12) |
| `Autosens` | bool | true | CAVA auto sensitivity |
| `Sensitivity` | uint | 10 | CAVA sensitivity (squared) |
| `Stereo` | bool | true | CAVA channels |
| `Monstercat` | bool | true | CAVA smoothing |
| `NoiseReduction` | float | 0.77 | CAVA noise filter (0–1) |
| `ReverseOrder` | bool | true | flips bar order |
| `Direction` | DrawingDirection | `BottomTop` | orientation |
| `ItemsOffset` | float | 0.1 | spacing between items (valid ~0–0.5) |
| `ItemsRoundness` | float | 0.5 | corner radius multiplier |
| `Filling` | bool | true | solid fill vs stroke |
| `LinesThickness` | float | 5 | stroke width when not filling |
| `Mode` | DrawingMode | `WaveBox` | active mode |
| `Mirror` | Mirror | `Off` | mirror mode |
| `ReverseMirror` | bool | false | flip mirror side |
| `InnerRadius` | float | 0.5 | circle modes: inner radius ratio (0–1) |
| `Rotation` | float | 0 | circle modes: angular offset (radians) |
| `ColorProfiles` | List<ColorProfile> | one default | color schemes |
| `ActiveProfile` | int | 0 | selected profile index |
| `BgImageIndex` | int | -1 | background image (-1 = none) |
| `BgImageScale` | float | 1 | bg image scale |
| `BgImageAlpha` | float | 1 | bg image opacity |
| `FgImageIndex` | int | -1 | foreground image (-1 = none) |
| `FgImageScale` | float | 1 | fg image scale |
| `FgImageAlpha` | float | 1 | fg image opacity |
| `Hearts` | bool | false | Spine modes draw hearts instead of squares |

## 8. Render pipeline (per frame)

`Renderer.Draw(float[] sample, float width, float height)`:

1. Clear canvas; paint background (solid or gradient from active `ColorProfile.BgColors`).
2. Prepare/cache background & foreground images (rescaled only when size or scale changed; alpha applied via paint).
3. `switch (Configuration.Current.Mode)` → call the matching `Draw*` method, producing one or more `SKPath`s.
4. Apply mirror: draw a second mirrored path with reversed/half sample data and mirrored geometry.
5. If a foreground image is set: combine paths (`AddPath`), `ClipPath` to them, and draw the fg image as a mask
   (otherwise fill/stroke paths with fg color or gradient).
6. `Canvas.Flush()`.

## 9. Notes for reimplementation (bava context)

- The renderer is **stateless per frame** apart from cached scaled images — easy to port to an immediate-mode renderer.
- The **sample contract is the clean seam**: produce a `float[]` of `BarPairs*2` values in `[0,1]` per frame and the
  mode math above is all pure geometry. `bava` already drives cavacore, so the equivalent is the cavacore output buffer
  normalized the same way.
- Quantized modes (Levels = 10 steps, Particles ≈ 11 steps) intentionally discretize; Bars/Wave/Spine/Splitter are
  continuous.
- Splitter has no circle variant — don't try to add one to reach "12".
- `Filling` + `LinesThickness` is the fill-vs-outline switch shared by Wave and Bars.
- Mirror `SplitChannels` assumes the first half / second half of the bar array map to the two stereo channels.

---
*Sources: README and `NickvisionCavalier.Shared/Models/` source on the `main` branch of
https://github.com/NickvisionApps/Cavalier (DrawingMode.cs, Mirror.cs, DrawingDirection.cs, ColorProfile.cs,
ColorType.cs, Theme.cs, Configuration.cs, CAVA.cs, Renderer.cs).*
