#!/bin/sh
# Build the distro from Alpine packages:
#
#   target/alpine/release/          simplewm and simplewm-init for musl (build-simplewm.sh)
#   target/gram/release/            the Gram editor for musl (build-gram.sh)
#   distro/root/                  the root filesystem (apk installs into it)
#   distro/out/vmlinuz            the kernel, built from distro/kernel.config (build-kernel.sh)
#   distro/out/root.img           distro/root/ as an ext4 disk image
#
# Run it on the host (it re-runs itself inside the Alpine container, container.sh) or in
# the distro (natively, see where.sh). In the distro, the part that installs the root
# filesystem needs real root (files owned by root, setuid sudo, chroot): build.sh re-runs
# itself through sudo for that part only (`build.sh root-part`, asking for your password
# once), and everything else, the compiling included, runs as you. In the container,
# podman's root does it all.
# Each build starts from an empty root, so the image's home (simplewm's settings) starts
# empty too. Downloaded packages are kept in distro/cache/.
set -eu

. "$(dirname "$0")/where.sh"
. "$PROJECT/distro/apk.sh"

cd "$PROJECT/distro"

# What goes into the root filesystem. Vulkan (the loader, radv for AMD cards, lavapipe
# for software rendering in the VM) is for Gram, which draws with Vulkan, or OpenGL if
# there's none. wayland-utils (wayland-info) is for checking what
# simplewm offers apps. The last group is for development: Rust (rustup, which the build
# uses to install RUST_VERSION into the user's home; it needs curl, CA certificates and
# git, and a C toolchain to link), C (build-base: gcc, musl headers, make, binutils), Go and Python,
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
    vulkan-loader
    mesa-vulkan-ati
    mesa-vulkan-swrast
    mesa-vulkan-virtio

    curl
    ca-certificates
    git
    openssh-client-default
    grep
    sed
    build-base
    rustup
    go
    python3
    py3-pip

    pkgconf
    linux-headers
    eudev-dev
    libseat-dev
    libinput-dev
    libxkbcommon-dev
    mesa-dev

    bison
    flex
    perl
    gawk
    diffutils
    findutils
    xz
    zstd
    elfutils-dev
    openssl-dev
    ncurses-dev

    cmake
    wayland-dev
    fontconfig-dev
    zstd-dev
    sqlite-dev
    vulkan-loader-dev

    font-dejavu
    wayland-utils
"

# The user simplewm runs as (UID 1000, which is how simplewm-init finds them), and their
# password (for sudo, which the Network applet uses). Development values; the password
# is the same in every build.
USER_NAME=user
USER_PASSWORD=simplewm
USER_GROUPS="seat audio video input wheel"

# The Rust toolchain installed in the user's home (rustup, with rustfmt and clippy), the
# same as simplewm's rust-version.
RUST_VERSION=1.100

# What the system doesn't need, removed at the end of the build (see "A smaller
# filesystem" in CLAUDE.md for why each goes). Nothing can bring them back: there's no
# package manager in the system.
#
# REMOVE: files and folders deleted with whatever is in them.
REMOVE="
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
    /dev/null

    /etc/modprobe.d
    /etc/pulse
    /etc/gtk-3.0
    /usr/lib/sysctl.d

    /etc/ssl1.1
    /etc/ssl/*.dist
    /etc/ssl/ct_log_list.cnf
    /etc/ca-certificates
    /etc/ca-certificates.conf
    /usr/share/ca-certificates
    /usr/sbin/update-ca-certificates
    /usr/bin/c_rehash

    /etc/terminfo/g
    /etc/terminfo/k
    /etc/terminfo/p
    /etc/terminfo/r
    /etc/terminfo/s/st-*
    /etc/terminfo/s/sun
    /etc/terminfo/t/terminator
    /etc/terminfo/t/terminology*
    /etc/terminfo/v/vt52
    /etc/terminfo/v/vt200
    /etc/terminfo/v/vte*

    /usr/bin/lto-dump
    /usr/lib/gcc/*/*/plugin
    /usr/lib/gcc/*/*/install-tools
    /usr/lib/gcc/*/*/libcaf_single.a
    /usr/libexec/gcc/*/*/plugin
    /usr/libexec/gcc/*/*/install-tools

    /usr/share/X11/Xcms.txt
    /usr/share/X11/XErrorDB
    /usr/share/X11/xkb/geometry

    /usr/lib/bash
    /usr/lib/gtk-2.0
    /usr/lib/vdpau
    /usr/lib/cairo
    /usr/lib/engines-3
    /usr/lib/gtk-3.0/*/printbackends/libprintbackend-cups.so
    /usr/lib/gtk-3.0/*/printbackends/libprintbackend-lpr.so
    /usr/share/glib-2.0/schemas/org.gtk.exampleapp.gschema.xml
    /usr/share/gtk-3.0
    /usr/share/gettext
    /usr/share/libcamera/pipeline
    /usr/bin/libcamerify
    /usr/lib/python3.14/turtledemo
    /usr/lib/python3.14/turtle.py
    /usr/lib/python3.14/__pycache__/turtle.*

    /usr/bin/pacmd
    /usr/bin/padsp
    /usr/bin/pa-info
    /usr/lib/pulseaudio/libpulsedsp.so
    /usr/lib/pulseaudio/libpulsecore-*.so
    /usr/bin/pipewire-aes67
    /usr/bin/pipewire-avb
    /usr/bin/pipewire-vulkan
    /usr/share/pipewire/pipewire-aes67.conf
    /usr/share/pipewire/pipewire-avb.conf
    /usr/share/pipewire/pipewire-vulkan.conf
    /usr/share/pipewire/minimal.conf
    /usr/share/spa-0.2
    /usr/bin/aserver
    /usr/bin/sndfile-*

    /usr/bin/chvt
    /usr/bin/deallocvt
    /usr/bin/dumpkeys
    /usr/bin/fgconsole
    /usr/bin/getkeycodes
    /usr/bin/kbdinfo
    /usr/bin/kbdrate
    /usr/bin/loadkeys
    /usr/bin/loadunimap
    /usr/bin/mapscrn
    /usr/bin/open
    /usr/bin/openvt
    /usr/bin/psf*table
    /usr/bin/resizecons
    /usr/bin/setkeycodes
    /usr/bin/setleds
    /usr/bin/setmetamode
    /usr/bin/setvtrgb
    /usr/bin/showconsolefont
    /usr/bin/showkey
    /usr/bin/unicode_start
    /usr/bin/unicode_stop
    /bin/kbd_mode

    /usr/bin/bomtool
    /usr/bin/econftool
    /usr/bin/cvtsudoers
    /usr/bin/dbus-test-tool
    /usr/bin/dbus-cleanup-sockets
    /usr/bin/scmp_sys_resolver
    /usr/bin/yuvconvert
    /usr/bin/fribidi
    /usr/bin/git-shell
    /bin/hugetop
    /bin/tload
    /sbin/dpll
    /sbin/netshaper
    /sbin/genl
    /sbin/ifstat
    /sbin/lnstat
    /sbin/ctstat
    /sbin/rtstat
    /sbin/rtacct
    /sbin/rtmon
    /sbin/routel

    /usr/lib/udev/rules.d/60-persistent-storage-tape.rules
    /usr/lib/udev/rules.d/75-probe_mtd.rules
    /usr/lib/udev/mtd_probe
    /usr/lib/udev/rules.d/81-net-dhcp.rules
"
# BUSYBOX: busybox commands for what this system doesn't do (kernel modules, other init
# systems and logins, cron, syslog, mail, DHCP, ifupdown, initramfs, NAND flash, …) or
# that duplicate a real tool (busybox's `ip` shortcuts). Only the links go; the build
# stops if one turns out to be a real program.
BUSYBOX="
    /bin/bbconfig /bin/dumpkmap /bin/fatattr /bin/fdflush /bin/linux32 /bin/linux64
    /bin/login /bin/lzop /bin/makemime /bin/reformime /bin/pipe_progress /bin/run-parts
    /bin/setserial /bin/usleep
    /sbin/acpid /sbin/depmod /sbin/insmod /sbin/lsmod /sbin/modinfo /sbin/modprobe
    /sbin/rmmod /sbin/fbsplash /sbin/getty /sbin/ifdown /sbin/ifup /sbin/ifenslave
    /sbin/klogd /sbin/syslogd /sbin/logread /sbin/mdev /sbin/nameif /sbin/raidautorun
    /sbin/setconsole /sbin/slattach /sbin/tunctl /sbin/udhcpc /sbin/vconfig
    /sbin/watchdog /sbin/zcip /sbin/loadkmap /sbin/ipaddr /sbin/iplink /sbin/ipneigh
    /sbin/iproute /sbin/iprule /sbin/iptunnel /sbin/switch_root /sbin/fsck
    /usr/bin/beep /usr/bin/crontab /usr/bin/last /usr/bin/mesg /usr/bin/pscan
    /usr/bin/udhcpc6 /usr/bin/vlock /usr/bin/volname /usr/bin/ttysize
    /usr/sbin/crond /usr/sbin/sendmail /usr/sbin/nanddump /usr/sbin/nandwrite
    /usr/sbin/nbd-client /usr/sbin/rdev /usr/sbin/readahead /usr/sbin/setlogcons
    /usr/sbin/loadfont /usr/sbin/fbset /usr/sbin/add-shell /usr/sbin/remove-shell
    /usr/sbin/killall5
"
# KEEP_ONLY: one folder per line, then the only names in it that stay; everything else
# in it is deleted. The build stops if a kept name is missing.
KEEP_ONLY="
    /etc/ssl/certs ca-certificates.crt
    /usr/share/mime aliases generic-icons globs globs2 icons magic mime.cache packages subclasses treemagic types version XMLnamespaces
    /usr/share/X11/locale compose.dir locale.alias locale.dir en_US.UTF-8
    /usr/share/X11/xkb/rules evdev
    /usr/share/X11/xkb/keycodes evdev aliases
    /usr/share/X11/xkb/symbols pc us inet srvr_ctrl keypad
    /usr/share/X11/xkb/types complete basic pc iso9995 extra level5 numpad mousekeys
    /usr/share/X11/xkb/compat complete basic ledcaps lednum ledscroll iso9995 mousekeys accessx misc xfree86 level5 caps
    /usr/lib/gtk-3.0/3.0.0/immodules im-wayland.so
    /usr/lib/dri libdril_dri.so kms_swrast_dri.so swrast_dri.so zink_dri.so radeonsi_dri.so r600_dri.so r300_dri.so iris_dri.so crocus_dri.so i915_dri.so nouveau_dri.so virtio_gpu_dri.so vmwgfx_dri.so vkms_dri.so udl_dri.so
    /usr/share/alsa/cards aliases.conf HDA-Intel.conf USB-Audio.conf
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
    /lib/firmware
"

ROOT="$PROJECT/distro/root"
OUT="$PROJECT/distro/out"
CACHE="$PROJECT/distro/cache"
RUST_HOME="$CACHE/home"

# mkfs.ext4 from e2fsprogs: the container's own, or cache/tools/ in the distro (whose
# image doesn't have it), run with that folder's libraries.
mkfs_ext4() {
    if [ -n "$NATIVE" ]; then
        LD_LIBRARY_PATH="$APK_TOOLS/usr/lib" "$APK_TOOLS/sbin/mkfs.ext4" "$@"
    else
        mkfs.ext4 "$@"
    fi
}

# Everything that needs root: the root filesystem and its disk image.
root_part() {
echo "== root filesystem"
rm -rf "$ROOT" "$OUT"
mkdir -p "$ROOT" "$OUT"
# The system's / : root's, mode 755. In the distro the project usually sits in the user's
# home, which busybox's adduser makes setgid; without this every folder apk creates
# would inherit the setgid bit and the user's group.
chown 0:0 "$ROOT"
chmod g-s "$ROOT"
chmod 755 "$ROOT"
# shellcheck disable=SC2086
alpine_apk --root "$ROOT" --initdb --quiet add $PACKAGES

# No package manager in the system: it's changed only by rebuilding it. apk itself was
# never installed; remove its records (database and settings) too.
rm -rf "$ROOT/etc/apk" "$ROOT/lib/apk" "$ROOT/var/cache/apk"

echo "== our pieces"
install -m 755 "$PROJECT"/target/alpine/release/simplewm "$ROOT/usr/bin/simplewm"
# simplewm-init is PID 1; it runs simplewm as the account with UID 1000 (made below).
install -m 755 "$PROJECT"/target/alpine/release/simplewm-init "$ROOT/sbin/simplewm-init"
ln -sf simplewm-init "$ROOT/sbin/init"
echo simplewm > "$ROOT/etc/hostname"
# Gram: the `gram` command finds the editor at ../libexec/gram-editor.
install -m 755 "$PROJECT"/target/gram/release/cli "$ROOT/usr/bin/gram"
install -D -m 755 "$PROJECT"/target/gram/release/gram "$ROOT/usr/libexec/gram-editor"
# Without its debug info (as Gram's own packages do; symbols stay, for backtraces).
strip --strip-debug "$ROOT/usr/bin/gram" "$ROOT/usr/libexec/gram-editor"
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

echo "== Rust $RUST_VERSION for $USER_NAME"
cp -a "$RUST_HOME/.rustup" "$RUST_HOME/.cargo" "$ROOT/home/$USER_NAME/"
chroot "$ROOT" chown -R "$USER_NAME:$USER_NAME" "/home/$USER_NAME/.rustup" \
    "/home/$USER_NAME/.cargo"
# ~/.cargo/bin on PATH for login shells (simplewm is started through one, so its apps
# get it too).
echo '. "$HOME/.cargo/env"' > "$ROOT/home/$USER_NAME/.profile"
chroot "$ROOT" chown "$USER_NAME:$USER_NAME" "/home/$USER_NAME/.profile"

echo "== trimming"
# REMOVE entries may be patterns (gcc's folders are named after its version). One that
# matches nothing is reported: the package dropped it, or the list has a typo.
# Patterns are only matched inside the root: `set -f` keeps the shell from matching them
# against the container's own files while the list is split.
set -f
for path in $REMOVE; do
    set +f
    set -- "${ROOT:?}"$path
    set -f
    found=
    for file in "$@"; do
        [ -e "$file" ] || [ -L "$file" ] || continue
        rm -rf "$file"
        found=1
    done
    [ -n "$found" ] || echo "REMOVE: nothing matches $path" >&2
done
set +f
for path in $BUSYBOX; do
    if [ "$(readlink "$ROOT$path")" != /bin/busybox ]; then
        echo "BUSYBOX: $path isn't a link to busybox" >&2
        exit 1
    fi
    rm "$ROOT$path"
done
echo "$KEEP_ONLY" | while read -r dir keep; do
    [ -n "$dir" ] || continue
    for name in $keep; do
        if [ ! -e "$ROOT$dir/$name" ]; then
            echo "KEEP_ONLY: $dir/$name is missing" >&2
            exit 1
        fi
    done
    for file in "$ROOT$dir"/* "$ROOT$dir"/.[!.]*; do
        [ -e "$file" ] || [ -L "$file" ] || continue
        case " $keep " in
            *" ${file##*/} "*) ;;
            *) rm -rf "$file" ;;
        esac
    done
done
# Caches that listed what was just removed: GTK's input methods and GSettings schemas.
chroot "$ROOT" gtk-query-immodules-3.0 --update-cache
chroot "$ROOT" glib-compile-schemas /usr/share/glib-2.0/schemas
# The one console font simplewm-init loads, unpacked from font-terminus (the rest of
# that package, 238 more console fonts and Terminus for apps, isn't wanted).
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
# Every program and library must still find the libraries it links.
missing=$(find "$ROOT/bin" "$ROOT/sbin" "$ROOT/lib" "$ROOT/usr" -xdev -type f \
        \( -name '*.so*' -o -perm -u+x \) | while read -r file; do
    [ "$(head -c 4 "$file" | tail -c 3)" = ELF ] || continue
    chroot "$ROOT" ldd "${file#"$ROOT"}" 2>&1 | grep 'Error loading shared library' |
        sed "s|^|${file#"$ROOT"}: |"
done)
if [ -n "$missing" ]; then
    echo "Libraries missing after trimming:" >&2
    echo "$missing" >&2
    exit 1
fi

echo "== disk image"
used=$(du -sm "$ROOT" | cut -f1)
# Room to work in (a Rust toolchain alone unpacks to well over 1 GB). The image is a sparse
# file: space the system never writes takes nothing on the host's disk.
size=16384
[ "$size" -gt $((used * 2)) ] || size=$((used * 2))
mkfs_ext4 -q -L simplewm -d "$ROOT" "$OUT/root.img" "${size}M"
echo "== root ${used} MB, image ${size} MB"

# Through sudo in the distro: give out/ (the image) and cache/ (apk's downloads) back to
# the user, so their own builds can write there.
if [ -n "${SUDO_UID:-}" ]; then
    chown -R "$SUDO_UID:$SUDO_GID" "$OUT" "$CACHE"
fi
}

if [ "${1:-}" = root-part ]; then
    root_part
    exit
fi

sh build-simplewm.sh
sh build-gram.sh

echo "== tools"
alpine_tools e2fsprogs
# The one console font the system gets (unpacked in the root part).
alpine_fetch "$CACHE" font-terminus

echo "== Rust $RUST_VERSION (for the user's home)"
# Installed the way the user would themselves, with Alpine's rustup-init, into their home
# (~/.rustup, ~/.cargo), so rustup works as usual afterwards. It's installed once into
# cache/home/ (with HOME pointing there, so ~/.cargo/env says $HOME, not the cache's path)
# and copied in, so later builds don't download it again.
if ! ls -d "$RUST_HOME/.rustup/toolchains/$RUST_VERSION-"* >/dev/null 2>&1; then
    [ -n "$NATIVE" ] || apk add -q rustup
    rm -rf "$RUST_HOME"
    mkdir -p "$RUST_HOME"
    HOME="$RUST_HOME" RUSTUP_HOME="$RUST_HOME/.rustup" CARGO_HOME="$RUST_HOME/.cargo" \
        rustup-init -y -q --no-modify-path \
        --profile minimal --component rustfmt,clippy --default-toolchain "$RUST_VERSION"
fi
# In the distro cache/ usually sits in the user's setgid home (busybox's adduser), so its
# folders inherit the setgid bit; the image's copy shouldn't carry it.
find "$RUST_HOME" -type d -perm -2000 -exec chmod g-s {} +

if [ -n "$NATIVE" ]; then
    echo "== root part, through sudo"
    sudo sh "$PROJECT/distro/build.sh" root-part
else
    root_part
fi

sh build-kernel.sh

echo "== done"
