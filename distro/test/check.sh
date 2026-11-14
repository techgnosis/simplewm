#!/bin/bash
# Run in the guest by test/bash_profile, as the user, alongside simplewm. Writes
# ~/check.log, which distro/test-boot.sh reads back from the image.
sleep 20
{
echo "== uname"; uname -a
echo "== procs"; for p in simplewm seatd udevd pipewire pipewire-pulse wireplumber; do printf '%s: ' $p; pgrep -x $p >/dev/null && echo yes || echo NO; done
echo "== bus"; dbus-send --session --print-reply --dest=org.freedesktop.DBus / org.freedesktop.DBus.ListNames 2>&1 | head -20
echo "== devs"; ls /dev/dri /dev/input /dev/snd
echo "== wpctl"; wpctl status 2>&1 | sed -n '1,40p'
echo "== ip"; ip -br link; ip -br addr
echo "== interrupts (MSI should be in use)"; grep -E "MSI|IO-APIC" /proc/interrupts | awk '{print $(NF-2), $(NF-1), $NF}'
echo "== mounts"; cat /proc/mounts
echo "== userns"; unshare -U -r id 2>&1
echo "== bwrap"; bwrap --unshare-all --ro-bind / / --dev /dev --proc /proc true 2>&1 && echo bwrap ok
echo "== firefox"; cd /tmp && echo "<h1>hello</h1>" > /tmp/t.html; timeout 60 firefox --headless --screenshot /tmp/ff.png file:///tmp/t.html 2>&1 | tail -20; ls -la /tmp/ff.png 2>&1
echo "== dmesg warnings"; dmesg -r | grep -E "^<[0-4]>"
echo "== dmesg"; dmesg 2>&1
echo "== END"
} > ~/check.log 2>&1
sync
