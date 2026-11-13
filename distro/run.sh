#!/bin/sh
# Boot what distro/build.sh made in QEMU.
#
#   distro/run.sh            the screen in a window, with OpenGL passed through to the
#                            host's GPU (needs qemu-ui-gtk, qemu-ui-opengl and
#                            qemu-hw-display-virtio-vga-gl on Arch)
#   distro/run.sh headless   no window; QEMU's standard VGA card (software rendering in the
#                            guest), for testing boots from a script
#
# Either way the serial console is this terminal: kernel messages and a root shell.
# Ctrl+A then X quits QEMU.
#
# Network: QEMU's user-mode network. In the Network applet: address 10.0.2.15/24,
# gateway 10.0.2.2, DNS 10.0.2.3.
# Sound: only if QEMU has its PipeWire driver (qemu-audio-pipewire on Arch).
set -eu

out=$(cd "$(dirname "$0")" && pwd)/out
for f in vmlinuz initramfs.cpio.gz root.img; do
    [ -e "$out/$f" ] || { echo "no $out/$f; run distro/build.sh first" >&2; exit 1; }
done

if [ "${1:-}" = headless ]; then
    screen="-display none -device VGA"
elif qemu-system-x86_64 -display help 2>/dev/null | grep -qx gtk &&
    qemu-system-x86_64 -device help 2>/dev/null | grep -q '"virtio-vga-gl"'; then
    screen="-device virtio-vga-gl -display gtk,gl=on"
else
    echo "QEMU has no GTK window or virtio-vga-gl; running headless" >&2
    screen="-display none -device VGA"
fi

sound=
if qemu-system-x86_64 -audiodev help 2>/dev/null | grep -qx pipewire; then
    sound="-audiodev pipewire,id=snd0 -device intel-hda -device hda-output,audiodev=snd0"
else
    echo "QEMU has no PipeWire audio driver; no sound card" >&2
fi

# shellcheck disable=SC2086
exec qemu-system-x86_64 \
    -enable-kvm -cpu host -smp 2 -m 2G \
    -kernel "$out/vmlinuz" \
    -initrd "$out/initramfs.cpio.gz" \
    -append "root=/dev/vda console=tty0 console=ttyS0" \
    -drive "file=$out/root.img,format=raw,if=virtio" \
    -vga none $screen \
    -device virtio-keyboard-pci -device virtio-tablet-pci \
    -nic user,model=virtio-net-pci \
    $sound \
    -serial mon:stdio
