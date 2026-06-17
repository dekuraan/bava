# Maintainer: dekuraan <declan.li.carney@gmail.com>
pkgname=bava-git
pkgver=r31.g697d611
pkgrel=1
pkgdesc="Bevy music visualizer driven by cavacore, MPRIS, and PulseAudio"
arch=('x86_64')
url="https://github.com/dekuraan/bava"
license=('GPL-3.0-or-later')
depends=(
    'fftw'
    'libpulse'
    'dbus'
    'libxkbcommon'
    'wayland'
    'libx11'
    'libxcursor'
    'libxrandr'
    'libxi'
    'vulkan-icd-loader'
    'systemd-libs'
)
makedepends=(
    'rust'
    'cargo'
    'pkgconf'
    'gcc'
)
optdepends=(
    'pipewire-pulse: PipeWire as PulseAudio drop-in (replaces libpulse at runtime)'
    'spotifyd: Spotify MPRIS player for album art and now-playing info'
)
provides=('bava')
conflicts=('bava')
source=('bava::git+https://github.com/dekuraan/bava.git')
sha256sums=('SKIP')

pkgver() {
    cd "$srcdir/bava"
    printf "r%s.g%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
    cd "$srcdir/bava"
    cargo fetch --locked --target "$CARCH-unknown-linux-gnu"
}

build() {
    cd "$srcdir/bava"
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR=target
    cargo build --frozen --release --package bava
}

check() {
    cd "$srcdir/bava"
    cargo test --frozen --package cavacore-rs
}

package() {
    cd "$srcdir/bava"
    install -Dm755 "target/release/bava" "$pkgdir/usr/bin/bava"
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
