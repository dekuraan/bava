# Solar-system scene assets — attribution

Sounds are **CC0 1.0** (Kenney; license verified 2026-09-28 from each zip's `License.txt`,
"License: (Creative Commons Zero, CC0)", and the kenney.nl asset pages). Textures are
**NASA public-domain** imagery (US Government works); credit lines are courtesy, not a
license condition. Both maps are 2:1 equirectangular (longitude −180°…180° left→right,
north up) at 512×256.

## Files

| File | Source (page — download) | Author | License | Modifications |
|---|---|---|---|---|
| sounds/blip.ogg | https://kenney.nl/assets/interface-sounds — https://kenney.nl/media/pages/assets/interface-sounds/fa43c1dd4d-1677589452/kenney_interface-sounds.zip | Kenney (www.kenney.nl) | [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) | `Audio/glass_001.ogg` (short glassy ping, ~1.9 kHz); downmixed to mono ((L+R)/2), peak-normalized to −1 dBFS, re-encoded Ogg Vorbis (`libvorbis -q:a 4`, 44.1 kHz), metadata stripped; length unchanged |
| sounds/whoosh.ogg | https://kenney.nl/assets/sci-fi-sounds — https://kenney.nl/media/pages/assets/sci-fi-sounds/6b296f9ecf-1677589334/kenney_sci-fi-sounds.zip | Kenney (www.kenney.nl); shaped for bava | [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) | `Audio/thrusterFire_000.ogg` (5 s noise burst): took 0.8 s from t=1.6 s, mono, applied a swept low-pass (2× one-pole, cutoff 300 Hz → 3.5 kHz at 0.32 s → 450 Hz) and a sin²/cos² swell envelope peaking at 0.32 s, normalized to −1.5 dBFS, encoded Ogg Vorbis (`libvorbis -q:a 4`, 44.1 kHz mono). Derived work also released CC0. |
| textures/moon.jpg | https://svs.gsfc.nasa.gov/4720 (CGI Moon Kit) — https://svs.gsfc.nasa.gov/vis/a000000/a004700/a004720/lroc_color_2k.jpg | NASA's Scientific Visualization Studio (Ernie Wright, USRA; Noah Petro, NASA/GSFC); LRO LROC WAC mosaic | Public domain (US Government work). SVS FAQ, https://svs.gsfc.nasa.gov/help/ : "All of our content is in the public domain (unless otherwise noted)"; credit requested: "NASA's Scientific Visualization Studio". | Equirectangular color map; downscaled 2048×1024 → 512×256 (Lanczos), re-saved JPEG q85 (4:4:4). |
| textures/earth.jpg | https://science.nasa.gov/earth/earth-observatory/blue-marble-next-generation/base-topography-bathymetry/ — https://assets.science.nasa.gov/content/dam/science/esd/eo/images/bmng/bmng-topography-bathymetry/july/world.topo.bathy.200407.3x5400x2700.jpg | NASA Earth Observatory — Blue Marble: Next Generation, July 2004, topography + bathymetry | Public domain (US Government work). NASA Images and Media Usage Guidelines, https://www.nasa.gov/nasa-brand-center/images-and-media/ : "NASA content – images, audio, video, and media files used in the rendition of 3-dimensional models, such as texture maps … – generally are not subject to copyright in the United States." No per-page copyright notice. Credit: NASA Earth Observatory. (Do not imply NASA endorsement; NASA logos/insignia are not covered.) | Equirectangular map; downscaled 5400×2700 → 512×256 (Lanczos), re-saved JPEG q85 (4:4:4). |
