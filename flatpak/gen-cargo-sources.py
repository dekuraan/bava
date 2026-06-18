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
    for pkg in lock.get("package", []):
        # Only crates.io packages have a registry source + checksum. Local
        # path crates (cavacore-sys/-rs, bava) have neither and are built
        # straight from the repo checkout, so skip them here.
        if pkg.get("source") != CRATES_IO:
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

    OUT.write_text(json.dumps(sources, indent=4) + "\n")
    crates = sum(1 for s in sources if s["type"] == "archive")
    print(f"wrote {OUT.relative_to(ROOT)} ({crates} crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
