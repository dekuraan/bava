// SPDX-License-Identifier: GPL-3.0-or-later
use std::path::PathBuf;

fn main() {
    let vendor = PathBuf::from("vendor/cava");
    let src = vendor.join("cavacore.c");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-changed={}", vendor.join("cavacore.h").display());

    // Compile the vendored cavacore translation unit. cavacore.c only depends
    // on fftw3 + libm; no generated config.h is required (the config.h include
    // some distro packages add is build-system cruft, not part of upstream).
    let mut build = cc::Build::new();
    build.file(&src).include(&vendor).warnings(false);

    // Pick up non-standard fftw3 prefix (e.g. /opt/homebrew on macOS/ARM), but
    // only for a native build: the host `pkg-config` reports host include/lib
    // paths that are wrong for a cross target (e.g. Linux→Windows). When cross,
    // rely on the caller's CFLAGS/headers and the `-lfftw3` fallback instead.
    let probe_pkg_config = !pkg_config::cross_compiling();
    if probe_pkg_config && let Ok(paths) = pkg_config::include_paths("fftw3") {
        for p in &paths {
            build.include(p);
        }
    }

    build.compile("cavacore");

    // cavacore links against the double-precision FFTW and libm.
    // Prefer pkg-config so we pick up non-standard prefixes, but fall back to
    // a plain `-lfftw3 -lm` which is correct on every mainstream distro.
    if !probe_pkg_config || pkg_config::link_lib("fftw3").is_err() {
        println!("cargo:rustc-link-lib=fftw3");
    }
    // libm is a separate library on Unix; on Windows the math functions are
    // part of the C runtime so there is no m.lib to link against.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        println!("cargo:rustc-link-lib=m");
    }
}

// Minimal inline pkg-config shim so we don't pull a dependency just for the
// happy-path probe. Returns Ok if `pkg-config` succeeds.
mod pkg_config {
    use std::{path::PathBuf, process::Command};

    /// True when building for a target different from the host, unless the caller
    /// opted in with `PKG_CONFIG_ALLOW_CROSS=1`. The host `pkg-config` only knows
    /// host paths, so probing it for a cross target yields wrong include/lib dirs.
    pub fn cross_compiling() -> bool {
        if std::env::var("PKG_CONFIG_ALLOW_CROSS").as_deref() == Ok("1") {
            return false;
        }
        match (std::env::var("HOST"), std::env::var("TARGET")) {
            (Ok(host), Ok(target)) => host != target,
            _ => false,
        }
    }

    /// Returns include paths from `pkg-config --cflags <name>`.
    pub fn include_paths(name: &str) -> Result<Vec<PathBuf>, ()> {
        let out = Command::new("pkg-config")
            .args(["--cflags", name])
            .output()
            .map_err(|_| ())?;
        if !out.status.success() {
            return Err(());
        }
        let flags = String::from_utf8_lossy(&out.stdout);
        let paths = flags
            .split_whitespace()
            .filter_map(|f| f.strip_prefix("-I"))
            .map(PathBuf::from)
            .collect();
        Ok(paths)
    }

    /// Emits link flags from `pkg-config --libs <name>`.
    pub fn link_lib(name: &str) -> Result<(), ()> {
        let out = Command::new("pkg-config")
            .args(["--libs", name])
            .output()
            .map_err(|_| ())?;
        if !out.status.success() {
            return Err(());
        }
        let flags = String::from_utf8_lossy(&out.stdout);
        for flag in flags.split_whitespace() {
            if let Some(lib) = flag.strip_prefix("-l") {
                println!("cargo:rustc-link-lib={lib}");
            } else if let Some(path) = flag.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={path}");
            }
        }
        Ok(())
    }
}
