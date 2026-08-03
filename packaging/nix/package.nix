# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Kept as a standalone callPackage expression (rather than living inside
# flake.nix) so it can be dropped into nixpkgs as pkgs/by-name/ba/bava/
# package.nix almost verbatim. To upstream it: replace the `src` default with a
# fetchFromGitHub, and swap `cargoLock.lockFile` for a `cargoHash` — nixpkgs
# can't read a lockfile out of a fetched source without import-from-derivation.
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  # Capture + session backends.
  libpulseaudio,
  pipewire,
  dbus,
  # Windowing / input / GPU. Bevy dlopens vulkan-loader, wayland, and
  # libxkbcommon at runtime, so they need an rpath entry, not just a link.
  vulkan-loader,
  wayland,
  libxkbcommon,
  xorg,
  # Only needed for `--input song.mp3 --out video.mp4`; off by default because
  # ffmpeg is a large closure to force on someone who just wants the visualizer.
  ffmpeg,
  withFfmpeg ? false,
  src ? ../..,
  version ? "0.3.0",
}:

let
  runtimeLibs = [
    vulkan-loader
    wayland
    libxkbcommon
  ];
in
rustPlatform.buildRustPackage {
  pname = "bava";
  inherit version src;

  cargoLock.lockFile = src + "/Cargo.lock";

  nativeBuildInputs = [
    pkg-config
    # libspa-sys (pulled in by the default `pipewire` feature) runs bindgen over
    # the PipeWire headers; the hook is what points it at libclang.
    rustPlatform.bindgenHook
  ] ++ lib.optional withFfmpeg makeWrapper;

  buildInputs = [
    libpulseaudio
    pipewire
    dbus
    wayland
    libxkbcommon
    vulkan-loader
    xorg.libX11
    xorg.libXcursor
    xorg.libXrandr
    xorg.libXi
  ];

  cargoBuildFlags = [
    "--package"
    "bava"
  ];

  # cavacore-rs' suite only. bava's own tests are headless but spin up the asset
  # and physics stacks, which is more than a sandboxed build should take on.
  cargoTestFlags = [
    "--package"
    "cavacore-rs"
  ];

  postInstall = ''
    install -Dm644 flatpak/io.github.dekuraan.bava.desktop \
      $out/share/applications/io.github.dekuraan.bava.desktop
    install -Dm644 flatpak/io.github.dekuraan.bava.svg \
      $out/share/icons/hicolor/scalable/apps/io.github.dekuraan.bava.svg
    install -Dm644 flatpak/io.github.dekuraan.bava.metainfo.xml \
      $out/share/metainfo/io.github.dekuraan.bava.metainfo.xml
  '';

  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/bava
  ''
  + lib.optionalString withFfmpeg ''
    wrapProgram $out/bin/bava --prefix PATH : ${lib.makeBinPath [ ffmpeg ]}
  '';

  meta = {
    description = "Music visualizer driven by cavacore, MPRIS, and PipeWire/PulseAudio";
    homepage = "https://github.com/dekuraan/bava";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "bava";
    platforms = lib.platforms.linux;
  };
}
