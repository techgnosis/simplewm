#!/bin/sh
# Install the distro (what build.sh made in distro/out/) into one of its two slots, the
# partitions distro-a and distro-b at the end of the first disk.
#
#   sudo distro/install.sh        into the slot that isn't running (from the distro)
#   sudo distro/install.sh a      into distro-a (from Arch, say which)
#
# out/root.img is written over the slot and grown to fill it, and gets its own filesystem
# UUID and label (simplewm-a, simplewm-b), so the two slots don't look like one disk.
# out/vmlinuz goes into the EFI system partition on the slot's disk as
# \EFI\simplewm\a.efi or b.efi, which the firmware's "Distro A" and "Distro B" entries
# start (efibootmgr.sh makes them once; they never change).
#
# Refuses the running slot and anything mounted. Doesn't ask: a slot holds nothing but
# the system (the home folders are on distro-home). Works from Arch and from the distro.
set -eu

. "$(dirname "$0")/disk.sh"

out="$DISTRO/out"
need_root "$@"
for f in vmlinuz root.img; do
    [ -e "$out/$f" ] || die "no $out/$f; run distro/build.sh first"
done

a=$(one_part distro-a)
b=$(one_part distro-b)
[ -n "$a" ] && [ -n "$b" ] || die "no distro-a and distro-b partitions on this machine"
running=
[ "$(dev_number "$a")" = "$(root_dev_number)" ] && running=a
[ "$(dev_number "$b")" = "$(root_dev_number)" ] && running=b

case ${1:-} in
    a | b) slot=$1 ;;
    "")
        case $running in
            a) slot=b ;;
            b) slot=a ;;
            *) die "not running from a slot; say which: sudo $0 a (or b)" ;;
        esac
        ;;
    *) die "usage: sudo $0 [a | b]" ;;
esac
[ "$slot" != "$running" ] || die "distro-$slot is the running system"
if [ "$slot" = a ]; then part=$a; else part=$b; fi
! in_use "$part" || die "$part (distro-$slot) is mounted"

disk=$(disk_of "$part")
esp=$(esp_of "$disk")

image=$(stat -c %s "$out/root.img")
room=$(($(cat "/sys/class/block/${part#/dev/}/size") * 512))
[ "$image" -le "$room" ] || die "root.img ($image bytes) doesn't fit in $part ($room bytes)"

echo "== distro-$slot ($part): out/root.img"
dd if="$out/root.img" of="$part" bs=4M conv=fsync 2>/dev/null
# A clean check first: resize2fs and tune2fs insist on one.
e2fsck -fy "$part" >/dev/null || [ $? -le 1 ] || die "e2fsck found problems on $part"
resize2fs "$part" 2>&1 | tail -n 1
tune2fs -U random -L "simplewm-$slot" "$part" >/dev/null

echo "== kernel: out/vmlinuz onto $esp as \\EFI\\simplewm\\$slot.efi"
mnt=$(mount_point "$esp")
mounted_here=
if [ -z "$mnt" ]; then
    mnt=$(mktemp -d)
    mount -t vfat "$esp" "$mnt"
    mounted_here=1
fi
mkdir -p "$mnt/EFI/simplewm"
cp "$out/vmlinuz" "$mnt/EFI/simplewm/$slot.efi.new"
mv "$mnt/EFI/simplewm/$slot.efi.new" "$mnt/EFI/simplewm/$slot.efi"
sync
if [ -n "$mounted_here" ]; then
    umount "$mnt"
    rmdir "$mnt"
fi

echo "== done: pick Distro $(echo "$slot" | tr ab AB) in the firmware's boot menu"
