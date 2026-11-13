#!/bin/sh
# Boot what distro/build.sh made in QEMU. The serial console is this terminal: the
# kernel's messages and a root shell. Ctrl+A then X quits QEMU.
#
# With QEMU's GTK window installed (qemu-ui-gtk, qemu-ui-opengl,
# qemu-hw-display-virtio-vga-gl on Arch), the screen also opens in a window, with
# OpenGL passed through to the host's GPU. Without it, there's no screen, only the serial
# console.
set -eu

out=$(cd "$(dirname "$0")" && pwd)/out
for f in vmlinuz initramfs.cpio.gz root.img; do
    [ -e "$out/$f" ] || { echo "no $out/$f; run distro/build.sh first" >&2; exit 1; }
done

if qemu-system-x86_64 -display help 2>/dev/null | grep -qx gtk &&
    qemu-system-x86_64 -device help 2>/dev/null | grep -q '"virtio-vga-gl"'; then
    screen="-device virtio-vga-gl -display gtk,gl=on"
else
    screen="-display none -vga none"
fi

# shellcheck disable=SC2086
exec qemu-system-x86_64 \
    -enable-kvm -cpu host -smp 2 -m 2G \
    -kernel "$out/vmlinuz" \
    -initrd "$out/initramfs.cpio.gz" \
    -append "root=/dev/vda console=tty0 console=ttyS0" \
    -drive "file=$out/root.img,format=raw,if=virtio" \
    $screen \
    -device virtio-keyboard-pci -device virtio-tablet-pci \
    -serial mon:stdio
