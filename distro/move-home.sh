#!/bin/sh
# Turn the old distro disk (distro-esp, distro-root, distro-home) into one big distro-home
# partition, keeping what's in the home.
#
#   sudo distro/move-home.sh /var/tmp/distro-home.tar
#
# Runs once, on the Arch host (in the distro, /home is always mounted), after install.sh
# has put the system into a slot: the old install on that disk is erased. The steps:
# copy the home filesystem into the tar file named (owners, permissions, ACLs, extended
# attributes and sparse files kept), compare the copy with the home, partition the disk
# as one distro-home partition (ext4, label home), unpack the copy into it and compare
# again. The tar file is kept; delete it once the distro has booted with its home.
# Shows the disk and goes ahead only when its name is typed.
set -eu

. "$(dirname "$0")/disk.sh"

tarball=${1:-}
[ -n "$tarball" ] || die "where should the copy go? e.g. sudo $0 /var/tmp/distro-home.tar"
need_root "$tarball"
[ ! -e "$tarball" ] || die "$tarball exists already; pick another name or delete it"
tar --version 2>/dev/null | grep -q 'GNU tar' || die "needs GNU tar (run it on the Arch host)"

home=$(one_part distro-home)
[ -n "$home" ] || die "no distro-home partition"
disk=$(disk_of "$home")
[ "$(dev_number "$home")" != "$(root_dev_number)" ] || die "$home is this system's /"
for p in $(parts_of "$disk"); do
    ! in_use "$p" || die "$p is mounted or in use; unmount it first"
    case $(sed -n 's/^PARTNAME=//p' "/sys/class/block/${p#/dev/}/uevent") in
        distro-esp | distro-root | distro-home) ;;
        *) die "$disk has a partition that isn't the old distro's: $p" ;;
    esac
done
set -- $(parts_of "$disk")
[ $# -gt 1 ] || die "$disk holds only distro-home already"

src=$(mktemp -d)
# Whatever happens, don't leave a filesystem mounted on the temporary folder.
trap 'umount "$src" 2>/dev/null; rmdir "$src" 2>/dev/null' EXIT
mount -o ro "$home" "$src"
used=$(df -P "$src" | awk 'NR == 2 { print $3 }')
free=$(df -P "$(dirname "$tarball")" | awk 'NR == 2 { print $4 }')
[ $((used + used / 10)) -le "$free" ] ||
    die "not enough room for the copy in $(dirname "$tarball"): needs about $((used / 1048576 + 1)) GiB"

show_disk "$disk"
echo "The home ($home, $((used / 1048576)) GiB used) is copied to $tarball, then $disk"
echo "becomes one distro-home partition and the copy is unpacked into it."
confirm "Everything else on $disk (the old distro install) will be erased." "$disk"

TAR_OPTS="--numeric-owner --xattrs --xattrs-include=* --acls --sparse"
echo "== copying the home into $tarball"
# shellcheck disable=SC2086
tar -C "$src" $TAR_OPTS -cf "$tarball" .
echo "== comparing the copy with the home"
# shellcheck disable=SC2086
tar -C "$src" $TAR_OPTS -df "$tarball" || die "the copy differs from the home; nothing erased"
umount "$src"

echo "== partitioning $disk"
wipefs -q -a "$disk"
# The new partition starts where the old ESP did, so its FAT boot sector would still be
# there: wipe the old signatures from the new partition, or mount sees vfat and ext4.
sfdisk -q --wipe always --wipe-partitions always "$disk" <<PARTITIONS
label: gpt
type=$TYPE_HOME, name="distro-home"
PARTITIONS
udevadm settle
home=$(one_part distro-home)
[ -n "$home" ] || die "the new partition didn't show up; the copy is in $tarball"
wipefs -q -a "$home"
mkfs.ext4 -q -F -L home "$home"

echo "== unpacking the copy"
mount -t ext4 "$home" "$src"
# shellcheck disable=SC2086
tar -C "$src" $TAR_OPTS -xpf "$tarball"
# shellcheck disable=SC2086
tar -C "$src" $TAR_OPTS -df "$tarball" || die "the unpacked home differs from the copy (kept in $tarball)"
umount "$src"
e2fsck -fn "$home" >/dev/null || die "e2fsck found problems on $home (the copy is in $tarball)"

echo "== done"
show_disk "$disk"
echo "Once the distro has booted with its home, delete $tarball."
