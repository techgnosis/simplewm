#!/bin/sh
# Make room for the distro's two slots on the Arch disk: shrink Arch's partition and add
# distro-a and distro-b after it, SLOT_SIZE each, at the end of the disk.
#
#   sudo distro/make-slots.sh /dev/sda
#
# Runs once, from the distro (an ext4 filesystem can only shrink while it isn't mounted,
# so not from Arch itself). The disk must hold exactly the EFI system partition (1) and
# Arch's ext4 root (2), and nothing on it may be mounted. Partition 2 keeps its start and
# its partition ID, and the filesystem its UUID, so Arch boots as before. The slots are
# left empty: install.sh writes the system onto them.
#
# The steps: check the filesystem, shrink it below the new size, shrink the partition,
# add the slots, grow the filesystem to fill the partition again, check it again. It
# refuses unless the filesystem's minimum size (resize2fs -P) plus room to spare fits.
# Shows the plan first and goes ahead only when the disk's name is typed.
set -eu

. "$(dirname "$0")/disk.sh"

SLOT_SIZE_GIB=150
# What Arch must keep free after the shrink, on top of resize2fs's minimum.
SPARE_GIB=50

disk=${1:-}
[ -n "$disk" ] || die "which disk? e.g. sudo $0 /dev/sda"
need_root "$disk"
[ -b "$disk" ] && [ -e "/sys/block/${disk#/dev/}" ] || die "$disk isn't a whole disk"

if [ -n "$(part_by_name distro-a)$(part_by_name distro-b)" ]; then
    die "there are distro-a/distro-b partitions already: $(echo $(part_by_name distro-a) $(part_by_name distro-b))"
fi

set -- $(parts_of "$disk")
[ $# = 2 ] || die "$disk should have two partitions (EFI, Arch), has $#: $*"
esp=$1
arch=$2
[ "$(part_number "$esp")" = 1 ] && [ "$(part_number "$arch")" = 2 ] ||
    die "expected partitions 1 and 2 on $disk"
[ "$(sfdisk --part-type "$disk" 1 | tr a-f A-F)" = "$TYPE_ESP" ] ||
    die "$esp isn't an EFI system partition"
for p in "$esp" "$arch"; do
    ! in_use "$p" || die "$p is mounted or in use"
done
[ "$(dev_number "$arch")" != "$(root_dev_number)" ] || die "$arch is this system's /"
dumpe2fs -h "$arch" >/dev/null 2>&1 || die "$arch isn't an ext4 filesystem"

# Sizes in 512-byte sectors. The slots end at the last 1 MiB boundary before the backup
# GPT (the last 33 sectors), each SLOT_SIZE long; Arch's partition ends where they start.
slot=$((SLOT_SIZE_GIB * 2097152))
disk_size=$(cat "/sys/block/${disk#/dev/}/size")
end=$(((disk_size - 34) / 2048 * 2048))
b_start=$((end - slot))
a_start=$((b_start - slot))
arch_start=$(cat "/sys/class/block/${arch#/dev/}/start")
arch_size=$(cat "/sys/class/block/${arch#/dev/}/size")
new_arch=$((a_start - arch_start))
[ "$new_arch" -lt "$arch_size" ] || die "$arch already ends before where the slots would start"

block=$(dumpe2fs -h "$arch" 2>/dev/null | awk -F: '/^Block size/ { print $2 + 0 }')
min_blocks=$(resize2fs -P "$arch" 2>/dev/null | awk -F: '/minimum size/ { print $2 + 0 }')
[ -n "$block" ] && [ -n "$min_blocks" ] || die "can't read $arch's sizes"
min=$((min_blocks * (block / 512)))
gib() { echo "$(($1 / 2097152)) GiB"; }

echo "Now:"
show_disk "$disk"
echo
echo "The plan:"
echo "  $arch (Arch): $(gib "$arch_size") -> $(gib "$new_arch") (it needs at least $(gib "$min"))"
echo "  new distro-a: $SLOT_SIZE_GIB GiB, new distro-b: $SLOT_SIZE_GIB GiB"
echo
[ $((min + SPARE_GIB * 2097152)) -le "$new_arch" ] ||
    die "Arch needs $(gib "$min") and $SPARE_GIB GiB to spare; it wouldn't fit in $(gib "$new_arch")"

confirm "Arch's filesystem will be shrunk (back it up first: a power cut midway can wreck it)." "$disk"

fsck_ok() {
    e2fsck -f -p "$arch" || [ $? -le 1 ] || die "e2fsck found problems on $arch it can't fix by itself; run e2fsck -f $arch"
}

echo "== checking $arch"
fsck_ok
echo "== shrinking the filesystem"
# 1 GiB under the new partition size; it's grown to fit exactly afterwards.
resize2fs "$arch" "$((new_arch - 2097152))s"
echo "== shrinking partition 2, adding distro-a and distro-b"
# --wipe-partitions never: leave every existing signature alone (Arch's above all).
echo ",$new_arch" | sfdisk -q --wipe-partitions never -N 2 "$disk"
sfdisk -q --wipe-partitions never --append "$disk" <<PARTITIONS
start=$a_start, size=$slot, type=$TYPE_ROOT, name="distro-a"
start=$b_start, size=$slot, type=$TYPE_ROOT, name="distro-b"
PARTITIONS
udevadm settle
[ "$(cat "/sys/class/block/${arch#/dev/}/size")" = "$new_arch" ] ||
    die "the kernel doesn't see $arch's new size; reboot into the distro and run: resize2fs $arch && e2fsck -f $arch"
echo "== growing the filesystem to fill the partition"
resize2fs "$arch"
fsck_ok

echo "== done"
show_disk "$disk"
echo "Next: sudo distro/install.sh a (and b)."
