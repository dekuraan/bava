# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/dekuraan/bava/releases/tag/v0.2.0) - 2026-07-02

### Added

- live balls follow dynamic album-color fade
- F3 FPS/ball overlay, right-click ball spray, up-to-5 dynamic colors
- harden audio capture + add native PipeWire backend
- v0.0.2 — full vis settings, 4-direction layout, image layers, expanded egui editor

### Fixed

- harden config/capture/vis against crashes and cleanups (v0.1.1)
- lift planet balls onto the spike envelope, not the crevice floor
- eject balls pinned to the outside of the planet rim
- stop WASAPI/CoreAudio read() zero-filling mid-playback
- correct PipeWire bar smoothing + follow the active sink
- throttle F3 FPS overlay and show averaged data instead of per-frame raw
- rebuild cavacore at the negotiated capture rate so smoothing is correct
- planet blob balls stick when rotation is set ([#7](https://github.com/dekuraan/bava/pull/7))
- resolve macOS dead-code error and Windows fftw3.lib linker failure
- resolve Windows dead-code errors and macOS fftw3 include path

### Other

- Merge pull request #21 from dekuraan/release-plz-2026-06-30T16-28-23Z
- release v0.2.0
- rewrite cavacore in pure Rust, drop C/FFTW
- Merge pull request #13 from dekuraan/release-plz-2026-06-30T04-38-31Z
- *(bava)* release v0.1.0
- *(deps)* Bump bevy_egui from 0.40.1 to 0.41.0 ([#19](https://github.com/dekuraan/bava/pull/19))
- Fix slab-allocator use-after-free from zero-vertex stroke meshes
- Fix ureq 3.x response API in Linux album-art fetch
- Merge branch 'main' into dependabot/cargo/ureq-3.3.0
- Merge pull request #15 from dekuraan/fix/jellyfin-desktop-data-uri-art
- Merge pull request #14 from dekuraan/worktree-ballsssss
- upgrade to Bevy 0.19 (avian2d 0.7, bevy_egui 0.40)
- bump bava to 0.1.0
- Merge pull request #6 from dekuraan/worktree-audio-hardening
- rename mpris module to now_playing
- fix stale cross-platform references in CLAUDE.md and doc comments
- Release v0.0.1
- Merge pull request #1 from dekuraan/cross-platform-support
- Address code-review findings + proper cross-platform build infrastructure
- Merge branch 'main' into worktree-macos-support
- Fix code-review findings: physics panic, mirror, per-shape circles
- Tree-shake Bevy/avian/egui features; add configurable tonemapping
- Add fading color trails behind balls
- Add avian collider debug draw (F3)
- Match physics colliders to the rendered meshes per mode
- License under GPL-3.0-or-later
- Fix balls sticking to the planet blob rim
- Merge branch 'main' into worktree-physics
- Guard ball spawn against egui clicks; debounce gravity write
- Merge branch 'main' into worktree-physics
- Add planet mode: radial gravity + blob collider + orbital spawn
- Unstick balls swallowed by a rising surface; tighten floor despawn
- Refactor bar physics to a smooth heightfield with slope-normal launch
- Merge branch 'main' into worktree-physics
- Merge branch 'main' into worktree-physics
- Add Box-mode 2D physics: click-spawn balls, bars bounce them (avian2d)
- Add circle visualizer with live style toggle
- Monstercat/Cavalier styling, album art, and steady-chunk cava fix
- Drive cavacore at the render rate
- wip
- Initial scaffold: cavacore-driven Bevy music visualizer

## [0.1.1](https://github.com/dekuraan/bava/releases/tag/v0.1.1) - 2026-07-01

### Added

- live balls follow dynamic album-color fade
- F3 FPS/ball overlay, right-click ball spray, up-to-5 dynamic colors
- harden audio capture + add native PipeWire backend
- v0.0.2 — full vis settings, 4-direction layout, image layers, expanded egui editor

### Fixed

- harden config/capture/vis against crashes and cleanups (v0.1.1)
- lift planet balls onto the spike envelope, not the crevice floor
- eject balls pinned to the outside of the planet rim
- stop WASAPI/CoreAudio read() zero-filling mid-playback
- correct PipeWire bar smoothing + follow the active sink
- throttle F3 FPS overlay and show averaged data instead of per-frame raw
- rebuild cavacore at the negotiated capture rate so smoothing is correct
- planet blob balls stick when rotation is set ([#7](https://github.com/dekuraan/bava/pull/7))
- resolve macOS dead-code error and Windows fftw3.lib linker failure
- resolve Windows dead-code errors and macOS fftw3 include path

### Other

- Merge pull request #13 from dekuraan/release-plz-2026-06-30T04-38-31Z
- *(bava)* release v0.1.0
- *(deps)* Bump bevy_egui from 0.40.1 to 0.41.0 ([#19](https://github.com/dekuraan/bava/pull/19))
- Fix slab-allocator use-after-free from zero-vertex stroke meshes
- Fix ureq 3.x response API in Linux album-art fetch
- Merge branch 'main' into dependabot/cargo/ureq-3.3.0
- Merge pull request #15 from dekuraan/fix/jellyfin-desktop-data-uri-art
- Merge pull request #14 from dekuraan/worktree-ballsssss
- upgrade to Bevy 0.19 (avian2d 0.7, bevy_egui 0.40)
- bump bava to 0.1.0
- Merge pull request #6 from dekuraan/worktree-audio-hardening
- rename mpris module to now_playing
- fix stale cross-platform references in CLAUDE.md and doc comments
- Release v0.0.1
- Merge pull request #1 from dekuraan/cross-platform-support
- Address code-review findings + proper cross-platform build infrastructure
- Merge branch 'main' into worktree-macos-support
- Fix code-review findings: physics panic, mirror, per-shape circles
- Tree-shake Bevy/avian/egui features; add configurable tonemapping
- Add fading color trails behind balls
- Add avian collider debug draw (F3)
- Match physics colliders to the rendered meshes per mode
- License under GPL-3.0-or-later
- Fix balls sticking to the planet blob rim
- Merge branch 'main' into worktree-physics
- Guard ball spawn against egui clicks; debounce gravity write
- Merge branch 'main' into worktree-physics
- Add planet mode: radial gravity + blob collider + orbital spawn
- Unstick balls swallowed by a rising surface; tighten floor despawn
- Refactor bar physics to a smooth heightfield with slope-normal launch
- Merge branch 'main' into worktree-physics
- Merge branch 'main' into worktree-physics
- Add Box-mode 2D physics: click-spawn balls, bars bounce them (avian2d)
- Add circle visualizer with live style toggle
- Monstercat/Cavalier styling, album art, and steady-chunk cava fix
- Drive cavacore at the render rate
- wip
- Initial scaffold: cavacore-driven Bevy music visualizer

## [0.1.0](https://github.com/dekuraan/bava/releases/tag/v0.1.0) - 2026-06-30

### Added

- live balls follow dynamic album-color fade
- F3 FPS/ball overlay, right-click ball spray, up-to-5 dynamic colors
- harden audio capture + add native PipeWire backend
- v0.0.2 — full vis settings, 4-direction layout, image layers, expanded egui editor

### Fixed

- lift planet balls onto the spike envelope, not the crevice floor
- eject balls pinned to the outside of the planet rim
- stop WASAPI/CoreAudio read() zero-filling mid-playback
- correct PipeWire bar smoothing + follow the active sink
- throttle F3 FPS overlay and show averaged data instead of per-frame raw
- rebuild cavacore at the negotiated capture rate so smoothing is correct
- planet blob balls stick when rotation is set ([#7](https://github.com/dekuraan/bava/pull/7))
- resolve macOS dead-code error and Windows fftw3.lib linker failure
- resolve Windows dead-code errors and macOS fftw3 include path

### Other

- *(deps)* Bump bevy_egui from 0.40.1 to 0.41.0 ([#19](https://github.com/dekuraan/bava/pull/19))
- Fix slab-allocator use-after-free from zero-vertex stroke meshes
- Fix ureq 3.x response API in Linux album-art fetch
- Merge branch 'main' into dependabot/cargo/ureq-3.3.0
- Merge pull request #15 from dekuraan/fix/jellyfin-desktop-data-uri-art
- Merge pull request #14 from dekuraan/worktree-ballsssss
- upgrade to Bevy 0.19 (avian2d 0.7, bevy_egui 0.40)
- bump bava to 0.1.0
- Merge pull request #6 from dekuraan/worktree-audio-hardening
- rename mpris module to now_playing
- fix stale cross-platform references in CLAUDE.md and doc comments
- Release v0.0.1
- Merge pull request #1 from dekuraan/cross-platform-support
- Address code-review findings + proper cross-platform build infrastructure
- Merge branch 'main' into worktree-macos-support
- Fix code-review findings: physics panic, mirror, per-shape circles
- Tree-shake Bevy/avian/egui features; add configurable tonemapping
- Add fading color trails behind balls
- Add avian collider debug draw (F3)
- Match physics colliders to the rendered meshes per mode
- License under GPL-3.0-or-later
- Fix balls sticking to the planet blob rim
- Merge branch 'main' into worktree-physics
- Guard ball spawn against egui clicks; debounce gravity write
- Merge branch 'main' into worktree-physics
- Add planet mode: radial gravity + blob collider + orbital spawn
- Unstick balls swallowed by a rising surface; tighten floor despawn
- Refactor bar physics to a smooth heightfield with slope-normal launch
- Merge branch 'main' into worktree-physics
- Merge branch 'main' into worktree-physics
- Add Box-mode 2D physics: click-spawn balls, bars bounce them (avian2d)
- Add circle visualizer with live style toggle
- Monstercat/Cavalier styling, album art, and steady-chunk cava fix
- Drive cavacore at the render rate
- wip
- Initial scaffold: cavacore-driven Bevy music visualizer
