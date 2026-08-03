# App icon

One source of truth — [`gen-icons.py`](gen-icons.py) — draws the artwork (the
deep-violet backdrop and five bloom-lit spectrum bars in bava's cyan→magenta
foreground gradient) in a normalised 1024×1024 box, then wraps it in whatever
shape each platform expects and writes every derived file:

| Output | Shape | Consumed by |
| --- | --- | --- |
| `flatpak/io.github.dekuraan.bava.svg` | full-bleed rounded square | Flatpak/Flathub → `hicolor/scalable/apps/` (see the manifest) |
| `packaging/icon/bava.svg` | same, kept here as the canonical copy | anything else Linux/web |
| `packaging/icon/bava-macos.svg` | 824×824 squircle in a 1024 canvas + drop shadow | source for the `.icns`; also handy for `sips`/Preview on a Mac |
| `packaging/macos/bava.icns` | ditto, rasterised | macOS `.app` bundles (`CFBundleIconFile`) |
| `packaging/icon/bava-512.png` | full-bleed rounded square | READMEs, release pages |

## Regenerate

```sh
python3 packaging/icon/gen-icons.py                 # rewrites every output above
python3 packaging/icon/gen-icons.py --keep-iconset  # also leaves packaging/macos/bava.iconset/
```

Needs one of `resvg` / `rsvg-convert` / `inkscape` on `PATH`, or the `cairosvg`
Python module; everything else is stdlib. **The SVGs are generated too** — edit
`gen-icons.py` (the palette and geometry constants live at the top), not them.

## Design notes

- **macOS uses Apple's grid**, not a full-bleed square: the body is 824×824
  centred in a 1024 canvas, leaving the 100px margin the Dock, Finder, and
  Launchpad assume, with the shadow baked into the asset like every system
  icon. The corner is a true superellipse (`|x/a|⁵ + |y/a|⁵ = 1`, sampled as a
  sub-pixel polyline), not a plain rounded rect, so it sits flush next to
  first-party icons.
- **≤32px gets a three-bar variant** with the bloom and top sheen dropped. Five
  bars at 16px land on ~1.5 device pixels each and turn to mush; three wider
  ones keep the same silhouette and stay legible in the menu bar and Finder
  lists. The threshold lives in `main()`.
- **`.icns` is written by hand** (`write_icns`) rather than by `iconutil`, which
  is macOS-only — the icon has to be reproducible from the Linux dev box. The
  container holds the same ten `OSType`s `iconutil -c icns` emits (`icp4`,
  `ic11`, `icp5`, `ic12`, `ic07`, `ic13`, `ic08`, `ic14`, `ic09`, `ic10`) with
  PNG payloads, which macOS has read since 10.7.

## Using the `.icns` in a macOS bundle

bava has no `.app` bundling yet. When it lands, the icon goes in as:

```
bava.app/Contents/
  Info.plist          # CFBundleIconFile = bava   (extension optional)
  MacOS/bava
  Resources/bava.icns
```

`cargo-bundle` and `cargo-packager` both take it via `icon = ["packaging/macos/bava.icns"]`.
On a Mac the iconset can also be repacked natively:

```sh
python3 packaging/icon/gen-icons.py --keep-iconset
iconutil -c icns packaging/macos/bava.iconset -o packaging/macos/bava.icns
```
