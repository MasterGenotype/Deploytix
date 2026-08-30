# Maintainer: superphenotype
#
# Embedded copy of vendor/tkg-gui/pkg/PKGBUILD, used by the installer to build
# tkg-gui-git on demand when no pre-built package and no local clone of the
# deploytix repo are available.  It is self-contained — `source` is the
# upstream git URL, so makepkg fetches everything it needs — which is what
# lets the installer build it from the PKGBUILD file alone.
#
# MUST stay in sync with vendor/tkg-gui/pkg/PKGBUILD.
pkgname=tkg-gui-git
pkgver=r39.ga8cae66
pkgrel=1
pkgdesc="Graphical interface for building custom Linux kernels using the linux-tkg build system"
arch=('x86_64')
url="https://github.com/MasterGenotype/tkg-gui"
license=('unknown')
depends=('gcc-libs' 'glibc' 'libxkbcommon' 'libxcb' 'wayland' 'mesa' 'openssl')
makedepends=('cargo' 'git')
optdepends=(
    'base-devel: required by makepkg for kernel builds'
)
provides=('tkg-gui')
conflicts=('tkg-gui')
source=("tkg-gui::git+https://github.com/MasterGenotype/tkg-gui.git")
sha256sums=('SKIP')

pkgver() {
    cd tkg-gui
    ( set -o pipefail
      git describe --long --tags --abbrev=7 2>/dev/null \
          | sed 's/^v//;s/\([^-]*-g\)/r\1/;s/-/./g'
    ) || printf "r%s.g%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short=7 HEAD)"
}

prepare() {
    cd tkg-gui
    git submodule update --init --recursive
    export RUSTUP_TOOLCHAIN=stable
    cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
    cd tkg-gui
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR=target
    cargo build --frozen --release
}

package() {
    cd tkg-gui
    install -Dm755 "target/release/tkg-gui" "$pkgdir/usr/bin/tkg-gui"

    # Install submodules to /usr/share/tkg-gui so the binary can locate them
    install -dm755 "$pkgdir/usr/share/tkg-gui"
    cp -a submodules/linux-tkg "$pkgdir/usr/share/tkg-gui/linux-tkg"

    install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"

    # Install desktop entry
    sed 's|%BINDIR%|/usr/bin|g' tkg-gui.desktop > tkg-gui.desktop.out
    install -Dm644 tkg-gui.desktop.out "$pkgdir/usr/share/applications/tkg-gui.desktop"

    # Install polkit policy
    install -Dm644 com.tkg-gui.policy "$pkgdir/usr/share/polkit-1/actions/com.tkg-gui.policy"
    sed -i 's|%BINDIR%|/usr/bin|g' "$pkgdir/usr/share/polkit-1/actions/com.tkg-gui.policy"
}
