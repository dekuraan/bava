# Packaging

Every distribution channel bava ships through, what state it's in, and what it
takes to publish. Artwork for all of them comes from one generator —
[`icon/README.md`](icon/README.md).

| Channel | Files | Gatekeeper | State |
| --- | --- | --- | --- |
| **AUR** `bava` | [`aur/bava/`](aur/bava) | none (AUR account) | ready — published by `publish.sh`, then automatically on every tag |
| **AUR** `bava-git` | [`aur/bava-git/`](aur/bava-git) | none (AUR account) | ready — publish once, `pkgver()` tracks HEAD afterwards |
| **Flathub** | [`../flatpak/`](../flatpak) | Flathub review PR | builds locally; needs a submittable `sources:` before the PR (see below) |
| **.deb** | `[package.metadata.deb]` in `crates/bava/Cargo.toml` | none | ready — built and attached on every `v*` tag |
| **.rpm** | `[package.metadata.generate-rpm]` in the same file | none | ready — same |
| **AppImage** | [`appimage/build-appimage.sh`](appimage/build-appimage.sh) | none | ready — same |
| **Nix** | [`../flake.nix`](../flake.nix), [`nix/package.nix`](nix/package.nix) | none (nixpkgs PR is optional) | written, **not yet evaluated** — no nix on the dev box |
| **Snap Store** | [`../snap/snapcraft.yaml`](../snap/snapcraft.yaml) | Snapcraft account; manual review only if you go classic | written, **not yet built** — needs LXD + snapcraft |
| **macOS** | [`macos/bava.icns`](macos) | — | icon only; no `.app` bundling yet |
| **Web/wasm** | [`web/`](web), [`../crates/bava/web/`](../crates/bava/web) | — | **live** — built by trunk and deployed to GitHub Pages on every push to `main` |
| **TV app stores** | — | — | not viable, see [below](#tv-app-stores) |

## AUR

Two packages: `bava` builds the tagged release tarball, `bava-git` builds HEAD.
Both install the binary plus the `.desktop`, icon, and AppStream metainfo.

```sh
packaging/aur/publish.sh bava-git          # one-time
packaging/aur/publish.sh bava              # one-time; CI handles it after that
packaging/aur/publish.sh bava 0.4.0        # manual bump: rewrites pkgver + sha256sums
```

Publishing needs an [AUR account](https://aur.archlinux.org/register) with your
SSH public key registered. After the first push, the `publish-aur` job in
`release.yml` updates `bava` on every `v*` tag — it stays a no-op until you add
three repo secrets: `AUR_USERNAME`, `AUR_EMAIL`, `AUR_SSH_PRIVATE_KEY`.

**`options=('!lto')` is load-bearing.** makepkg's default `lto` option adds
`-Clinker-plugin-lto`, which makes rustc emit bitcode-only objects while the C
shims our dependencies build (libspa's format wrappers, ring's and blake3's
assembly) stay native. The final link then dies on undefined
`spa_format_parse_libspa_rs` / `ring_core_*_OPENSSL_cpuid_setup`. `profile.release`
already runs thin LTO inside cargo, so disabling makepkg's costs nothing.

Verify a change before pushing:

```sh
cd packaging/aur/bava && makepkg -f --nodeps    # full build + package
makepkg --printsrcinfo > .SRCINFO               # publish.sh does this for you
```

## Flathub

The manifest, desktop entry, metainfo, and icon live in [`../flatpak/`](../flatpak);
that README covers building and running locally. Submitting:

1. **Swap `sources:` for something Flathub can fetch.** The in-tree manifest
   uses `type: dir, path: ..`, which only resolves when building from a
   checkout of *this* repo. Flathub's buildbot checks out only the flathub
   repo, so a submission needs the released tarball instead:

   ```yaml
   sources:
     - type: archive
       url: https://github.com/dekuraan/bava/archive/refs/tags/v0.4.0.tar.gz
       sha256: <sha256sum of that exact tarball>
     - cargo-sources.json
   ```

   Both values depend on the tag, so this step can only happen after a release
   is tagged — keep the `type: dir` version in-tree for local builds.
2. Fork [flathub/flathub](https://github.com/flathub/flathub), branch
   `new-pr`, add `io.github.dekuraan.bava.yml` (plus `cargo-sources.json`), open
   a PR against the `new-pr` branch. A bot builds it; a reviewer follows up.
3. Re-run `python3 flatpak/gen-cargo-sources.py` after **any** dependency
   change — Flathub builds offline, and a stale vendored list fails the build.

The `io.github.*` app-id requires you to own `github.com/dekuraan/bava`, which
you do. `docs/screenshot.png` is already committed and the metainfo points at
its raw GitHub URL, so the screenshot requirement is met.

**Two things the manifest learned the hard way**, both found by building it
rather than reading it:

- **The runtime is pinned to 25.08 for its rustc, not its libraries.**
  `rust-stable//24.08` ships rustc 1.89 and Bevy 0.19 needs 1.95, so a 24.08
  build dies at `cargo build` before linking anything. 25.08 ships 1.97.1.
- **The build is `--no-default-features`**, like the AppImage. The default
  build's `libspa-sys` runs bindgen over the PipeWire headers, and neither
  libclang nor those headers are in the Sdk. Dropping the native PipeWire
  backend avoids that and is the right call for a sandboxed app anyway —
  PulseAudio reaches PipeWire hosts through pipewire-pulse.

## .deb / .rpm / AppImage

No review queue, no account: the `package-linux` job builds all three on every
`v*` tag and attaches them to the GitHub release. Locally:

```sh
# One build feeds all three.
cargo build --release -p bava --no-default-features
cargo deb -p bava --no-build                 # target/debian/bava_<ver>-1_amd64.deb
cargo generate-rpm -p crates/bava            # target/generate-rpm/bava-<ver>-1.x86_64.rpm
packaging/appimage/build-appimage.sh target/release/bava 0.4.0
```

Three deliberate choices:

- The job is pinned to the **oldest LTS runner** GitHub still offers. glibc is
  forward- but not backward-compatible, so a package built on 24.04 refuses to
  start on 22.04. Bump the pin when the image retires; the compatibility floor
  rises with it.
- **All three ship the PulseAudio-only build**, and that follows from the pin.
  22.04 carries PipeWire 0.3.48 headers from 2022, which the `libspa` crate no
  longer compiles against (`cannot find function spa_meta_first`, `no field
  flags on type spa_video_info_raw`). Keeping the native backend would mean
  building on 24.04, whose glibc 2.39 locks out Debian 12 (2.36) and Ubuntu
  22.04 — the machines this job exists to serve. The PulseAudio path reaches
  PipeWire hosts through pipewire-pulse, so the trade buys installability for a
  backend most users arrive at indirectly anyway. For the native backend: the
  `bava-linux-x86_64` release binary, the AUR package, or a source build.
- Bundling a PipeWire *client* library into a portable archive would be a trap
  independently of the above — it talks to whatever daemon the host runs, and
  skewed client/daemon pairs misbehave.

The packaging tools are **built from source** in CI rather than fetched as
prebuilts: `taiki-e/install-action` now serves a `cargo-deb` needing GLIBC_2.39,
which the 22.04 pin cannot satisfy, so the tooling — not the output — was what
failed to start.

`cargo deb`'s `$auto` and `cargo generate-rpm`'s auto-req both read the built
ELF, so the shared-library dependency lists can't drift from what we link. On a
non-Debian dev box `$auto` silently produces nothing (no `dpkg-shlibdeps`) — the
dependency list is only correct when built on the CI runner.

## Nix

```sh
nix run github:dekuraan/bava      # once the flake is on the default branch
nix build .#bava
nix develop                       # dev shell with the runtime libs on LD_LIBRARY_PATH
```

`packaging/nix/package.nix` is deliberately a standalone `callPackage`
expression so it can move into nixpkgs as `pkgs/by-name/ba/bava/package.nix`
nearly verbatim — swap the `src` default for a `fetchFromGitHub` and
`cargoLock.lockFile` for a `cargoHash`. Bevy dlopens vulkan-loader, wayland, and
libxkbcommon, so `postFixup` patchelfs them onto the binary's rpath; without
that the app dies at window creation.

**Unverified.** Neither file has been evaluated — there's no nix on the machine
they were written on. Expect to iterate on `nix build .#bava` once.

## Snap

```sh
sudo snap install snapcraft --classic
snapcraft                                    # builds in LXD
sudo snap install --dangerous ./bava_*.snap
snapcraft upload --release=stable bava_*.snap
```

Two confinement caveats worth knowing before you publish:

- **`audio-record` is not auto-connected.** Capturing the sink monitor needs
  `sudo snap connect bava:audio-record`; the snap sees silence until then. The
  install instructions say so, and the store description repeats it.
- **MPRIS now-playing only reaches snap-packaged players.** The `mpris` plug
  connects to `mpris` *slots*, which only snaps provide. A deb/flatpak Spotify
  or Firefox is invisible to it, so the now-playing HUD stays empty. Fixing that
  properly means `confinement: classic`, which needs manual store review and a
  written justification. Strict-but-degraded is the honest default; revisit if
  users complain.

**Unbuilt.** snapcraft needs LXD and a Ubuntu-ish host; this was written on
Arch. Expect one round of fixes on the first `snapcraft` run.

## TV app stores

Not viable, and it isn't a packaging problem — bava needs **system-wide loopback
capture**, and no TV platform exposes it:

| Platform | Blocker |
| --- | --- |
| **tvOS** | No loopback API at all. Apps get their own audio and the mic (and tvOS has no mic on most hardware). Also Swift/Metal + App Store review; a Rust/wgpu binary isn't a shippable form. |
| **Android TV** | `AudioPlaybackCapture` (API 29+) only records apps that set `allowAudioPlaybackCapture=true`. Netflix, Spotify, YouTube, and every DRM'd player set it false — which is the entire use case. |
| **Roku** | BrightScript/SceneGraph only. No native code, no audio capture API. |
| **webOS / Tizen** | Sandboxed web apps. `getDisplayMedia` isn't available with system audio; no loopback. |

The nearest workable thing is an Android TV build that visualizes the **mic or a
line-in dongle** instead of loopback — a real port (Bevy's Android target, a new
`AudioCapture` backend, no MPRIS), not a packaging step. Scope it separately if
it ever matters.

The same reasoning is why the web build was a real port rather than a packaging
job — it could not reuse a capture backend, so the page pushes tab-shared audio
into the wasm module instead. That port is done and deployed; see
[`web/README.md`](web/README.md) and [`../docs/WEB.md`](../docs/WEB.md).

## Web

Nothing to submit and no gatekeeper — the site is static, and `pages.yml`
publishes `dist/` on every push to `main`.

```sh
trunk serve                                # http://localhost:8080
trunk build --release --public-url /bava/  # exactly what CI deploys
```

The `--public-url` matters: a project page is served from
`https://<user>.github.io/<repo>/`, and without the prefix the page requests the
wasm at the domain root and gets the 404 page back. Pages must be set to the
"GitHub Actions" source once in repo settings.
