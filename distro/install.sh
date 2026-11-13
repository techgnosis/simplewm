#!/bin/sh
# Install the distro (what build.sh made in distro/out/) on a whole disk of this machine.
#
#   sudo distro/install.sh /dev/sdb
#
# The disk gets two GPT partitions, found again later by their names:
#   distro-esp   512 MiB EFI system partition (FAT32); the kernel is \EFI\distro\vmlinuz.efi,
#                which the firmware starts directly (the kernel is an EFI program)
#   distro-root  the rest: out/root.img written over it, then grown to fill it
#
# The first time (when the disk doesn't have those partitions) it shows what's on the
# disk and erases all of it once you type its name. Later runs write a new root
# filesystem and kernel over the old ones without asking: everything on distro-root is
# replaced, the user's home too, like every build. The partitions themselves (and so
# the boot entry from efibootmgr.sh, which names them by their IDs) stay.
#
# Runs on the host, not in the container: it needs root (sudo), sfdisk, wipefs,
# mkfs.vfat, e2fsck and resize2fs.
set -eu

out=$(cd "$(dirname "$0")" && pwd)/out
disk=${1:-}

die() {
    echo "install.sh: $*" >&2
    exit 1
}

[ -n "$disk" ] || die "which disk? e.g. sudo distro/install.sh /dev/sdb"
[ "$(id -u)" = 0 ] || die "needs root: sudo distro/install.sh $disk"
for f in vmlinuz root.img; do
    [ -e "$out/$f" ] || die "no $out/$f; run distro/build.sh first"
done
[ -b "$disk" ] || die "$disk isn't a block device"
[ "$(lsblk -dno TYPE "$disk")" = disk ] || die "$disk isn't a whole disk"

# Never a disk that's in use: anything on it mounted, used as swap, or holding this
# system's root filesystem.
if lsblk -lno MOUNTPOINTS "$disk" | grep -q .; then
    lsblk -o NAME,SIZE,FSTYPE,MOUNTPOINTS "$disk" >&2
    die "something on $disk is mounted; unmount it first"
fi
rootdisk=$(lsblk -no PKNAME "$(findmnt -no SOURCE /)" 2>/dev/null || true)
[ "/dev/$rootdisk" != "$disk" ] || die "$disk holds this system's root filesystem"

# The disk's partition called $1, if it has one.
part() {
    lsblk -lnpo NAME,PARTLABEL "$disk" | awk -v want="$1" '$2 == want { print $1 }'
}

esp=$(part distro-esp)
root=$(part distro-root)
if [ -z "$esp" ] || [ -z "$root" ]; then
    echo "$disk doesn't have the distro's partitions yet. It holds:"
    lsblk -o NAME,SIZE,TYPE,MODEL,FSTYPE,LABEL,PARTLABEL "$disk"
    echo
    name=$(basename "$disk")
    printf 'Everything on %s will be erased. Type %s to go ahead: ' "$disk" "$name"
    read -r answer
    [ "$answer" = "$name" ] || die "not erased"

    echo "== partitioning $disk"
    wipefs -q -a "$disk"
    sfdisk -q "$disk" <<EOF
label: gpt
size=512MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="distro-esp"
type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, name="distro-root"
EOF
    udevadm settle
    esp=$(part distro-esp)
    root=$(part distro-root)
    [ -n "$esp" ] && [ -n "$root" ] || die "the new partitions didn't show up"
    mkfs.vfat -F 32 -n DISTRO "$esp" >/dev/null
fi

image=$(stat -c %s "$out/root.img")
room=$(blockdev --getsize64 "$root")
[ "$image" -le "$room" ] || die "root.img ($image bytes) doesn't fit in $root ($room bytes)"

echo "== root filesystem: out/root.img onto $root"
dd if="$out/root.img" of="$root" bs=4M conv=fsync status=progress
# A clean check first: resize2fs insists on one.
e2fsck -fy "$root" >/dev/null || [ $? -le 1 ] || die "e2fsck found problems on $root"
resize2fs "$root" 2>&1 | tail -n 1

echo "== kernel: out/vmlinuz onto $esp"
mnt=$(mktemp -d)
mount "$esp" "$mnt"
mkdir -p "$mnt/EFI/distro"
cp "$out/vmlinuz" "$mnt/EFI/distro/vmlinuz.efi"
umount "$mnt"
rmdir "$mnt"
sync

echo "== done: $disk"
if ! efibootmgr 2>/dev/null | grep -q '^Boot[0-9A-F]\{4\}\*\? Distro\b'; then
    echo "No Distro boot entry yet: sudo distro/efibootmgr.sh"
fi
