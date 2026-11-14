#!/bin/bash
# Boot a throwaway copy of out/root.img (with out/vmlinuz) headless, and collect what the
# checks in test/check.sh found:
#
#   distro/test-boot.sh [name]
#
# Leaves in out/test/: <name>-check.log (services, devices, sound, user namespaces,
# bubblewrap, a headless Firefox screenshot, kernel warnings and the whole dmesg),
# <name>-serial.log (the serial console) and <name>.png (the screen near the end).
# Takes about 2.5 minutes. The copy is deleted afterwards; out/root.img isn't touched.
set -eu
distro=$(cd "$(dirname "$0")" && pwd)
name=${1:-test}
dir=$distro/out/test
mkdir -p "$dir"
img=$dir/$name.img
cp --sparse=always "$distro/out/root.img" "$img"
"$distro/container.sh" sh /work/distro/test/prep.sh "/work/distro/out/test/$name.img" \
    >/dev/null 2>&1
# check.sh needs about 100 s after boot; then a screenshot through QEMU's monitor and quit.
( sleep 130; printf '\001c'; sleep 1; echo "screendump $dir/$name.ppm"; sleep 3
  echo quit; sleep 2 ) |
    timeout 200 "$distro/run.sh" headless "$img" 2>&1 | grep -av '^(qemu)' \
    > "$dir/$name-serial.log" || true
"$distro/container.sh" sh -c \
    "apk add -q e2fsprogs-extra && debugfs -c -R 'cat /home/user/check.log' /work/distro/out/test/$name.img" \
    > "$dir/$name-check.log" 2>/dev/null
magick "$dir/$name.ppm" "$dir/$name.png" 2>/dev/null || true
rm -f "$img" "$dir/$name.ppm"
grep -q '^== END' "$dir/$name-check.log" || echo "check.log incomplete" >&2
echo "out/test/$name-check.log, $name-serial.log, $name.png"
