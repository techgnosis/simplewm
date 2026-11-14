#!/bin/sh
# Add "Distro A" and "Distro B" to the machine's EFI boot menu: each starts its slot's
# kernel (\EFI\simplewm\a.efi or b.efi, which install.sh puts on the EFI system
# partition of the slots' disk) with root= naming its slot, distro-a or distro-b.
#
#   sudo distro/efibootmgr.sh
#
# Run once, on the Arch host or in the distro (images since efibootmgr was added; init
# mounts efivarfs), after make-slots.sh. The entries name partitions by their IDs, which never change, so
# installs don't need it again. Older Distro entries (the one for the old install on its
# own disk too) are replaced. The new entries go last in the boot order, so the machine
# still starts what it started before; pick Distro A or B in the firmware's boot menu.
# No bootloader: the firmware starts the kernel itself (an EFI program).
set -eu

. "$(dirname "$0")/disk.sh"

need_root
[ -d /sys/firmware/efi ] || die "this machine didn't start through EFI"
[ $# = 0 ] || die "usage: sudo $0"

# Remove older Distro entries (Distro, Distro A, Distro B), keeping the order otherwise.
for num in $(efibootmgr | sed -n 's/^Boot\([0-9A-F]\{4\}\)\*\{0,1\} Distro\b.*/\1/p'); do
    efibootmgr -q -b "$num" -B
done

for slot in a b; do
    part=$(one_part "distro-$slot")
    [ -n "$part" ] || die "no distro-$slot partition; run distro/make-slots.sh first"
    disk=$(disk_of "$part")
    esp=$(esp_of "$disk")
    uuid=$(sfdisk --part-uuid "$disk" "$(part_number "$part")")
    [ -n "$uuid" ] || die "can't read $part's partition ID"
    label="Distro $(echo "$slot" | tr ab AB)"
    cmdline="root=PARTUUID=$uuid rw rootwait"

    order=$(efibootmgr | sed -n 's/^BootOrder: //p')
    efibootmgr -q --create --disk "$disk" --part "$(part_number "$esp")" --label "$label" \
        --loader "\\EFI\\simplewm\\$slot.efi" --unicode "$cmdline"
    num=$(efibootmgr | sed -n "s/^Boot\\([0-9A-F]\\{4\\}\\)\\*\\{0,1\\} $label\\b.*/\\1/p" | head -n 1)
    [ -n "$num" ] || die "the $label entry didn't show up"
    # efibootmgr puts a new entry first; put it last.
    efibootmgr -q -o "${order:+$order,}$num"
    echo "Boot$num: $label, kernel \\EFI\\simplewm\\$slot.efi on $esp, command line: $cmdline"
done
efibootmgr | grep '^BootOrder:'
