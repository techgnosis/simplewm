#!/bin/sh
# Add a "Distro" entry to the machine's EFI boot menu that starts the distro's kernel on
# the disk install.sh set up (it finds the partitions by their names, distro-esp and
# distro-root, on whichever disk has them: /dev/sdb on the user's machine).
#
#   sudo distro/efibootmgr.sh          add it (replacing an older Distro entry)
#
# The entry goes last in the boot order, so the machine still starts what it started
# before; pick Distro in the firmware's boot menu. No bootloader: the
# firmware starts the kernel itself (an EFI program) and hands it the command line
# below. The entry names the partitions by their IDs, which only change if install.sh
# partitions the disk again; then run this again.
set -eu

die() {
    echo "efibootmgr.sh: $*" >&2
    exit 1
}

[ "$(id -u)" = 0 ] || die "needs root: sudo distro/efibootmgr.sh"
[ -d /sys/firmware/efi ] || die "this machine didn't start through EFI"
[ $# = 0 ] || die "usage: sudo distro/efibootmgr.sh"

label() {
    lsblk -lnpo NAME,PARTLABEL | awk -v want="$1" '$2 == want { print $1 }'
}
esp=$(label distro-esp)
root=$(label distro-root)
[ -n "$esp" ] && [ -n "$root" ] || die "no disk with distro-esp and distro-root; run distro/install.sh first"
[ "$(echo "$esp" | wc -l)" = 1 ] || die "more than one distro-esp partition: $esp"

disk=/dev/$(lsblk -no PKNAME "$esp")
partnum=$(cat "/sys/class/block/$(basename "$esp")/partition")
rootuuid=$(lsblk -no PARTUUID "$root")
[ -n "$rootuuid" ] || die "can't read $root's partition ID"
cmdline="root=PARTUUID=$rootuuid rw rootwait"

# Remove older Distro entries, keeping the boot order otherwise as it is.
for num in $(efibootmgr | sed -n 's/^Boot\([0-9A-F]\{4\}\)\*\{0,1\} Distro\b.*/\1/p'); do
    efibootmgr -q -b "$num" -B
done
order=$(efibootmgr | sed -n 's/^BootOrder: //p')

efibootmgr -q --create --disk "$disk" --part "$partnum" --label Distro \
    --loader '\EFI\distro\vmlinuz.efi' --unicode "$cmdline"
num=$(efibootmgr | sed -n 's/^Boot\([0-9A-F]\{4\}\)\*\{0,1\} Distro\b.*/\1/p' | head -n 1)
[ -n "$num" ] || die "the entry didn't show up"
# efibootmgr puts a new entry first; put it last.
efibootmgr -q -o "${order:+$order,}$num"

echo "Boot$num: Distro on $esp ($disk), root $root, command line: $cmdline"
efibootmgr | grep '^BootOrder:'
