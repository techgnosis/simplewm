#!/bin/sh
# Which build is in each slot, which slot is running, and where install.sh would go next.
#
#   distro/check.sh          the running slot and out/ (no root needed)
#   sudo distro/check.sh     also reads the other slot
#
# A build is dated by /usr/bin/simplewm in it, which build.sh writes while filling root/,
# so two slots with the same date hold the same build. The other slot is read with
# debugfs, which opens the partition read-only and doesn't mount it. Changes nothing.
# Works from Arch and from the distro.
set -eu

. "$(dirname "$0")/disk.sh"

# The date of the build in $1: a directory (a mounted system) or an ext4 image/partition.
built() {
    if [ -d "$1" ]; then
        t=$(stat -c %Y "$1/usr/bin/simplewm" 2>/dev/null) || t=
    else
        t=$(debugfs -R 'stat /usr/bin/simplewm' "$1" 2>/dev/null |
            sed -n 's/^ *mtime: 0x\([0-9a-f]*\).*/\1/p')
        [ -z "$t" ] || t=$(printf '%d' "0x$t")
    fi
    if [ -n "$t" ]; then
        echo "$t $(date -d "@$t" '+%a %b %-d %Y, %H:%M')"
    else
        echo "0 no build found"
    fi
}

a=$(one_part distro-a)
b=$(one_part distro-b)
[ -n "$a" ] && [ -n "$b" ] || die "no distro-a and distro-b partitions on this machine"
running=
[ "$(dev_number "$a")" = "$(root_dev_number)" ] && running=a
[ "$(dev_number "$b")" = "$(root_dev_number)" ] && running=b

newest=0
for slot in a b; do
    if [ "$slot" = a ]; then part=$a; else part=$b; fi
    if [ "$slot" = "$running" ]; then
        set -- $(built /)
        note="  <- running now"
    elif [ "$(id -u)" = 0 ]; then
        set -- $(built "$part")
        note=
    else
        set -- 0 "not read (run with sudo to see it)"
        note=
    fi
    eval "time_$slot=$1"
    [ "$1" -le "$newest" ] || newest=$1
    shift
    printf 'distro-%s (%s): %s%s\n' "$slot" "$part" "$*" "$note"
done

if [ -e "$DISTRO/out/root.img" ]; then
    set -- $(built "$DISTRO/out/root.img")
    shift
    echo "out/root.img:          $*"
fi
echo

if [ "$time_a" -gt 0 ] && [ "$time_b" -gt 0 ]; then
    if [ "$time_a" = "$time_b" ]; then
        echo "Both slots hold the same build."
    elif [ "$time_a" -gt "$time_b" ]; then
        echo "distro-a has the newer build."
    else
        echo "distro-b has the newer build."
    fi
fi
case $running in
    a | b)
        other=$(echo "$running" | tr ab ba)
        echo "Running distro-$running. sudo distro/install.sh would write distro-$other."
        ;;
    *) echo "Not running from a slot (Arch?). install.sh needs a or b." ;;
esac
current=$(efibootmgr 2>/dev/null | sed -n 's/^BootCurrent: //p') || current=
if [ -n "$current" ]; then
    name=$(efibootmgr | sed -n "s/^Boot$current\* \([^	]*\).*/\1/p")
    echo "The firmware started \"$name\" this boot."
fi
