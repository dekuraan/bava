# Scenes

A scene is a directory with a `scene.toml` and the files it names: WGSL
shaders, textures, glTF models, and sounds. It can add 2D or 3D content, drive
any of it from the music, replace the shaders of bava's own blob, and override
any setting from `config.toml`. You don't need Rust.

Two scenes ship in the binary, and both are written only in this format:

| Scene | What it shows |
|---|---|
| `solar_system` | 2D. The blob is the sun (custom shader); planets on tilted orbits pass behind it and pulse with their bands. Physics balls are comets. |
| `minecraft` | 3D. "Blockland": a voxel island that grows from the spectrum, glowing ores, two dancing CC0 characters, a ring of floating ore blocks, drifting clouds. |

Their sources are under [`assets/scenes/`](../assets/scenes). Copy one to start your own.

## Using scenes

```sh
bava --list-scenes                      # built-ins and your own scenes
bava --scene solar_system               # start with a scene
bava --scene ~/my-scenes/aquarium       # any directory with a scene.toml
bava --scene scene.toml                 # or the scene.toml itself
bava --scene none                       # scenes off (overrides the config)
bava --input song.flac --out out.mp4 --scene minecraft   # render one to video
```

- In the app, **N** cycles through the scenes (none, then each scene, then
  none again). The editor (`p`) has a Scene picker and a reload button.
- `[scene] name = "..."` in `config.toml` picks the startup scene. Saving from
  the editor records the active scene there.
- On the web, add `?scene=minecraft`.
- **User scenes** live in `~/.config/bava/scenes/<name>/scene.toml` and show up
  by name. A user scene with a built-in's name replaces the built-in, so you can
  copy one out and edit it in place.
- **Hot reload:** a scene loaded from a directory rebuilds within a second of
  saving any file in it. A shader that fails to compile is logged, and the
  objects using it disappear until it's fixed; a `scene.toml` that fails to
  parse leaves the scene unloaded, with the error shown in the editor.

While a scene is active, its `[config]` overrides sit on top of your settings.
Switching the scene off puts back the values it overrode. Anything you change
while it runs is yours and stays, including a key the scene had set. Saving
from the editor stores your own settings plus the scene's name, never the
scene's overrides.

## File layout

```
my-scene/
  scene.toml
  shaders/*.wgsl
  textures/*.png|jpg
  models/*.glb|gltf
  sounds/*.ogg|wav
```

Paths in `scene.toml` are relative to the scene directory and can't climb out
of it (`..` is rejected). File names can't contain `#`, which Bevy's asset
paths reserve for labels. Symlinked files and directories inside a scene are
followed. Colors are `"#rrggbb"`, or `"#aarrggbb"` with alpha
first, the same as `config.toml`. Unknown keys are errors, reported with the
key's name.

## `[scene]`

```toml
[scene]
name = "My Scene"
description = "One line for the picker"
dimension = "2d"        # "2d" or "3d"
canvas = 1000.0         # 2D: units across the window's shorter side (0 = pixels)
clear_color = "#020309"
hud = true              # the now-playing title
overlay_2d = false      # 3D: keep drawing bava's 2D visualizer on top
```

`canvas` makes a 2D scene resolution-independent. With `canvas = 1000`, every
position, size and radius in the file is in thousandths of the shorter window
side.

## `[config]`: override any setting

Use the same tables and keys as `config.toml`:

```toml
[config.vis]
mode = "wave_circle"
circle_scale = 0.62      # overall size of the circle modes
dynamic_colors = false

[[config.vis.profile]]   # arrays replace wholesale
name = "Sun"
fg = ["#b81d00", "#ff6a00", "#ffb52e", "#fff1c4"]
bg = ["#020309"]

[config.physics]
trails = true

[config.fx]
flares = 1.6
```

`[config.audio]`, `[config.gui]` and `[config.scene]` are ignored with a
warning. Keys that don't exist are reported.

## `[shaders]`, `[blob]`

```toml
[shaders]
sun = "shaders/sun.wgsl"
planet = "shaders/planet.wgsl"

[blob]                         # 2D: re-skin bava's own layers
fill_shader = "sun"            # the blob's interior (WaveCircle, fill on)
fill_params = [[5.0, 0.8, 1.0, 0.0]]
halo_shader = "..."            # the additive glow around the blob
backdrop_shader = "..."        # the full-window starfield
```

## Objects

```toml
[[object]]
name = "earth"                 # other objects can orbit it
mesh = "circle"                # 2D: circle, rect, ring
                               # 3D: sphere, cube, plane, cylinder, torus, capsule, cone
size = [26.0]                  # see the table below
anchor = "center"              # or "bottom": grow upward under scale.y reactions
position = [0.0, 0.0, 2.0]     # 2D: z is the draw layer
rotation = [0.0, 45.0, 0.0]    # degrees
scale = 1.0                    # or [x, y, z]
material = { shader = "planet", texture = "textures/earth.jpg", params = [[...], ...] }
orbit = { radius = [380.0, 160.0], period = 14.0 }
spin = { speed = 30.0, axis = [0.0, 1.0, 0.0] }
react = [{ property = "scale", band = "mid", amount = 0.2 }]
collider = { radius = 21.0 }   # 2D: physics balls bounce off it
```

| `mesh` | `size` |
|---|---|
| `circle` | `[radius]` |
| `rect` | `[width, height]` |
| `ring` | `[radius, line width]` |
| `sphere` | `[radius]` (UV sphere, so equirectangular textures wrap) |
| `cube` | `[x]` or `[x, y, z]` |
| `plane` | `[x, z]` |
| `cylinder` | `[radius, height]` |
| `torus` | `[major radius, tube radius]` |
| `capsule` | `[radius, length]` |
| `cone` | `[radius, height]` |

**Models (3D):** use `model = "models/x.glb"` instead of `mesh`. The first
scene in the file is spawned. `animation = N` loops clip N and plays it faster
the louder the music is.

### Materials

A material with a `shader` is an effect material. The shader receives the
live audio uniform (see below). Without a shader you get a plain material:
`ColorMaterial` in 2D, lit PBR `StandardMaterial` in 3D.

| Key | Meaning |
|---|---|
| `color`, `alpha` | Base color (hex) and opacity |
| `texture`, `pixelated` | An image; `pixelated` uses nearest-neighbor sampling |
| `atlas_tile`, `atlas_columns`, `atlas_rows` | Cubes: show tile N (row-major) of a texture atlas on every face |
| `emissive`, `emissive_strength`, `emissive_texture` | 3D glow (bloom picks it up) |
| `unlit`, `roughness`, `metallic`, `double_sided` | 3D surface |
| `blend` | `alpha`, `additive`, `opaque`, or `mask` (cutout). In 3D, a shader material with `alpha` blend and `alpha = 1` still blends but also writes depth, so a solid shape hides its own far side |
| `shader`, `params` | Effect shader and up to four `[x, y, z, w]` parameters |

### Motion

- **`orbit`** circles `center` or another object (`parent`, which must be
  declared earlier) with `radius` (or `[rx, ry]`) every `period` seconds
  (negative = clockwise), starting at `phase` (a fraction of a turn).
  `speed_band` / `speed_react` speed it up with the music. In 2D, `behind_z`
  moves the object to that layer while it's on the far half of the orbit, and
  `perspective` shrinks it there. Together they fake a tilted 3D orbit that
  passes behind the sun. Satellites follow their parent's near/far state.
  `path_width` / `path_color` draw the orbit. In 3D, `tilt = [x°, z°]` tilts
  the orbit plane. With an orbit, `position` is an offset from the orbiting
  point, except that in 2D its `z` stays the object's own draw layer: a
  satellite follows only its parent's x/y. So Saturn's rings at z 1.9 and 2.2
  sit behind and in front of Saturn at 2.0.
- **`spin`** rotates at `speed` degrees per second around `axis`.

### Audio reactions

```toml
react = [
    { property = "scale", band = "bass", amount = 0.3 },
    { property = "position.y", band = "beat", amount = 0.9, smooth = 0.18 },
    { property = "param.3.w", band = "bar:5", amount = 1.2 },
]
```

- **`property`:** `scale`, `scale.x|y|z`, `position.x|y|z`,
  `rotation.x|y|z` (degrees), `brightness` (material), or `param.N.x|y|z|w`
  (an effect-shader parameter).
- **`band`:** `bass`, `mid`, `treble`, `energy`, `beat` (a pulse that jumps to
  1 on each detected beat and decays), `bar:N`, or `bar` inside a `[[ring]]`.
- **Result:** `amount × level` is added (position, rotation, param), or
  `1 + amount × level` multiplies (scale, brightness).
- **`smooth`:** the release time in seconds. Attack is instant.

### Rings

`[[ring]]` repeats a `template` object `count` times around a circle (or an
ellipse, `radius = [rx, ry]`). With `band = "bar"`, instance *i* reacts to its
own spectrum bar, so a ring becomes a radial spectrum. By default both halves
mirror at the bass (`mirror_bars = true`). Other keys:

- `spin`: degrees per second.
- `face_out`: turn each instance to face outward.
- `horizontal`: in 3D, lay the ring in the XZ plane.
- `tiles = [..]`: cycle atlas tiles across instances.
- `behind_z` / `perspective`: as for orbits.

## 3D: camera, environment, lights, terrain

```toml
[camera]
position = [30.0, 21.0, 30.0]
look_at = [0.0, 9.0, 0.0]
fov = 50.0
orbit = 0.07        # auto-orbit, rad/s
bob = 0.6           # rise with the bass
punch = 0.035       # FOV punch on the beat

[environment]
ambient_color = "#dfe9ff"
ambient_brightness = 160.0
fog_color = "#7fb2ff"
fog_start = 70.0
fog_end = 200.0
sky_shader = "sky"            # drawn on a dome around the scene
sky_params = [[...], [...]]

[[light]]
kind = "directional"          # or "point"
color = "#fff2d6"
intensity = 11000.0           # lux (directional) / lumens (point)
position = [30.0, 40.0, 16.0]
look_at = [0.0, 0.0, 0.0]
shadows = true
react = [{ band = "bass", amount = 0.35 }]
```

The 3D view renders under bava's 2D camera, which composites the HUD (and the
2D visualizer when `overlay_2d = true`) and applies bloom, tone mapping,
chromatic aberration and the vignette once for both.

### `[terrain]`

`[terrain]` is a block field whose column heights follow the spectrum. It is
meshed like a block-game chunk, so only visible faces are drawn, and it's
rebuilt only when a column moves by a whole block.

```toml
[terrain]
atlas = "textures/blocks.png"
atlas_columns = 8      # the atlas grid, each 1..1024
atlas_rows = 8
size = 32              # columns per side
block = 1.0            # world size of one block, above 0
base_height = 2
max_height = 15
mapping = "radial"     # bass in the middle | "radial_inverted" | "waterfall" (scrolling spectrogram)
roughness = 1.8        # static height noise, in blocks
edge_falloff = 0.75    # lower the rim: an island, not a bowl
fall = 0.35            # seconds for falling columns to settle
top = "grass_top"      # tile names → [terrain.tiles]
side = "grass_side"
under = "dirt"
under_depth = 3
deep = "stone"
peak_height = 11       # above this, the cap is `peak`
peak = "snow"
water_level = 3        # at or below, the cap is `shore`
shore = "sand"
ore_chance = 0.12      # deep blocks that become glowing ores
ores = ["diamond_ore", "gold_ore"]
ore_glow = 0.15
ore_pulse = 1.2        # extra glow on the beat

[terrain.tiles]
grass_top = 0
grass_side = 1
# ...
```

`water_level` only picks the `shore` caps; the terrain draws no water. For a
water surface, add a `plane` object just under `water_level × block`, as the
built-in `minecraft` scene does.

## Sounds

```toml
[sounds.place]
path = "sounds/place.ogg"
trigger = "click"      # start | loop | beat | spawn | impact | click | key
volume = 0.6           # linear gain, 0..2
jitter = 0.12          # ± random playback speed, 0..0.9
min_interval = 0.05    # seconds
every = 32             # trigger = "beat": every Nth beat
key = "j"              # trigger = "key": a-z, 0-9, f1-f12, space, enter, ...
```

An unknown `key` name is an error that lists the names accepted (the same
ones as `[gui] toggle_key`).

bava visualizes what the system plays, so a scene's own sounds reach the
visualizer too. Prefer user-driven triggers (`click`, `key`, `spawn` for
physics balls, `impact` for balls struck hard), and give `beat` sounds an
`every` divider. Offline renders never play scene sounds.

## Writing shaders

Effect shaders are WGSL fragment shaders. Import the bindings instead of
declaring them, so the same file works in 2D and 3D:

```wgsl
#import bevy_sprite::mesh2d_vertex_output::VertexOutput      // 2D
// #import bevy_pbr::forward_io::VertexOutput                // 3D
#import bava::fx::{fbm, noise2, hash21, PI, TAU}             // helpers
#import bava::fx_material::{fx, fx_texture, fx_sampler, palette, rim_radius}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let bass = fx.audio.x;
    let beat = fx.clock.x;
    let col = palette(in.uv.x) * (1.0 + 2.0 * bass + beat);
    return vec4<f32>(col, 1.0);
}
```

The uniform `fx` is refreshed every frame:

| Field | Contents |
|---|---|
| `fx.color` | Material color (linear, HDR). `brightness` reactions scale it |
| `fx.palette[4]` | The live gradient stops (album colors or the profile); `palette(t)` samples them |
| `fx.audio` | `(bass, mid, treble, energy)`, each roughly 0..1 |
| `fx.clock` | `(beat pulse 1→0, flow, seconds, stop count)`. The flow clock runs faster when the music is louder, so use it to animate |
| `fx.params[4]` | Your `params`. On the `[blob]` layers, `params[0]` is filled by bava: fill `(glow gain, opacity, style, loudest level)`, halo `(intensity, corona, falloff, quad half-size)`, backdrop `(stars, nebula, star gain, _)` |
| `fx.shape[16]` | 64 blob rim radii (px); `rim_radius(theta)` interpolates them |
| `fx.info` | `(viewport width, viewport height, blob base radius px, rotation)` |

On the blob fill, `uv.x` runs from 0 at the center to 1 on the rim. Bevy's
`circle` mesh maps UVs over the unit square, and `ring` meshes use
`uv = (radial 0..1, angle fraction from the top)`.

Output HDR colors (above 1.0) to bloom. For examples, see the built-in scenes'
`shaders/` and bava's own effect shaders in
[`crates/bava/src/vis/fx/shaders/`](../crates/bava/src/vis/fx/shaders).
