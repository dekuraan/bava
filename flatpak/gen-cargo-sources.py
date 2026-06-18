#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate flatpak-builder cargo sources from Cargo.lock, offline.

This is a self-contained stand-in for flatpak-builder-tools'
`flatpak-cargo-generator.py`. It works without network access and without
the `aiohttp`/`toml` third-party deps (stdlib `tomllib` only), because this
workspace pulls *only* crates.io registry crates plus local path crates —
there are no git dependencies to resolve.

It emits a flatpak-builder sources array (one entry per crates.io crate as a
verified `.crate` archive, plus the `.cargo-checksum.json` shims and a cargo
config that redirects crates-io to the vendored copies). Re-run whenever
Cargo.lock changes:

    python3 flatpak/gen-cargo-sources.py

Output: flatpak/cargo-sources.json
"""
from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "Cargo.lock"
OUT = ROOT / "flatpak" / "cargo-sources.json"

VENDOR = "cargo/vendor"


def main() -> int:
    with LOCK.open("rb") as f:
        lock = tomllib.load(f)

    sources: list[dict] = []
    skipped: list[str] = []
    for pkg in lock.get("package", []):
        source = pkg.get("source")
        # Local path crates (cavacore-sys/-rs, bava) have no source and are
        # built straight from the repo checkout — skip them silently.
        if source is None:
            continue
        # Only crates.io packages can be vendored by this offline generator.
        # A git or alternate-registry dependency would be silently dropped,
        # producing an incomplete cargo-sources.json and an opaque offline
        # build failure deep in the sandbox. Collect and report these loudly.
        if source != CRATES_IO:
            skipped.append(f"{pkg['name']} {pkg['version']} ({source})")
            continue
        name = pkg["name"]
        version = pkg["version"]
        checksum = pkg["checksum"]
        dest = f"{VENDOR}/{name}-{version}"

        # The crate tarball, integrity-pinned by the lockfile checksum.
        sources.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        # cargo refuses a vendored crate without this side-file; an empty
        # `files` map disables per-file hashing (the archive sha256 already
        # guarantees integrity).
        sources.append(
            {
                "type": "inline",
                "contents": json.dumps({"files": {}, "package": checksum}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )

    # Redirect every crates-io fetch to the vendored directory so the build
    # runs fully offline.
    config = (
        "[source.crates-io]\n"
        'replace-with = "vendored-sources"\n\n'
        "[source.vendored-sources]\n"
        f'directory = "{VENDOR}"\n'
    )
    sources.append(
        {
            "type": "inline",
            "contents": config,
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )

    if skipped:
        print(
            "ERROR: this generator only vendors crates.io dependencies, but "
            f"{len(skipped)} package(s) use another source:",
            file=sys.stderr,
        )
        for entry in skipped:
            print(f"  - {entry}", file=sys.stderr)
        print(
            "The offline flatpak build would fail to find these. Use the "
            "upstream flatpak-cargo-generator.py (it resolves git sources) "
            "instead of this stand-in.",
            file=sys.stderr,
        )
        return 1

    OUT.write_text(json.dumps(sources, indent=4) + "\n")
    crates = sum(1 for s in sources if s["type"] == "archive")
    print(f"wrote {OUT.relative_to(ROOT)} ({crates} crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
