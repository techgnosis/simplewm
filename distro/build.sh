#!/bin/sh
# Build the distro from Alpine packages:
#
#   target/alpine/release/          simplewm and simplewm-init for musl (build-simplewm.sh)
#   distro/root/                  the root filesystem (apk installs into it)
#   distro/out/vmlinuz            the kernel, built from distro/kernel.config (kernel.sh)
#   distro/out/root.img           distro/root/ as an ext4 disk image
#
# Run it on the host; it re-runs itself inside the Alpine container (container.sh).
# Each build starts from an empty root, so the user's home (simplewm's settings) starts
# empty too. Downloaded packages are kept in distro/cache/.
set -eu

if [ ! -e /etc/alpine-release ]; then
    exec "$(dirname "$0")/container.sh" distro/build.sh "$@"
fi

cd /work/distro

# What goes into the root filesystem. wayland-utils (wayland-info) is for checking what
# simplewm offers apps. The last group is for development: Rust (rustup-init installs the
# toolchain into the user's home; it needs curl, CA certificates and git, and a C
# toolchain to link), C (build-base: gcc, musl headers, make, binutils), Go and Python,
# plus GNU grep and sed (busybox's lack options people and LLMs use, like grep -P).
PACKAGES="
    alpine-baselayout
    busybox
    bash
    musl-utils
    procps-ng
    kbd

    eudev
    eudev-hwids
    seatd

    libgcc
    libseat
    libinput-libs
    libxkbcommon
    xkeyboard-config
    mesa-gbm
    mesa-egl
    mesa-gles
    mesa-dri-gallium

    dbus
    pipewire
    pipewire-pulse
    pipewire-tools
    wireplumber

    iproute2
    sudo
    shared-mime-info

    alacritty
    firefox

    curl
    ca-certificates
    git
    grep
    sed
    build-base
    rustup
    go
    python3
    py3-pip
    font-dejavu
    wayland-utils
"

# The user simplewm runs as (UID 1000, which is how simplewm-init finds them), and their
# password (for sudo, which the Network applet uses). Development values; the password
# is the same in every build.
USER_NAME=user
USER_PASSWORD=simplewm
USER_GROUPS="seat audio video input wheel"

# What the system doesn't need, removed at the end of the build (see "A smaller
# filesystem" in CLAUDE.md for why each goes). Nothing can bring them back: there's no
# package manager in the system.
#
# REMOVE: files and folders deleted with whatever is in them.
REMOVE="
    /boot

    /etc/inittab
    /etc/fstab
    /etc/motd
    /etc/securetty
    /etc/modules
    /etc/sysctl.conf
    /etc/nsswitch.conf
    /etc/vdpau_wrapper.cfg
    /etc/sudo.conf
    /etc/sudo_logsrvd.conf
    /etc/crontabs
    /etc/periodic
    /etc/network
    /etc/udhcpc
    /etc/logrotate.d
    /etc/pam.d
    /etc/security
    /etc/pkcs11
    /etc/busybox-paths.d
    /etc/xdg/autostart
    /etc/xdg/Xwayland-session.d
    /etc/profile.d/README
    /etc/profile.d/color_prompt.sh.disabled
    /etc/dbus-1/system.conf

    /usr/lib/udev/hwdb.d
    /usr/libexec/pipewire-launcher
    /usr/share/keymaps
    /usr/share/consoletrans
    /usr/share/unimaps
    /usr/share/consolefonts
    /usr/share/metainfo
    /usr/share/fonts/encodings
    /usr/bin/mkfontscale
    /usr/bin/mkfontdir
    /usr/bin/start-pulseaudio-x11
    /usr/bin/pax11publish
    /usr/bin/broadwayd
    /usr/bin/sudoreplay
    /usr/sbin/sudo_logsrvd
    /usr/sbin/sudo_sendlog
    /usr/bin/libcamera-bug-report
    /usr/share/paxrelabel.d

    /usr/share/dbus-1/system.conf
    /usr/share/dbus-1/system.d
    /usr/share/dbus-1/system-services
    /usr/share/dbus-1/services/org.a11y.Bus.service
    /usr/share/dbus-1/accessibility-services
    /usr/libexec/at-spi-bus-launcher
    /usr/libexec/at-spi2-registryd
    /usr/share/defaults
    /usr/lib/gnome-settings-daemon-3.0
    /usr/share/udhcpc

    /var/spool
    /var/log/apk.log
"
# REMOVE_DIRS: folders that should be empty (or hold only empty folders). If a package
# ever puts files in one, the build stops and lists them instead of deleting them.
REMOVE_DIRS="
    /media
    /mnt
    /opt
    /srv
    /usr/local

    /etc/opt
    /etc/xdg
    /etc/alsa
    /etc/udev/rules.d
    /etc/modules-load.d
    /etc/sysctl.d
    /lib/modules-load.d
    /lib/sysctl.d
    /usr/lib/modules-load.d

    /var/mail
    /var/opt
    /var/local
    /var/db
    /var/empty
    /var/lib/misc
    /var/lib/dbus
    /var/cache/misc
    /var/lib/sudo/lectured
    /usr/share/dbus-1/session.d
    /usr/share/dbus-1/services
    /usr/lib/gio/modules
"

ROOT=/work/distro/root
OUT=/work/distro/out
CACHE=/work/distro/cache

sh build-simplewm.sh

echo "== tools"
apk add -q e2fsprogs

echo "== root filesystem"
rm -rf "$ROOT" "$OUT"
mkdir -p "$ROOT" "$OUT" "$CACHE"
# shellcheck disable=SC2086
apk --root "$ROOT" --initdb --quiet \
    --keys-dir /etc/apk/keys --repositories-file /etc/apk/repositories \
    --cache-dir "$CACHE" \
    add $PACKAGES

# No package manager in the system: it's changed only by rebuilding it. apk itself was
# never installed; remove its records (database and settings) too.
rm -rf "$ROOT/etc/apk" "$ROOT/lib/apk" "$ROOT/var/cache/apk"

echo "== our pieces"
install -m 755 /work/target/alpine/release/simplewm "$ROOT/usr/bin/simplewm"
# simplewm-init is PID 1; it runs simplewm as the account with UID 1000 (made below).
install -m 755 /work/target/alpine/release/simplewm-init "$ROOT/sbin/simplewm-init"
ln -sf simplewm-init "$ROOT/sbin/init"
echo simplewm > "$ROOT/etc/hostname"
install -m 644 firefox.js "$ROOT/usr/lib/firefox/defaults/pref/simplewm.js"
# udevd reads the hardware database from this compiled file.
chroot "$ROOT" udevadm hwdb --update
# dbus-daemon wants a machine ID.
chroot "$ROOT" dbus-uuidgen --ensure=/etc/machine-id

# bash is the login shell (root's too); /bin/sh stays busybox for scripts.
sed -i 's|^root:\(.*\):/bin/sh$|root:\1:/bin/bash|' "$ROOT/etc/passwd"
chroot "$ROOT" adduser -D -u 1000 -s /bin/bash -g "$USER_NAME" "$USER_NAME"
for g in $USER_GROUPS; do
    chroot "$ROOT" addgroup "$USER_NAME" "$g"
done
echo "$USER_NAME:$USER_PASSWORD" | chroot "$ROOT" chpasswd >/dev/null
# Members of wheel may use sudo, with their own password, and without sudo's lecture.
printf '%%wheel ALL=(ALL:ALL) ALL\nDefaults lecture=never\n' > "$ROOT/etc/sudoers.d/wheel"
chmod 440 "$ROOT/etc/sudoers.d/wheel"

# Only the accounts the system uses: root and the user. Alpine's base layout adds ones for
# services that don't run here (mail, news, cron, sshd, …), and dbus adds messagebus for
# the system bus, which doesn't run either (simplewm runs the session bus). Its setuid
# helper for starting system-bus services goes too. The tools' backup copies (passwd-,
# shadow-, group-) go as well.
KEEP_ACCOUNTS="root $USER_NAME"
for file in passwd shadow; do
    awk -F: -v keep=" $KEEP_ACCOUNTS " 'index(keep, " " $1 " ")' \
        "$ROOT/etc/$file" > "$ROOT/etc/$file.new"
    cat "$ROOT/etc/$file.new" > "$ROOT/etc/$file"
    rm "$ROOT/etc/$file.new"
done
rm -f "$ROOT/etc/passwd-" "$ROOT/etc/shadow-" "$ROOT/etc/group-"
# Alpine ships root with an empty password: anything running as the user could `su` to
# root without one. Lock it; the user gets root through sudo, with their own password.
sed -i 's/^root:[^:]*:/root:*:/' "$ROOT/etc/shadow"
rm -f "$ROOT/usr/libexec/dbus-daemon-launch-helper"

echo "== trimming"
for path in $REMOVE; do
    rm -rf "${ROOT:?}$path"
done
# The one console font simplewm-init loads, unpacked from font-terminus (the rest of
# that package, 238 more console fonts and Terminus for apps, isn't wanted).
apk fetch --quiet --output "$CACHE" font-terminus
mkdir -p "$ROOT/usr/share/consolefonts"
tar -xzf "$(ls -t "$CACHE"/font-terminus-*.apk | head -n 1)" -C "$ROOT" \
    usr/share/consolefonts/ter-132n.psf.gz 2>/dev/null
for dir in $REMOVE_DIRS; do
    [ -e "$ROOT$dir" ] || continue
    find "$ROOT$dir" -depth -type d -empty -delete
    if [ -e "$ROOT$dir" ]; then
        echo "$dir isn't empty; not removing it:" >&2
        find "$ROOT$dir" -not -type d >&2
        exit 1
    fi
done

echo "== disk image"
used=$(du -sm "$ROOT" | cut -f1)
# Room to work in (a Rust toolchain alone unpacks to well over 1 GB). The image is a sparse
# file: space the system never writes takes nothing on the host's disk.
size=16384
[ "$size" -gt $((used * 2)) ] || size=$((used * 2))
mkfs.ext4 -q -L simplewm -d "$ROOT" "$OUT/root.img" "${size}M"

sh kernel.sh

echo "== done: root ${used} MB, image ${size} MB"
