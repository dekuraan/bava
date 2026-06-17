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
    cc::Build::new()
        .file(&src)
        .include(&vendor)
        .warnings(false)
        .compile("cavacore");

    // cavacore links against the double-precision FFTW and libm.
    // Prefer pkg-config so we pick up non-standard prefixes, but fall back to
    // a plain `-lfftw3 -lm` which is correct on every mainstream distro.
    if pkg_config::probe_library("fftw3").is_err() {
        println!("cargo:rustc-link-lib=fftw3");
    }
    println!("cargo:rustc-link-lib=m");
}

// Minimal inline pkg-config shim so we don't pull a dependency just for the
// happy-path probe. Returns Ok if `pkg-config --libs <name>` succeeds and we
// emitted the link flags it reported.
mod pkg_config {
    use std::process::Command;

    pub fn probe_library(name: &str) -> Result<(), ()> {
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
