# Sourced by the disk scripts (make-slots.sh, install.sh, efibootmgr.sh, move-home.sh):
# finding partitions and what's mounted, the same way on the Arch host and in the distro.
#
# Partitions are found by their GPT names (PARTNAME in /sys/class/block/*/uevent), never
# by sdX: the disks' letters can swap between boots. Mounts are matched by device number
# (/proc/self/mountinfo), since the distro's / shows up as /dev/root.
#
# The distro's image may lack e2fsprogs, sfdisk or lsblk (older builds did); then they
# come from distro/cache/tools/, where build.sh's apk.static puts them (see "Disk tools"
# in CLAUDE.md).

DISTRO=$(cd "$(dirname "$0")" && pwd)

# GPT partition types: EFI system, Linux root (x86-64), Linux /home.
TYPE_ESP=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
TYPE_ROOT=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709
TYPE_HOME=933AC7E1-2EB4-4F13-B844-0E14E2AEF915

die() {
    echo "${0##*/}: $*" >&2
    exit 1
}

need_root() {
    [ "$(id -u)" = 0 ] || die "needs root: sudo $0 $*"
}

# Tools missing from the system come from cache/tools/ (with its libraries).
tools="$DISTRO/cache/tools"
for t in e2fsck resize2fs dumpe2fs tune2fs mkfs.ext4 sfdisk; do
    command -v "$t" >/dev/null 2>&1 && continue
    if [ -e "$tools/lib/apk/db/installed" ]; then
        PATH="$PATH:$tools/sbin:$tools/usr/sbin:$tools/bin:$tools/usr/bin"
        export LD_LIBRARY_PATH="$tools/usr/lib"
        break
    fi
    die "no $t; in the distro, run distro/build.sh once (as you) to put the disk tools in cache/tools/"
done

# The partitions called $1, one /dev path per line.
part_by_name() {
    for u in /sys/class/block/*/uevent; do
        grep -qx "PARTNAME=$1" "$u" || continue
        echo "/dev/$(sed -n 's/^DEVNAME=//p' "$u")"
    done
}

# Exactly one partition called $1 (or nothing, when there's none).
one_part() {
    found=$(part_by_name "$1")
    [ "$(echo "$found" | grep -c .)" -le 1 ] || die "more than one partition called $1: $(echo $found)"
    echo "$found"
}

# /dev/sdX for the partition /dev/sdXN, its number, and its maj:min.
disk_of() {
    echo "/dev/$(basename "$(dirname "$(readlink -f "/sys/class/block/${1#/dev/}")")")"
}
part_number() {
    cat "/sys/class/block/${1#/dev/}/partition"
}
dev_number() {
    cat "/sys/class/block/${1#/dev/}/dev"
}

# The partitions of the disk /dev/sdX.
parts_of() {
    for p in /sys/class/block/"${1#/dev/}"/"${1#/dev/}"*; do
        [ -e "$p/partition" ] && echo "/dev/${p##*/}"
    done
}

# Where the device $1 is mounted (first mount point), or nothing.
mount_point() {
    awk -v dev="$(dev_number "$1")" '$3 == dev { print $5; exit }' /proc/self/mountinfo
}

# Whether $1 (a partition) is mounted or used as swap.
in_use() {
    [ -n "$(mount_point "$1")" ] && return 0
    awk 'NR > 1 { print $1 }' /proc/swaps | grep -qx "$1"
}

# The maj:min of the running system's /.
root_dev_number() {
    awk '$5 == "/" { dev = $3 } END { print dev }' /proc/self/mountinfo
}

# The EFI system partition on the disk $1 (exactly one).
esp_of() {
    found=
    for p in $(parts_of "$1"); do
        [ "$(sfdisk --part-type "$1" "$(part_number "$p")" 2>/dev/null | tr a-f A-F)" = "$TYPE_ESP" ] &&
            found="$found $p"
    done
    set -- $found
    [ $# = 1 ] || die "expected one EFI system partition on the disk, found: ${found:-none}"
    echo "$1"
}

# Ask before changing a disk: show it and want its name typed.
confirm() {
    printf '%s Type %s to go ahead: ' "$1" "${2#/dev/}"
    read -r answer
    [ "$answer" = "${2#/dev/}" ] || die "nothing changed"
}

# Show a disk's partitions: name, size, GPT name.
show_disk() {
    echo "$1: $(($(cat "/sys/block/${1#/dev/}/size") / 2097152)) GiB"
    for p in $(parts_of "$1"); do
        u=/sys/class/block/${p#/dev/}
        printf '  %-10s %6s GiB  %s\n' "$p" "$(($(cat "$u/size") / 2097152))" \
            "$(sed -n 's/^PARTNAME=//p' "$u/uevent")"
    done
}
