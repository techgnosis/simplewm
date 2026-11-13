#!/bin/sh
# Build the distro from Alpine packages:
#
#   target/alpine/release/          simplewm and simplewm-init for musl (build-simplewm.sh)
#   distro/root/                  the root filesystem (apk installs into it)
#   distro/out/vmlinuz            the kernel
#   distro/out/initramfs.cpio.gz  loads the disk and filesystem drivers, mounts the root
#   distro/out/root.img           distro/root/ as an ext4 disk image
#
# Run it on the host; it re-runs itself inside the Alpine container (container.sh).
# Each build starts from an empty root, so the user's home (simplewm's settings) starts
# empty too. Downloaded packages are kept in distro/cache/.
set -eu

if [ ! -e /etc/alpine-release ]; then
    exec "$(dirname "$0")/container.sh" distro/build.sh "$@"
fi

cd /work/distro

# The kernel flavor: "virt" is Alpine's smaller kernel for virtual machines.
KERNEL=virt

# What goes into the root filesystem. wayland-utils (wayland-info) is for checking what
# simplewm offers apps.
PACKAGES="
    alpine-baselayout
    busybox
    bash
    musl-utils
    procps-ng
    kbd
    font-terminus
    kmod

    eudev
    eudev-hwids
    seatd

    libgcc
    libseat
    libinput-libs
    libxkbcommon
    xkeyboard-config
    mesa-gbm
    mesa-egl
    mesa-gles
    mesa-dri-gallium

    dbus
    pipewire
    pipewire-pulse
    pipewire-tools
    wireplumber

    iproute2
    sudo
    shared-mime-info

    alacritty
    firefox
    font-dejavu
    wayland-utils
"

# The user simplewm runs as (UID 1000, which is how simplewm-init finds them), and their
# password (for sudo, which the Network applet uses). Development values; the password
# is the same in every build.
USER_NAME=user
USER_PASSWORD=simplewm
USER_GROUPS="seat audio video input wheel"

# Folders the system doesn't need, removed from the root filesystem (Alpine's base layout
# makes them). Only empty ones (or holding only empty folders) are removed; if a package
# ever puts files in one, the build stops. Nothing can make them again: there's no package
# manager in the system.
REMOVE_DIRS="
    /media
    /mnt
    /opt
    /srv
"

# Drivers the initramfs loads to reach the root filesystem (plus what they need).
INITRAMFS_MODULES="virtio_pci virtio_blk ext4"

ROOT=/work/distro/root
OUT=/work/distro/out
CACHE=/work/distro/cache

sh build-simplewm.sh

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

# The kernel is unpacked rather than installed: Alpine's kernel packages insist on an
# initramfs generator (mkinitfs), and we make our own initramfs below.
apk fetch --quiet --output "$CACHE" "linux-$KERNEL"
tar -xzf "$(ls -t "$CACHE"/linux-"$KERNEL"-*.apk | head -n 1)" -C "$ROOT" \
    --exclude '.*' boot lib 2>/dev/null

# No package manager in the system: it's changed only by rebuilding it. apk itself was
# never installed; remove its records (database and settings) too.
rm -rf "$ROOT/etc/apk" "$ROOT/lib/apk" "$ROOT/var/cache/apk"

echo "== our pieces"
install -m 755 /work/target/alpine/release/simplewm "$ROOT/usr/bin/simplewm"
# simplewm-init is PID 1; it runs simplewm as the account with UID 1000 (made below).
install -m 755 /work/target/alpine/release/simplewm-init "$ROOT/sbin/simplewm-init"
ln -sf simplewm-init "$ROOT/sbin/init"
echo simplewm > "$ROOT/etc/hostname"
install -m 644 firefox.js "$ROOT/usr/lib/firefox/defaults/pref/simplewm.js"
# udevd reads the hardware database from this compiled file.
chroot "$ROOT" udevadm hwdb --update
# dbus-daemon wants a machine ID.
chroot "$ROOT" dbus-uuidgen --ensure=/etc/machine-id

# bash is the login shell (root's too); /bin/sh stays busybox for scripts.
sed -i 's|^root:\(.*\):/bin/sh$|root:\1:/bin/bash|' "$ROOT/etc/passwd"
chroot "$ROOT" adduser -D -u 1000 -s /bin/bash -g "$USER_NAME" "$USER_NAME"
for g in $USER_GROUPS; do
    chroot "$ROOT" addgroup "$USER_NAME" "$g"
done
echo "$USER_NAME:$USER_PASSWORD" | chroot "$ROOT" chpasswd >/dev/null
echo "%wheel ALL=(ALL:ALL) ALL" > "$ROOT/etc/sudoers.d/wheel"
chmod 440 "$ROOT/etc/sudoers.d/wheel"

# Only the accounts the system uses: root and the user. Alpine's base layout adds ones for
# services that don't run here (mail, news, cron, sshd, …), and dbus adds messagebus for
# the system bus, which doesn't run either (simplewm runs the session bus). Its setuid
# helper for starting system-bus services goes too. The tools' backup copies (passwd-,
# shadow-, group-) go as well.
KEEP_ACCOUNTS="root $USER_NAME"
for file in passwd shadow; do
    awk -F: -v keep=" $KEEP_ACCOUNTS " 'index(keep, " " $1 " ")' \
        "$ROOT/etc/$file" > "$ROOT/etc/$file.new"
    cat "$ROOT/etc/$file.new" > "$ROOT/etc/$file"
    rm "$ROOT/etc/$file.new"
done
rm -f "$ROOT/etc/passwd-" "$ROOT/etc/shadow-" "$ROOT/etc/group-"
# Alpine ships root with an empty password: anything running as the user could `su` to
# root without one. Lock it; the user gets root through sudo, with their own password.
sed -i 's/^root:[^:]*:/root:*:/' "$ROOT/etc/shadow"
rm -f "$ROOT/usr/libexec/dbus-daemon-launch-helper"

echo "== trimming"
for dir in $REMOVE_DIRS; do
    [ -e "$ROOT$dir" ] || continue
    find "$ROOT$dir" -depth -type d -empty -delete
    if [ -e "$ROOT$dir" ]; then
        echo "$dir isn't empty; not removing it:" >&2
        find "$ROOT$dir" -not -type d >&2
        exit 1
    fi
done

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
