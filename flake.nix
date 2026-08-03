# SPDX-License-Identifier: MIT OR Apache-2.0
{
  description = "A music visualizer driven by cavacore, MPRIS, and PipeWire/PulseAudio";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        # `src = self` hands the derivation this flake's store path, which is
        # what makes `cargoLock.lockFile` resolvable without IFD.
        bava = pkgs.callPackage ./packaging/nix/package.nix { src = self; };
        default = bava;
      });

      apps = forAllSystems (pkgs: rec {
        bava = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.bava}/bin/bava";
        };
        default = bava;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.bava ];
          packages = with pkgs; [
            cargo
            rustc
            rust-analyzer
            clippy
            rustfmt
            ffmpeg # `--input song.mp3 --out video.mp4`
          ];
          # `cargo run` links against the store copies but dlopens these at
          # runtime; without the path, Bevy dies at window creation.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            pkgs.vulkan-loader
            pkgs.wayland
            pkgs.libxkbcommon
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
