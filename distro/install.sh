#!/bin/sh
# Install the distro (what build.sh made in distro/out/) on a whole disk of this machine.
#
#   sudo distro/install.sh /dev/sdb
#
# The disk gets three GPT partitions, found again later by their names:
#   distro-esp   512 MiB EFI system partition (FAT32); the kernel is \EFI\distro\vmlinuz.efi,
#                which the firmware starts directly (the kernel is an EFI program)
#   distro-root  ROOT_SIZE: out/root.img written over it, then grown to fill it
#   distro-home  the rest (ext4): the home folders, which simplewm-init mounts on /home
#
# The first time (when the disk doesn't have those partitions) it shows what's on the
# disk and erases all of it once you type its name. Later runs write a new root
# filesystem and kernel over the old ones without asking. The home partition is only
# written when it's made (with the build's /home: the user's Rust and simplewm's
# default settings); after that every install leaves it alone. The partitions (and so
# the boot entry from efibootmgr.sh, which names them by their IDs) stay.
#
# A disk from before there was a home partition (distro-esp, and distro-root filling the
# rest) gets one the same way: distro-root shrinks to ROOT_SIZE, keeping its ID, and
# distro-home takes the rest, once you type the disk's name (what was on distro-root,
# its home too, is replaced).
#
# Runs on the host, not in the container: it needs root (sudo), sfdisk, wipefs,
# mkfs.vfat, mkfs.ext4, e2fsck and resize2fs.
set -eu

out=$(cd "$(dirname "$0")" && pwd)/out
disk=${1:-}

# The system's partition: room for what's installed (about 2 GB) with plenty to spare.
# The rest of the disk is the home partition.
ROOT_SIZE=32GiB
# GPT partition types: EFI system, Linux root (x86-64), Linux /home.
TYPE_ESP=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
TYPE_ROOT=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709
TYPE_HOME=933AC7E1-2EB4-4F13-B844-0E14E2AEF915

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

# Ask before erasing anything: show the disk and want its name typed.
confirm() {
    lsblk -o NAME,SIZE,TYPE,MODEL,FSTYPE,LABEL,PARTLABEL "$disk"
    echo
    name=$(basename "$disk")
    printf '%s Type %s to go ahead: ' "$1" "$name"
    read -r answer
    [ "$answer" = "$name" ] || die "nothing changed"
}

# An empty ext4 filesystem on the new home partition $1.
make_home() {
    wipefs -q -a "$1"
    mkfs.ext4 -q -F -L home "$1"
}

esp=$(part distro-esp)
root=$(part distro-root)
home=$(part distro-home)
new_home=
if [ -z "$esp" ] || [ -z "$root" ]; then
    echo "$disk doesn't have the distro's partitions yet. It holds:"
    confirm "Everything on $disk will be erased."

    echo "== partitioning $disk"
    wipefs -q -a "$disk"
    sfdisk -q "$disk" <<PARTITIONS
label: gpt
size=512MiB, type=$TYPE_ESP, name="distro-esp"
size=$ROOT_SIZE, type=$TYPE_ROOT, name="distro-root"
type=$TYPE_HOME, name="distro-home"
PARTITIONS
    udevadm settle
    esp=$(part distro-esp)
    root=$(part distro-root)
    home=$(part distro-home)
    [ -n "$esp" ] && [ -n "$root" ] && [ -n "$home" ] || die "the new partitions didn't show up"
    mkfs.vfat -F 32 -n DISTRO "$esp" >/dev/null
    make_home "$home"
    new_home=yes
elif [ -z "$home" ]; then
    echo "$disk has no home partition yet: distro-root will shrink to $ROOT_SIZE and the"
    echo "rest of the disk becomes distro-home, which later installs leave alone."
    confirm "Everything on distro-root ($root), its home folder too, will be replaced."

    echo "== making the home partition"
    rootnum=$(cat "/sys/class/block/$(basename "$root")/partition")
    wipefs -q -a "$root"
    echo "size=$ROOT_SIZE" | sfdisk -q -N "$rootnum" "$disk"
    echo "type=$TYPE_HOME, name=\"distro-home\"" | sfdisk -q --append "$disk"
    udevadm settle
    home=$(part distro-home)
    [ -n "$home" ] || die "the new home partition didn't show up"
    make_home "$home"
    new_home=yes
fi

image=$(stat -c %s "$out/root.img")
room=$(blockdev --getsize64 "$root")
[ "$image" -le "$room" ] || die "root.img ($image bytes) doesn't fit in $root ($room bytes)"

echo "== root filesystem: out/root.img onto $root"
dd if="$out/root.img" of="$root" bs=4M conv=fsync status=progress
# A clean check first: resize2fs insists on one.
e2fsck -fy "$root" >/dev/null || [ $? -le 1 ] || die "e2fsck found problems on $root"
resize2fs "$root" 2>&1 | tail -n 1

if [ -n "$new_home" ]; then
    echo "== home: the build's /home onto $home"
    from=$(mktemp -d)
    to=$(mktemp -d)
    mount -o ro "$root" "$from"
    mount "$home" "$to"
    cp -a "$from/home/." "$to/"
    umount "$to" "$from"
    rmdir "$to" "$from"
fi

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
