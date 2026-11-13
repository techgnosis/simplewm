#!/bin/sh
# Build the distro's kernel from kernel.org's source and distro/kernel.config, into
# distro/out/vmlinuz.
#
#   distro/kernel.sh              build it (build.sh does this too)
#   distro/kernel.sh menuconfig   change the config in the kernel's own menus; kernel.config
#                                 is rewritten when you exit them
#   distro/kernel.sh refresh      rewrite kernel.config in its short form (after a new
#                                 VERSION, or to tidy hand edits) and show what changed
#
# Everything the system needs is built in: no modules and no initramfs. The kernel finds
# the disk, mounts the ext4 filesystem named by root= and runs /sbin/init itself.
#
# kernel.config is what `make savedefconfig` writes: only what differs from the kernel's
# defaults, so each line is a choice. A build stops if the file doesn't come back the same
# from the kernel's config tools, which catches options that don't exist or whose
# dependencies are off (the kernel would quietly drop them).
#
# Run it on the host; it re-runs itself in the Alpine container. The source and build
# tree are kept in distro/cache/kernel/, so later builds only redo what changed.
set -eu

if [ ! -e /etc/alpine-release ]; then
    exec "$(dirname "$0")/container.sh" distro/kernel.sh "$@"
fi

# The newest long-term kernel when it was picked, and its checksum from kernel.org's
# sha256sums.asc.
VERSION=6.18.63
SHA256=81764a545072d160748beff4d9de2e7d95aa9f16e8261110288f067a2cc61d71

CONFIG=/work/distro/kernel.config
CACHE=/work/distro/cache/kernel
OUT=/work/distro/out
SRC=$CACHE/linux-$VERSION
BUILD=$CACHE/build-$VERSION
TARBALL=$CACHE/linux-$VERSION.tar.xz

echo "== kernel: build tools"
apk add -q build-base bc bison flex perl elfutils-dev openssl-dev linux-headers \
    xz ncurses-dev diffutils findutils gawk curl

mkdir -p "$CACHE"
if [ ! -d "$SRC" ]; then
    if [ ! -e "$TARBALL" ]; then
        echo "== kernel: downloading $VERSION"
        major=${VERSION%%.*}
        curl -sSf -o "$TARBALL.part" \
            "https://cdn.kernel.org/pub/linux/kernel/v$major.x/linux-$VERSION.tar.xz"
        mv "$TARBALL.part" "$TARBALL"
    fi
    echo "$SHA256  $TARBALL" | sha256sum -c -s ||
        { echo "linux-$VERSION.tar.xz doesn't match its checksum" >&2; exit 1; }
    echo "== kernel: unpacking"
    tar -xf "$TARBALL" -C "$CACHE"
fi

export KBUILD_BUILD_USER=simplewm KBUILD_BUILD_HOST=distro
kmake() {
    make -s -C "$SRC" O="$BUILD" "$@"
}

# The full config: kernel.config with every other option at its default.
mkdir -p "$BUILD"
cp "$CONFIG" "$BUILD/.config"
kmake olddefconfig

case ${1:-build} in
    menuconfig)
        kmake menuconfig
        kmake savedefconfig
        cp "$BUILD/defconfig" "$CONFIG"
        echo "== kernel: kernel.config saved; distro/kernel.sh builds it"
        ;;
    refresh)
        kmake savedefconfig
        diff -u "$CONFIG" "$BUILD/defconfig" || true
        cp "$BUILD/defconfig" "$CONFIG"
        ;;
    build)
        kmake savedefconfig
        if ! diff -u "$CONFIG" "$BUILD/defconfig" > "$BUILD/config.diff"; then
            cat "$BUILD/config.diff" >&2
            echo "kernel.config doesn't come back the same from the kernel's config tools" \
                "(- is kernel.config, + what the kernel makes of it). Fix the lines, or run" \
                "distro/kernel.sh refresh to accept what the kernel makes of it." >&2
            exit 1
        fi
        echo "== kernel: building $VERSION"
        kmake -j"$(nproc)" bzImage
        mkdir -p "$OUT"
        cp "$BUILD/arch/x86/boot/bzImage" "$OUT/vmlinuz"
        echo "== kernel: $(kmake kernelrelease) in distro/out/vmlinuz"
        ;;
    *)
        echo "usage: distro/kernel.sh [menuconfig | refresh]" >&2
        exit 2
        ;;
esac
