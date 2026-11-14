#!/bin/sh
# Build the distro's kernel from kernel.org's source and distro/kernel.config, into
# distro/out/vmlinuz.
#
#   distro/build-kernel.sh              build it (build.sh does this too)
#   distro/build-kernel.sh menuconfig   change the config in the kernel's own menus; kernel.config
#                                 is rewritten when you exit them
#   distro/build-kernel.sh refresh      rewrite kernel.config in its short form (after a new
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
    exec "$(dirname "$0")/container.sh" distro/build-kernel.sh "$@"
fi

# The newest stable kernel when it was picked, and its checksum from kernel.org's
# sha256sums.asc.
VERSION=7.3.4
SHA256=b11b6df8295e8e991ff040b71aaa4994a8b55caa23b6b092907f195b5840fd1f

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

# Firmware built into the kernel (CONFIG_EXTRA_FIRMWARE): built-in drivers start before
# the disk is mounted, so they can't load firmware from it. Each file comes from Alpine's
# linux-firmware package named after its folder (amdgpu/… from linux-firmware-amdgpu),
# unpacked into the container's /lib/firmware, where the kernel's build looks
# (CONFIG_EXTRA_FIRMWARE_DIR's default). Alpine compresses firmware; the kernel wants it
# plain.
firmware=$(sed -n 's/^CONFIG_EXTRA_FIRMWARE="\(.*\)"$/\1/p' "$CONFIG")
if [ -n "$firmware" ]; then
    echo "== kernel: firmware"
    apk add -q zstd
    for pkg in $(for f in $firmware; do echo "linux-firmware-${f%%/*}"; done | sort -u); do
        apk fetch --quiet --output /work/distro/cache "$pkg"
        apkfile=$(ls -t /work/distro/cache/"$pkg"-[0-9]*.apk | head -n 1)
        for f in $firmware; do
            [ "linux-firmware-${f%%/*}" = "$pkg" ] || continue
            tar -xzf "$apkfile" -C / "lib/firmware/$f.zst" 2>/dev/null ||
                { echo "$pkg has no $f" >&2; exit 1; }
        done
    done
    # Some files are links to others; zstd won't read links named on its command line,
    # so each goes through stdin.
    for f in $firmware; do
        zstd -q -d < "/lib/firmware/$f.zst" > "/lib/firmware/$f" ||
            { echo "can't decompress $f (a link to a file not listed?)" >&2; exit 1; }
    done
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
        echo "== kernel: kernel.config saved; distro/build-kernel.sh builds it"
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
                "distro/build-kernel.sh refresh to accept what the kernel makes of it." >&2
            exit 1
        fi
        echo "== kernel: building $VERSION"
        kmake -j"$(nproc)" bzImage
        mkdir -p "$OUT"
        cp "$BUILD/arch/x86/boot/bzImage" "$OUT/vmlinuz"
        echo "== kernel: $(kmake kernelrelease) in distro/out/vmlinuz"
        ;;
    *)
        echo "usage: distro/build-kernel.sh [menuconfig | refresh]" >&2
        exit 2
        ;;
esac
