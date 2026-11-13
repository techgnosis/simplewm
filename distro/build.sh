#!/bin/sh
# Build the distro from Alpine packages:
#
#   distro/root/                  the root filesystem (apk installs into it)
#   distro/out/vmlinuz            the kernel
#   distro/out/initramfs.cpio.gz  loads the disk and filesystem drivers, mounts the root
#   distro/out/root.img           distro/root/ as an ext4 disk image
#
# Run it on the host; it re-runs itself inside the Alpine container (container.sh).
# Each build starts from an empty root. Downloaded packages are kept in distro/cache/.
set -eu

if [ ! -e /etc/alpine-release ]; then
    exec "$(dirname "$0")/container.sh" distro/build.sh "$@"
fi

cd /work/distro

# The kernel flavor: "virt" is Alpine's smaller kernel for virtual machines.
KERNEL=virt

# What goes into the root filesystem.
PACKAGES="
    alpine-baselayout
    busybox
    musl-utils
"

# Drivers the initramfs loads to reach the root filesystem (plus what they need).
INITRAMFS_MODULES="virtio_pci virtio_blk ext4"

ROOT=/work/distro/root
OUT=/work/distro/out
CACHE=/work/distro/cache

echo "== tools"
apk add -q busybox-static e2fsprogs kmod

echo "== root filesystem"
rm -rf "$ROOT" "$OUT"
mkdir -p "$ROOT" "$OUT" "$CACHE"
# shellcheck disable=SC2086
apk --root "$ROOT" --initdb --quiet \
    --keys-dir /etc/apk/keys --repositories-file /etc/apk/repositories \
    --cache-dir "$CACHE" \
    add $PACKAGES
cp /etc/apk/repositories "$ROOT/etc/apk/repositories"

# The kernel is unpacked rather than installed: Alpine's kernel packages insist on an
# initramfs generator (mkinitfs), and we make our own initramfs below.
apk fetch --quiet --output "$CACHE" "linux-$KERNEL"
tar -xzf "$(ls -t "$CACHE"/linux-"$KERNEL"-*.apk | head -n 1)" -C "$ROOT" \
    --exclude '.*' boot lib 2>/dev/null

# Our pieces.
rm -f "$ROOT/sbin/init"
install -m 755 init "$ROOT/sbin/init"
echo simplewm > "$ROOT/etc/hostname"

echo "== kernel"
kver=$(ls "$ROOT/lib/modules")
cp "$ROOT/boot/vmlinuz-$KERNEL" "$OUT/vmlinuz"

echo "== initramfs"
ird=$(mktemp -d)
mkdir -p "$ird/bin" "$ird/dev" "$ird/proc" "$ird/sys" "$ird/newroot" "$ird/modules"
cp /bin/busybox.static "$ird/bin/busybox"
install -m 755 initramfs-init "$ird/init"
# The modules in load order, uncompressed so busybox's insmod can load them.
# shellcheck disable=SC2086
modprobe --dirname "$ROOT" --set-version "$kver" --show-depends --all $INITRAMFS_MODULES |
    awk '$1 == "insmod" { print $2 }' | awk '!seen[$0]++' |
    while read -r ko; do
        name=$(basename "$ko" .gz)
        gunzip -c "$ko" > "$ird/modules/$name"
        echo "$name" >> "$ird/modules/order"
    done
(cd "$ird" && find . | cpio -o -H newc --quiet | gzip -9) > "$OUT/initramfs.cpio.gz"
rm -rf "$ird"

echo "== disk image"
used=$(du -sm "$ROOT" | cut -f1)
size=$((used + used / 2 + 256))
mkfs.ext4 -q -L simplewm -d "$ROOT" "$OUT/root.img" "${size}M"

echo "== done: kernel $kver, root ${used} MB, image ${size} MB"
