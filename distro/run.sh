#!/bin/sh
# Boot what distro/build.sh made in QEMU.
#
#   distro/run.sh            the screen in a window (needs qemu-ui-gtk and
#                            qemu-hw-display-virtio-vga on Arch; qemu-desktop has both)
#   distro/run.sh headless [image]
#                            no window, for testing boots from a script; changes to
#                            out/root.img are thrown away when QEMU exits. image: a test
#                            copy to boot instead, which is written to (so a test can
#                            leave results in it to read with debugfs afterwards)
#
# Either way the serial console is this terminal: the kernel's and simplewm-init's
# messages (output only). Ctrl+A then X quits QEMU; Ctrl+A then C its monitor.
#
# Network: QEMU's user-mode network. In the Network applet: address 10.0.2.15/24,
# gateway 10.0.2.2, DNS 10.0.2.3.
# Sound: only if QEMU has its PipeWire driver (qemu-audio-pipewire on Arch).
set -eu

out=$(cd "$(dirname "$0")" && pwd)/out
for f in vmlinuz root.img; do
    [ -e "$out/$f" ] || { echo "no $out/$f; run distro/build.sh first" >&2; exit 1; }
done
image=$out/root.img
test_image=
[ "${1:-}" = headless ] && [ -n "${2:-}" ] && image=$2 && test_image=yes

# The VM's screen fills what a window can use on the host's monitor: its preferred mode
# (the first line of a connected connector's modes, as simplewm picks it), less
# simplewm's bar (30) and a window's title bar and borders (32 high, 8 wide), rounded
# down to multiples of 8. 1920x1080 if no monitor is found (a nested host, say).
host=
for c in /sys/class/drm/card*-*; do
    if [ "$(cat "$c/status" 2>/dev/null)" = connected ]; then
        host=$(head -n 1 "$c/modes" 2>/dev/null)
        break
    fi
done
case $host in
    [0-9]*x[0-9]*) ;;
    *) host=1920x1080 ;;
esac
xres=$(( (${host%x*} - 8) / 8 * 8 ))
yres=$(( (${host#*x} - 30 - 32) / 8 * 8 ))

# A plain virtio GPU, no OpenGL passthrough: Alpine's Mesa has no virgl driver, so the
# guest draws in software either way (see CLAUDE.md). No menu bar (Ctrl+Alt+M shows it).
# QEMU scales the picture to the window: the window passes its size to the guest as the
# preferred mode, which simplewm follows, so simplewm's picture is pixel for pixel. Without
# scaling, the window snapped to the guest's picture, and after the text console came back
# (small) window and guest held each other small.
gpu="-device virtio-vga,xres=$xres,yres=$yres"
snapshot=
if [ "${1:-}" = headless ]; then
    screen="$gpu -display none"
    # Test boots never change out/root.img: writes go to a temporary layer that's thrown
    # away when QEMU exits. A test copy is written to.
    [ -n "$test_image" ] || snapshot=-snapshot
elif qemu-system-x86_64 -display help 2>/dev/null | grep -qx gtk; then
    screen="$gpu -display gtk,show-menubar=off"
else
    echo "QEMU has no GTK window; running headless" >&2
    screen="$gpu -display none"
fi
echo "VM screen: ${xres}x${yres} (host monitor $host)" >&2

sound=
if qemu-system-x86_64 -audiodev help 2>/dev/null | grep -qx pipewire; then
    sound="-audiodev pipewire,id=snd0 -device intel-hda -device hda-output,audiodev=snd0"
else
    echo "QEMU has no PipeWire audio driver; no sound card" >&2
fi

# shellcheck disable=SC2086
exec qemu-system-x86_64 \
    -enable-kvm -cpu host -smp 4 -m 2G \
    -kernel "$out/vmlinuz" \
    -append "root=/dev/vda rw console=tty0 console=ttyS0 video=Virtual-1:${xres}x${yres}" \
    -drive "file=$image,format=raw,if=virtio" $snapshot \
    -vga none $screen \
    -device virtio-keyboard-pci -device virtio-tablet-pci \
    -nic user,model=virtio-net-pci \
    $sound \
    -serial mon:stdio
