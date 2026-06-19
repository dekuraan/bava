# Flatpak packaging

Builds bava against the Freedesktop runtime. FFTW3 is built as a module (it is
not in the runtime); everything else bava links — libpulse, dbus, the Vulkan
loader, Wayland/X11 — comes from the runtime.

## Files

| File | Purpose |
| --- | --- |
| `io.github.dekuraan.bava.yml` | flatpak-builder manifest (fftw3 + bava modules) |
| `gen-cargo-sources.py` | Regenerates `cargo-sources.json` from `Cargo.lock` (offline, stdlib only) |
| `cargo-sources.json` | Vendored crates.io sources for an offline cargo build — **generated** |
| `io.github.dekuraan.bava.desktop` | Desktop entry |
| `io.github.dekuraan.bava.metainfo.xml` | AppStream metadata (required by Flathub) |
| `io.github.dekuraan.bava.svg` | App icon |

## Build & run locally

```sh
# 1. Refresh the vendored crate list whenever Cargo.lock changes.
python3 flatpak/gen-cargo-sources.py

# 2. Install the runtime, SDK, and the Rust SDK extension (once).
flatpak install -y flathub \
    org.freedesktop.Platform//24.08 \
    org.freedesktop.Sdk//24.08 \
    org.freedesktop.Sdk.Extension.rust-stable//24.08

# 3. Build and install into the user installation.
flatpak-builder --user --install --force-clean \
    build-dir flatpak/io.github.dekuraan.bava.yml

# 4. Run.
flatpak run io.github.dekuraan.bava
```

## Sandbox permissions

| Permission | Why |
| --- | --- |
| `--socket=wayland` / `--socket=fallback-x11` / `--share=ipc` | Display |
| `--device=dri` | GPU for the Vulkan/wgpu renderer |
| `--socket=pulseaudio` | Capture the sink **monitor** source (the portal audio path doesn't expose it) |
| `--share=network` | Album-art HTTP fetch |
| `--talk-name=org.mpris.MediaPlayer2.*` | Read now-playing metadata from other players |

## Notes

- **Config path.** Inside the sandbox `~/.config/bava` resolves to
  `~/.var/app/io.github.dekuraan.bava/config/bava/` — separate from a native
  install. Foreground/background image layers configured in `[vis]` must point
  at paths the sandbox can read (your home dir is **not** auto-shared; add
  `--filesystem=...` if you need it).
- **Album art** still depends on the *player* exposing `mpris:artUrl`
  (spotifyd does; browsers generally don't), same as a native build.
- **Before Flathub submission:** commit a real screenshot and update the
  `<screenshot>` URL in the metainfo, and host this manifest in a
  `flathub/io.github.dekuraan.bava` repo. The app-id assumes you own the
  `github.com/dekuraan/bava` repo (required for the `io.github.*` namespace).
- **`cargo-sources.json` is generated.** Re-run `gen-cargo-sources.py` after any
  dependency change; CI on Flathub builds offline, so a stale list fails the build.
