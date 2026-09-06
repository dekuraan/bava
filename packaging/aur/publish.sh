#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Push one of the AUR packages in this directory to aur.archlinux.org.
#
#   packaging/aur/publish.sh bava           # stable, from the release tarball
#   packaging/aur/publish.sh bava-git       # VCS package
#   packaging/aur/publish.sh bava 0.4.0     # bump pkgver + checksum first
#
# Needs an AUR account with your SSH key registered:
# https://aur.archlinux.org/account -- and `git config --global user.email`
# matching that account's email.
set -euo pipefail

pkg=${1:?usage: publish.sh <bava|bava-git> [new-pkgver]}
newver=${2:-}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
src="$here/$pkg"

[[ -f "$src/PKGBUILD" ]] || { echo "no such package: $pkg" >&2; exit 1; }

if [[ -n $newver ]]; then
    [[ $pkg == bava-git ]] && { echo "bava-git's pkgver() is computed; don't pass one" >&2; exit 1; }
    echo "==> bumping to $newver"
    sed -i "s/^pkgver=.*/pkgver=$newver/;s/^pkgrel=.*/pkgrel=1/" "$src/PKGBUILD"
    # updpkgsums downloads the new tarball and rewrites sha256sums in place.
    (cd "$src" && updpkgsums)
fi

# .SRCINFO is what the AUR actually indexes, and a stale one is the single most
# common way a package goes out of sync with its PKGBUILD.
(cd "$src" && makepkg --printsrcinfo > .SRCINFO)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone "ssh://aur@aur.archlinux.org/$pkg.git" "$work/$pkg"
cp "$src/PKGBUILD" "$src/.SRCINFO" "$work/$pkg/"

cd "$work/$pkg"
git add PKGBUILD .SRCINFO
if git diff --cached --quiet; then
    echo "==> nothing changed on the AUR side"
    exit 0
fi
git commit -m "Update $pkg to $(sed -n 's/^\tpkgver = //p' .SRCINFO)"
git --no-pager show --stat
read -rp "==> push to the AUR? [y/N] " reply
[[ $reply == [yY] ]] && git push origin master
