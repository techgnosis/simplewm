# A minimal distro for simplewm

Notes for the long-term goal: boot straight into simplewm on a small custom Linux system,
with simplewm doing as much as it can in one process. This file grows as decisions are
made.

## Decisions so far

- **C library: musl.** Small and simple, and well proven on Alpine, Void (musl) and Chimera.
  Mesa, libinput, libxkbcommon, PipeWire and seatd all build with it, and Rust has a musl
  target.
- **Seat manager: seatd.** simplewm talks to the seat through libseat, which works with
  either logind or seatd. seatd is a tiny daemon that runs as root, opens the GPU and
  input devices for simplewm and hands them over, and takes them back when you switch
  virtual terminals. Being in seatd's group is all simplewm needs, so it still runs with no
  special privileges. This also means no systemd and no logind.
- **Devices: eudev.** udev taken out of systemd: the `udevd` daemon with its rules and
  hardware database, and the libudev library smithay, libinput and PipeWire use. It loads
  drivers, sets permissions in `/dev` and tells programs about devices. First choice was
  libudev-zero with mdevd; changed because PipeWire found no sound cards with it. Why,
  below.
- **D-Bus: dbus-daemon, started by simplewm.** Why, below.
- **simplewm runs the user services**, in place of `systemd --user`: the session bus,
  PipeWire, pipewire-pulse and WirePlumber. Done (see "User services" below).
- **Boot: a custom init that starts simplewm as you, with no login.** simplewm-init, a
  Rust program built in this project. It sets up the bare minimum as root, then runs
  simplewm as a normal user. No logind, elogind, PAM or login
  prompt. Details below.
- **Packages: Alpine's binaries, installed by apk into a folder.** No compiling the
  system ourselves; Alpine's musl packages become the root filesystem. Tested in QEMU
  first. Details below.
- **Kernel: our own, with no modules and no initramfs.** Built from kernel.org's
  stable source with the config in `distro/kernel.config`, which we keep improving.
  Every driver is built in. Details in "The kernel" below.

## Devices: the jobs involved

When the machine boots, and whenever something is plugged in, the kernel announces each
device (a "uevent") and makes its file appear in `/dev` (via devtmpfs). Several separate
jobs then follow, and different tools split them up differently:

1. **Loading drivers.** Each device says which driver module it needs (its "modalias").
   Something has to run `modprobe` for those, at boot for what's already there
   ("coldplug") and later for what gets plugged in. Not needed here: our kernel has every
   driver built in and no modules at all (see "The kernel").
2. **Permissions and names in `/dev`.** The kernel creates device files owned by root.
   Something has to give them the right group (for example `audio` for `/dev/snd`, so
   PipeWire can open the sound card) and add friendly links such as `/dev/disk/by-uuid/…`.
   GPU and input devices matter less here, because seatd opens them as root for simplewm.
3. **Describing devices to programs.** Programs ask "which devices are keyboards, mice,
   GPUs, sound cards?" through a library called **libudev**. smithay (and so simplewm),
   libinput and PipeWire all use it. libinput in particular needs to know which input
   devices are keyboards, mice or touchpads.
4. **Hotplug notifications.** libudev also tells running programs when a device appears
   or goes away, so simplewm notices a new mouse or keyboard.

systemd's **udev** does all four: a daemon (`udevd`) follows a set of rules files and a
hardware database ("hwdb"), and the libudev library reads what the daemon worked out. The
two options below are the ways to get libudev without systemd.

## eudev vs libudev-zero

**eudev** is udev taken out of systemd: a fork started by Gentoo in 2012 and now kept by an
independent team. It's the same design as udev: the `udevd` daemon, rules files, the
hardware database and the full libudev. Void and Gentoo use it, and Alpine offers it.

**libudev-zero** is only the library, with no daemon. It answers programs' libudev
questions by reading the kernel's own device information in `/sys` directly, and works out
things like "this is a keyboard" itself from what the device reports. It doesn't load
drivers or set permissions, so something small has to do jobs 1 and 2 (or a kernel with
built-in drivers plus a few lines in the boot script). For hotplug it needs a helper to pass
the kernel's announcements on to it: usually **mdevd** (a small device manager from
skarnet, also musl-friendly), which can load drivers and set permissions too.

|                          | eudev                                    | libudev-zero (+ mdevd)                          |
|--------------------------|------------------------------------------|-------------------------------------------------|
| What it is               | Daemon + rules + hardware database + library | Library only (mdevd is a separate small daemon) |
| Size                     | Medium; a fork of a large codebase       | Very small, easy to read                        |
| Compatibility            | Same behavior as systemd's udev; anything written for udev works | Covers what common programs use; odd cases may differ |
| Driver loading           | Built in (rules)                         | mdevd, or build drivers into the kernel        |
| Permissions and `/dev` links | Built in (rules)                     | mdevd's config file, or a boot script           |
| Hotplug                  | Built in                                 | Needs mdevd (or another helper) to pass events on |
| Hardware quirks database | Yes (hwdb)                               | No; libinput's own quirk files still apply      |
| Predictable network names (`enp3s0`) | Yes (`eudev-netifnames`, not installed) | No; the card stays `eth0`        |
| Sound cards for PipeWire | Yes (marks them `SOUND_INITIALIZED`)     | No: PipeWire finds no cards (found in the VM)   |
| Who uses it              | Void, Gentoo, Alpine (optional)          | KISS Linux, Alpine (optional), small embedded systems |

**What simplewm needs from it:** enumerating the GPU, the monitor's connector and the
input devices at startup, plus hotplug for input. Both options give that. PipeWire also
uses libudev to find the sound card, and that's where they differ (below).

**First choice was libudev-zero with mdevd; changed to eudev.** libudev-zero fit the goal
better on paper: two small, readable pieces, no rules language, and one known machine
means the missing hardware database and network-name rules cost nothing. It worked for the
GPU and input in the VM, but PipeWire found no sound cards. PipeWire's ALSA monitor only
takes a card once udev has set `SOUND_INITIALIZED` on it (`78-sound-card.rules` in
systemd's udev and eudev), and libudev-zero only reports what's in the kernel's `uevent`
file plus the `ID_INPUT_*` guesses it makes for input devices, with no way to add more.
That's the case eudev was kept as the fallback for. The other ways out were patching
libudev-zero (the first thing we'd compile ourselves) or naming the card in a fixed
PipeWire config (no hotplug); the user chose eudev. Without the separate
`eudev-netifnames` package the network card keeps the name `eth0`. A third, less likely
option is building systemd's udev on its own (Chimera Linux does this), which is the most
compatible but the heaviest.

## D-Bus

simplewm's portal needs a D-Bus session bus, and so do many apps. There are three options:

- **dbus-daemon**, the original from 2003. It's reliable and every non-systemd distro uses
  it, but it's the old, larger design.
- **dbus-broker**, the modern rewrite. It's lean, fast and only needs the kernel and a C
  library. Fedora, Arch and now Ubuntu (26.10) use it by default. The catch is that it
  comes in two parts: the broker itself, and `dbus-broker-launch`, which sets it up. The
  launcher needs systemd: it starts services on demand ("activation") as systemd units.
- **busd**, a new broker in Rust, built on zbus, the same D-Bus library simplewm uses. Its
  own README says it isn't ready for production yet.

**First choice was dbus-broker without its launcher; changed to dbus-daemon.** dbus-broker
splits the bus into two programs: the broker, which only passes messages and never opens
files, and a launcher, which reads the config, creates the socket and hands both to the
broker over a private channel. The launcher links against systemd, so we'd have to write
our own, and the format for passing the bus's rules over that channel is private to the
two programs: its man page says it's "still subject to change". dbus-daemon is the plain
model instead: one process that reads its config and serves one socket. It's what Alpine,
Void and the other musl distros use, and its few hundred KB don't matter here.

simplewm starts it as `dbus-daemon --session --nofork --address=unix:path=$XDG_RUNTIME_DIR/bus`
(or whatever `DBUS_SESSION_BUS_ADDRESS` says). It uses the normal session config, so the bus
can still start programs on demand when an app asks for one. If busd (the Rust broker on
zbus) matures, it's the natural next step, maybe even inside simplewm.

## User services

simplewm owns the user services it always needs, without trying to be a smaller systemd:
a fixed list, no unit files, no dependency engine.

- On a TTY it starts the session bus, then PipeWire, then pipewire-pulse and WirePlumber,
  each once the one before answers on its socket.
- Anything already running (systemd's sockets, a host desktop when nested) is left alone
  and shown as run by that program.
- The Sound applet has Start/Stop and Log for PipeWire, pipewire-pulse and WirePlumber.
  The D-Bus applet does the same for the bus. The Log window shows a service's output.
- Nothing restarts a service that exits; you press Start. simplewm stops them when it
  quits, and they're told to stop if simplewm crashes.
- The portal starts once the bus is up and comes back when the bus is restarted.

pipewire-pulse is there because most apps (Firefox, for example) play sound through the
PulseAudio interface it provides, not through PipeWire directly.

To try this on a systemd host, keep systemd from starting the same things by masking
them for the user, then log out and back in:

```
systemctl --user mask pipewire.socket pipewire.service \
    pipewire-pulse.socket pipewire-pulse.service wireplumber.service dbus.socket
```

`systemctl --user unmask` with the same names goes back. The masks apply to every session
of that user, so a second user account just for testing keeps the main one untouched.

## Boot and logind's jobs

On a systemd system, logind does several jobs. Here each one is covered separately:

| logind's job | Here |
|---|---|
| Giving the session the screen and input devices, taking them back on a VT switch | seatd |
| Creating `/run/user/<uid>` (where the Wayland, D-Bus and PipeWire sockets live) | init, before starting simplewm |
| Shutdown, reboot, suspend without being root | Later: simplewm, through the sudo path the Network applet already uses |
| Tracking who's logged in, starting per-user services | Not needed on a one-user machine |
| "Inhibit" requests (don't sleep while a video plays) | Nothing; apps carry on without it |

elogind (logind taken out of systemd) was the drop-in alternative, but it overlaps with
seatd and needs a root-owned D-Bus system bus just for itself.

**The custom init** is PID 1, the first program the kernel runs. It does the bare minimum,
in order:

1. Mount `/proc`, `/sys`, `/dev` and `/run`.
2. Start the root services: udevd (devices) and seatd (screen and input access).
3. Create `/run/user/<uid>`, owned by the user.
4. Switch to the user for good (their user ID and groups, such as `seat` and `audio`), set
   what a login would have set (`HOME`, `USER`, `PATH`, `XDG_RUNTIME_DIR`), send output to
   a log file, and start simplewm.

simplewm then starts the session bus, PipeWire, pipewire-pulse and WirePlumber itself (see
"User services").

simplewm doesn't need to own a terminal: seatd handles the virtual terminal, so Ctrl+Alt+F2
to a text console still works.

Trade-offs accepted:

- No password at boot: anyone at the keyboard gets the desktop. The password still exists
  and sudo still asks for it. A lock screen would have to come from simplewm.
- No PAM, so nothing PAM would do at login happens; the steps above cover what's needed.

If a login prompt is ever wanted, the usual fallback is `getty` with autologin on tty1 and
`exec simplewm` in the shell profile, which goes through the normal login and PAM path.

## Building the root filesystem from Alpine packages

Alpine is a musl distro and already packages everything chosen above: seatd, eudev,
dbus-daemon, PipeWire, pipewire-pulse, WirePlumber, Mesa, libinput,
libxkbcommon, iproute2 and sudo. So instead of building the system from source, we take
Alpine's binaries.

Alpine's package manager, `apk`, can install into any empty folder instead of the running
system (`apk --root <folder> --initdb add …`, with Alpine's repositories and signing keys).
It pulls in every library each package needs, which is why we don't copy files out by
hand: one missed shared library and a program won't start. The distro is then a short
package list plus our own files, rebuilt with one command and updated by running apk again.

The steps, all inside an Alpine container (rootless podman, on the Arch host):

1. Build simplewm for musl. Use Rust from rustup in the container rather than Alpine's
   `rust` package, which may be older than simplewm's `rust-version`. The binary links
   Alpine's C libraries dynamically, so it only runs on Alpine or this distro, not on the
   Arch host.
2. Run apk to fill the root folder from the package list.
3. Add our own pieces: the simplewm binary, the custom init, the user account
   (`/etc/passwd`, `/etc/group`, including the `seat` and `audio` groups) and a few other
   config files.
4. Make the disk image from the folder, still in the container (`mkfs.ext4 -d <folder>`
   needs no root). Building it in there keeps ownership right (see "In the repo").
5. Boot it: our own kernel (see "The kernel"), which mounts the disk itself and runs our
   init as PID 1.

Things to keep in mind:

- **No OpenRC.** Alpine's own boot system only comes in with `alpine-base` or `openrc`;
  leave them out and our init is the only thing that runs.
- **Busybox comes along anyway** and provides `/bin/sh`, `mktemp`, `chmod`, `mv` and `rm`,
  which the Network applet's Apply script needs; `ip` comes from iproute2.
- **Package install scripts** run when apk installs into the folder. They expect Alpine's
  usual layout; usually harmless, but the most likely source of surprises. (busybox's
  warns it can't create device nodes; harmless, the kernel's devtmpfs provides them.)
- **Licensing is fine**: it's all open source and meant to be reused.

**Testing: QEMU first.** QEMU boots the disk image with a virtual graphics card (virtio-gpu)
that simplewm drives through DRM like a real one, so the whole boot can be tried over and
over without touching the real disk. Only once it boots cleanly does it go on a USB stick or
spare partition.

## In the repo: `distro/`

Done so far: the system builds and boots in QEMU straight into simplewm, running as the
user under seatd, with eudev handling devices, simplewm's own session bus, PipeWire and
WirePlumber finding the VM's sound card, and alacritty opening from the launcher. Tested
headless (screenshots through QEMU's monitor).

**Firefox** (Alpine's `firefox`, 151 at the time; `firefox-esr` also exists) is in the
image, and in the Launcher by default: the goal is a Firefox OS. It runs natively on Wayland under simplewm, drawing in
software in the VM. It roughly doubled the root filesystem (419 to 788 MB). Tested by
booting a copy of the image whose `~/.bash_profile` starts it in the background (simplewm
is started through the login shell, so that runs alongside it). `distro/firefox.js` is
installed as `/usr/lib/firefox/defaults/pref/simplewm.js`: `browser.tabs.inTitlebar = 0`
(tabs out of the title bar, so no Firefox minimize/maximize/close buttons; Alt still shows
the menu bar) and `widget.use-xdg-desktop-portal.file-picker = 1` (Ctrl+O and File → Open
File… open simplewm's File Picker; checked). With tabs out of the title bar, GTK 3 drew a
title bar of its own: it only asks KDE's older server-decoration protocol, so simplewm now
answers that one too, always "server side". Not in the Launcher by default yet.

- `container.sh [command…]` runs a command (a shell by default) in an Alpine 3.24
  container with the project root mounted at `/work`. Root in the container is the host
  user, so files owned by root come out as the host user's; anything owned by another
  user needs `podman unshare rm -rf` to delete. It passes `--cgroup-manager=cgroupfs
  --events-backend=file`, because when simplewm runs the session bus on a TTY, systemd
  isn't on it and podman's default (asking systemd over D-Bus) fails.
**Gram** (https://gram-editor.com, a fork of Zed with no AI or telemetry; 3.5.0) is in
the image, built from source for musl because Alpine doesn't package it, and it's in the
Launcher by default (simplewm's first-run entries). `/usr/bin/gram` is its command-line
tool, which starts the editor at `/usr/libexec/gram-editor`. It draws with Vulkan
(OpenGL if there's none): `vulkan-loader`, `mesa-vulkan-ati` (radv, for the user's RX
580) and `mesa-vulkan-swrast` (lavapipe, for the VM) are in `PACKAGES`. In the VM it
runs on lavapipe and shows an "Unsupported GPU" warning (software rendering is slow),
which Skip dismisses. Checked in the VM: it opens a file, gets simplewm's title bar and
bar tab. It doesn't download language servers unless allowed in its settings. The
editor is 228 MB with its symbols kept (for backtraces; 175 MB without).

- `build-simplewm.sh` builds simplewm for musl in the container into
  `target/alpine/release/`, with Rust from rustup (Alpine's is older than simplewm's
  `rust-version`) and `-C target-feature=-crt-static` so it links Alpine's C libraries
  dynamically. Rust and Cargo's downloads are kept in `cache/`.
- `build-gram.sh` builds Gram for musl in the container into `target/gram/release/`
  (`gram`, the editor, and `cli`, the `gram` command), linking Alpine's C libraries like
  simplewm. The tarball is pinned (`VERSION`, `SHA256`) and kept unpacked in
  `cache/gram-<version>/`; Gram's `rust-toolchain.toml` picks its Rust (1.99.1), which the
  shared rustup in `cache/` installs. Two changes to Gram's build: its optional
  `mimalloc` allocator is turned on (musl's malloc is slow for an editor), and X11 is
  taken out of its `Cargo.toml` files and one `Cargo.lock` line (Gram always turns X11
  on; simplewm has none), so it links only libxkbcommon and loads Wayland and Vulkan at
  run time. The edits are made only when a file still needs them, and an empty
  `.git/logs/HEAD` dated 1970 is put in the source (Gram's build scripts watch it, and a
  missing watched file made Cargo recompile the editor every time): a build with nothing
  new takes seconds, the first one about 35 minutes on the user's 4 cores (most of it the
  editor's final, link-time-optimized step, about 15 minutes). Raising `VERSION`: new
  `SHA256`, and if the X11 edit no longer fits the build stops and says so. Don't edit
  `build-gram.sh` while a build runs: the shell reads it as it goes, and an edit
  mid-build ran a fragment of the new text.
- `build.sh` (run on the host; it re-runs itself in the container) runs
  `build-simplewm.sh` and `build-gram.sh`, then builds everything else from scratch: `root/` (from `PACKAGES`
  via `apk --root … --initdb`; afterwards apk's records, `/etc/apk` and `/lib/apk`,
  are deleted, so the system has no package manager and is changed only by rebuilding
  it; then the folders in `REMOVE_DIRS` are removed, see "A smaller filesystem"), and in `out/`
  `root.img` (ext4) and the kernel (`build-kernel.sh`, see "The kernel"). It adds simplewm, the init, udev's compiled hardware database
  (`udevadm hwdb --update`), a machine ID for
  dbus-daemon, and the user (`USER_NAME`, uid 1000, in seat, audio, video, input and
  wheel, with the development password `USER_PASSWORD` for sudo; wheel may use sudo).
  bash is the login shell for the user and root, and the development consoles run it;
  `/bin/sh` stays busybox, which Alpine's package scripts and the Network applet's script
  expect. procps-ng replaces busybox's `ps`, `top`, `pgrep` and friends (busybox's `ps`
  has no `ps auxf` tree).
  Each build starts from an empty root, so simplewm's settings in the image's home start
  over too (an install on the user's machine keeps its own home partition: see "On the
  user's machine"). Downloaded packages are kept in `cache/`. `root/`, `out/` and `cache/` are
  git-ignored. busybox's install script warns it can't create device nodes; harmless.
- `run.sh` boots `out/` in QEMU with KVM: the serial console is the terminal, output
  only (the kernel's and simplewm-init's messages; Ctrl+A then X quits; Ctrl+A then C switches to QEMU's monitor, where `screendump file.ppm` takes
  a screenshot). The screen is a plain virtio-vga card sized to fill what a window can use
  on the host's monitor (its preferred mode from `/sys/class/drm`, less simplewm's bar and
  a window's title bar and borders; 3832x2096 on the user's 4K monitor), with no GTK menu
  bar (Ctrl+Alt+M shows it). QEMU's GTK window passes its own size to the guest as the
  preferred mode, so the screen follows the window: simplewm switches mode when the window
  is resized or maximized (it follows preferred-mode changes since this turned up), which
  keeps its picture pixel for pixel. QEMU scales the picture to the window
  (`zoom-to-fit`, the default): with scaling off, the window snapped to the guest's
  picture, and once the text console came back after simplewm exited (its framebuffer is
  sized when the driver loads, at the small boot-time window), window and guest held each
  other small. The kernel gets `video=Virtual-1:<size>` so the console's framebuffer is
  full size from the start. In a GTK window
  (`qemu-ui-gtk` and `qemu-hw-display-virtio-vga` on Arch; the user installed
  `qemu-desktop`), or with `run.sh headless` no window, for testing from a script, in QEMU's snapshot
  mode so a test never changes `root.img` (a test once left simplewm's binary renamed in
  the image the user then booted). No
  OpenGL passthrough: Alpine's Mesa is built without virgl (its gallium driver list for
  x86_64 has no `virgl`; Mesa says "virtio_gpu: driver missing"), so everything in the
  guest draws with llvmpipe whatever QEMU offers. (virtio-vga-gl with `egl-headless` also
  crashed QEMU on exit and couldn't take screenshots: "no surface".) Venus (`virtio-vga-gl,venus=on`: the guest's Vulkan
  calls run on the host's GPU, through Alpine's `mesa-vulkan-virtio`) would work with the
  host's QEMU and virglrenderer, but it also needs QEMU's OpenGL display, which breaks
  headless test boots; the user chose to keep the VM simple, since it's mainly for
  testing (2026-11-13). 4 CPUs, because
  llvmpipe at that size is busy. A virtio network card on QEMU's user network (address 10.0.2.15/24, gateway 10.0.2.2, DNS 10.0.2.3), and an
  Intel HDA sound card played through the host's PipeWire if QEMU has that driver
  (`qemu-audio-pipewire`).
- `build-kernel.sh` builds the kernel from `kernel.config` into `out/vmlinuz` (see "The
  kernel"). `run.sh` hands it `root=/dev/vda rw`; there's no initramfs.
- **simplewm-init** (`src/bin/simplewm-init/`, built with simplewm) is `/sbin/init`
  (`/sbin/simplewm-init`, `/sbin/init` links to it). It refuses to run unless it's PID 1.
  In order: it mounts `/proc`, `/sys`, `/dev` (unless it's already mounted: our
  kernel mounts devtmpfs there itself, `DEVTMPFS_MOUNT`), `/dev/pts`, `/dev/shm`, `/run` and `/tmp`; mounts the home partition on `/home` if the disk has
  one (`home.rs`, see "On the user's machine"); starts udevd (`--daemon`), replays
  the devices already there (`udevadm trigger`, subsystems then devices) and `udevadm
  settle`s; loads the Terminus 16x32 console font on tty1 (`setfont`, only after udev:
  the console may be VGA text mode before the GPU driver loads, which only takes fonts 8
  or 9 pixels wide); sets the hostname and brings up `lo`; starts seatd (`-g seat`,
  waiting for its ready line on fd 3); makes `/run/user/1000`; waits briefly for
  `/dev/dri/card0`; then runs simplewm as the user. The user is the account with UID
  1000 (there's no setting): their groups from `/etc/group`, a fresh login-like
  environment (`HOME`, `USER`, `LOGNAME`, `SHELL`, `PATH`, `XDG_RUNTIME_DIR`), output
  appended to `~/simplewm.log`. Every child starts with no signals blocked (simplewm-init
  blocks the ones it reads from a signalfd; inherited, they made simplewm and its
  services ignore SIGTERM).
- **One VT.** Only simplewm runs on tty1, and simplewm has no Ctrl+Alt+F-keys. There are
  no other consoles or shells, and nothing reads the serial port. When simplewm exits,
  the kernel's text console comes back on tty1 and simplewm-init writes why there (and on
  the serial console when the kernel uses one). It never restarts simplewm, whatever
  the reason (quitting on purpose too): it clears tty1, writes how simplewm ended and
  the last 20 lines of `~/simplewm.log`, and starts the user's login shell there (tty1 is
  theirs while it runs). Running `simplewm` there shows what goes wrong as it happens;
  exiting the shell has simplewm-init start simplewm again (fix, exit, read the log tail
  if it fails again).
- **The same environment either way.** simplewm-init starts simplewm through the user's
  login shell too (`bash -l -c 'exec simplewm'`, which replaces itself with simplewm), so
  both get what `/etc/profile` sets (`LANG`, `CHARSET`, …) on top of `HOME`, `USER`,
  `LOGNAME`, `SHELL`, `PATH` and `XDG_RUNTIME_DIR`, with the same IDs and groups. Checked
  in the VM: the only differences are `TERM=linux` (the shell has a terminal) and
  `SHLVL`. simplewm's output goes to `~/simplewm.log` from simplewm-init, to the screen
  from the shell.
- **Shutting down:** busybox's `poweroff`, `reboot` and `halt` signal PID 1 (SIGUSR2,
  SIGTERM, SIGUSR1); from simplewm that's `sudo poweroff` in a terminal. simplewm-init
  SIGKILLs the user's shell's session if one is open (an interactive bash ignores
  SIGTERM), sends everything else SIGTERM, names and SIGKILLs whatever is left after
  5 s, syncs, remounts `/` read-only and powers off, reboots or halts. Ctrl+Alt+Del is
  turned off. Logs: `~user/simplewm.log`, `/var/log/seatd.log` (udevd's output goes
  nowhere for now).
- Device rules are eudev's own (`/usr/lib/udev/rules.d`): `/dev/dri` goes to video
  (render nodes 0666), `/dev/snd` to audio and input devices to input, and sound cards get
  `SOUND_INITIALIZED`. Its rules for loading drivers by modalias find nothing to load (the
  kernel has no modules), and kmod's tools (`modprobe`, `lsmod`) aren't installed.

**Fixed: software rendering showed black tiles.** With llvmpipe, parts of the screen came
out as black 64×64 blocks: frames were queued before the renderer's worker threads
finished. simplewm now waits for rendering when smithay says the frame `needs_sync()`
(no fence the kernel can wait on).

**Fixed: apps couldn't find the GPU.** Under virtio-gpu, EGL can't name a render node
(no `EGL_EXT_device_drm`), so simplewm offered no default dmabuf feedback and Mesa in
alacritty found no device ("failed to get driver name for fd -1"). simplewm now falls back
to the render node of the GPU it opened.

**Fixed: no sound with libudev-zero.** PipeWire found no sound cards; switching to eudev
fixed it (see "eudev vs libudev-zero").

**Development tools** are in the image, a group of their own in `PACKAGES`: Rust (Alpine's
`rustup`; it needs `curl`, `ca-certificates` and `git`), C (`build-base`: gcc, musl headers, make,
binutils; Rust needs a C linker), Go (`go`), Python (`python3`, `py3-pip`), and GNU
`grep` and `sed` (busybox's lack options people and LLMs use, like `grep -P`). That
roughly 530 MB took the root filesystem from 753 to 1281 MB, most of it Go and gcc.
`ca-certificates` also fills `/etc/ssl/certs`, which was empty: until then only Firefox
(which carries its own) could check HTTPS certificates. The disk image is 16 GB (or twice what's
installed, if more): rustup failed with "no space left" when it was sized at what's
installed plus half. It's a sparse file, so only what the system writes takes room on the
host (1.4 GB at the time).

**Rust is installed** for the user by the build: `RUST_VERSION` in `build.sh` (1.100,
simplewm's `rust-version`), minimal profile plus rustfmt and clippy (no rust-docs or
rust-src; `rustup component add` brings them). Installed with the container's Alpine
`rustup-init` into `cache/home/` (`HOME` pointed there, so `~/.cargo/env` says `$HOME`;
rustup warns that `$HOME` differs from root's, harmless), only when that version isn't
there yet, then copied into `/home/user` (`~/.rustup`, `~/.cargo`, about 800 MB) and
given to the user. `~/.profile` sources `~/.cargo/env`, so login shells, simplewm and
its apps have `~/.cargo/bin` on `PATH`. rustup works as usual afterwards (`rustup
update`, other toolchains). Checked in a chroot: `bash -l` as the user builds and runs a
`cargo new` project. Testing in a chroot of `root/` needs `/proc` and `/dev/null` bound
from outside: an apk install script once left its error message in a plain-file
`/dev/null` (now in `REMOVE`; at boot devtmpfs covers `/dev` anyway), which cargo fed to
rustc.

**SSH keys:** the user asked for their host `~/.ssh` (one ed25519 key, `known_hosts`) in
the image's `/home/user/.ssh`, for git over SSH; `openssh-client-default` (the `ssh`
client, no server) came with it. `build.sh` copies it on the host side into
`cache/ssh` (git-ignored; the container sees only the project) before entering the
container, fresh each build, then into the home as 700/600, owned by the user. So
`out/root.img` holds a private key and must never be shared or committed. Like the rest
of the home, it reaches an installed `distro-home` only when that partition is made;
after that the user copies `~/.ssh` over themselves.

**A smaller filesystem.** The user wants as few files in the filesystem as possible, to
lighten the load on whoever looks around it: the measure is the number of files, not
bytes (a 40 MB binary and a one-line config file count the same). But it stays a normal userland a developer
can work in (rustup and Rust, Go, Gram, alacritty, curl, pkg-config, the usual tools): only
obscure things nobody would use go. `build.sh` trims at the end of the build
with four lists (`REMOVE`, `BUSYBOX`, `KEEP_ONLY`, `REMOVE_DIRS`); add to them to trim
more. Afterwards it rebuilds the caches that listed removed files (GTK's input methods,
GSettings schemas) and checks with `ldd` that every program and library still finds the
libraries it links; the build stops if one doesn't. Nothing in the system can bring anything back,
since there's no package manager.

- `REMOVE`: deleted with their contents. Entries may be shell patterns (gcc's folders
  carry its version); one that matches nothing is reported as `REMOVE: nothing matches`
  in the build output (the package dropped it, or a typo). So far:
  - What only OpenRC, busybox init or a login would read: `inittab`, `fstab` (it only
    listed a CD-ROM and USB stick under `/media`), `motd`, `securetty`, `modules`,
    `sysctl.conf`, and `busybox-paths.d` (a list for apk's busybox trigger).
  - Services that don't run: cron (`/etc/crontabs`, `/etc/periodic`, `/var/spool`),
    ifupdown and DHCP (`/etc/network`, `/etc/udhcpc`, `/usr/share/udhcpc`; the Network
    applet uses `ip` directly), logrotate (`/etc/logrotate.d`, for acpid, which isn't
    installed), sudo's log server (`sudo_logsrvd.conf`), D-Bus's system bus
    (`system.conf` in `/etc/dbus-1` and `/usr/share/dbus-1`, `system.d`,
    `system-services`), desktop-session autostart and Xwayland (`/etc/xdg`).
  - Files nothing reads here: `nsswitch.conf` (musl doesn't use it), `sudo.conf` (all
    comments), `/etc/pam.d` and `/etc/security` (sudo is built without PAM; only vlock's
    file was there), `pkcs11.conf.example`, `vdpau_wrapper.cfg` (Flash Player settings),
    `profile.d/README` and the disabled `color_prompt.sh.disabled`, and
    `/var/log/apk.log`.
  - Data only something else would read: udev's hardware database sources
    (`/usr/lib/udev/hwdb.d`, 7.6 MB; the build compiles them into `/etc/udev/hwdb.bin`
    first, which is all udevd reads; checked with `udevadm hwdb --test` in a chroot),
    kbd's keyboard layouts, character maps and
    console fonts (`keymaps`, `consoletrans`, `unimaps`, `consolefonts`; nothing runs
    `loadkeys`, and the US layout is the kernel's default), X11's font encodings and the
    X font tools `mkfontscale`/`mkfontdir` (only for X core fonts), AppStream metadata
    (`metainfo`, for software centers), `paxrelabel.d` (grsecurity), and
    `pipewire-launcher` (Alpine's script for starting PipeWire from OpenRC user services;
    simplewm starts it).
  - Programs nobody here would run: PulseAudio's X11 helpers (`start-pulseaudio-x11`,
    `pax11publish`), GTK's HTML5 display server `broadwayd`, sudo's log-server tools
    (`sudo_logsrvd`, `sudo_sendlog`, `sudoreplay`), `libcamera-bug-report`. Kept on
    purpose although a first pass listed them: `pkgconf`/`pkg-config` (Rust crates that
    link C libraries need it), the `spirv-*`, `gi-*` and gdk-pixbuf developer tools,
    GObject typelibs (`/usr/lib/girepository-1.0`, for Python or JavaScript GTK
    programs), `/usr/share/pkgconfig` and `/usr/share/aclocal`.
  - The accessibility bus (at-spi2-core's service side): the D-Bus service file
    `org.a11y.Bus.service`, which made the session bus start `at-spi-bus-launcher` (and
    with it a second dbus-daemon) whenever a GTK app such as Firefox started, the
    launcher and `at-spi2-registryd`, `accessibility-services`, their settings
    (`/usr/share/defaults/at-spi2`) and GNOME's `gtk-modules` file. Nothing else is
    D-Bus-activated: `/usr/share/dbus-1/services` is in `REMOVE_DIRS`, so a package that
    adds an auto-started service stops the build. The libraries (`libatspi`,
    `libatk-bridge`) stay: GTK links them. Apps find no accessibility bus and carry on.
  - `font-terminus` isn't installed at all: the build unpacks the one console font
    simplewm-init loads (`ter-132n.psf.gz`) from its package. The rest
    was 238 more console fonts and Terminus fonts that apps would have listed.
  - (Second pass, counting files: about 14,050 before.) Things only another setup would
    read: `/etc/modprobe.d` (no modules), `/etc/pulse/client.conf` (all comments),
    `/usr/lib/sysctl.d` (OpenRC's), GTK's multipress input method config and GTK 2's
    accessibility module, the CUPS and lpr print backends (Print to File stays),
    valgrind's `gtk.supp`, gcc's Fortran coarray library (no gfortran), gettext's shared-mime-info rules, the GTK example app's
    schema, libcamera's example config and the broken `libcamerify` (its library isn't
    shipped), the bluez config (no Bluetooth), Python's `turtle` and `turtledemo` (need
    tkinter, which isn't installed), X11-only data (`Xcms.txt`, `XErrorDB`, xkb
    `geometry`, which xkbcommon ignores), OpenSSL's engines (deprecated; nothing loads
    them), the vdpau tracer, cairo's trace recorder, bash's loadable builtins, and the
    udev rules for tape drives, NAND flash and IBM mainframe network cards.
  - Certificates: only `/etc/ssl/certs/ca-certificates.crt` stays (with `cert.pem`
    linking to it), which is what curl, git, Go, Python and OpenSSL read. The ~150
    single certificates, their hash links, `/usr/share/ca-certificates`,
    `update-ca-certificates`, `c_rehash` and `/etc/ssl1.1` went (the bundle is made at
    build time; nothing here rebuilds it). Firefox has its own store.
  - gcc's plugin headers and GDB's compile plugins (`plugin/`, ~570 files, for writing
    GCC plugins), its `install-tools` (fixincludes), and `lto-dump` (an LTO debugging
    tool). gcc, g++, LTO and the sanitizers all stay.
  - Terminal descriptions for terminals that aren't installed (GNOME, Konsole, PuTTY,
    rxvt, st, Sun, Terminator, Terminology, VTE, vt52, vt200); alacritty, linux, xterm,
    screen, tmux, vt100/102/220, ansi and dumb stay.
  - kbd's tools except `setfont` (simplewm-init's): one VT, no keymaps, so `chvt`,
    `openvt`, `loadkeys`, `showkey`, `setleds` and the rest have nothing to do.
  - Sound leftovers: `pacmd`, `pa-info` (both need a real PulseAudio daemon), `padsp`
    with its library and `libpulsecore` (nothing links it), PipeWire's AES67, AVB,
    Vulkan and minimal daemon setups (`pipewire-aes67` etc. and their configs), alsa's
    `aserver`, libsndfile's `sndfile-*` tools.
  - Other tools nobody here would run: `bomtool` (pkgconf's SBOM tool), `econftool`,
    `cvtsudoers`, `dbus-test-tool`, `dbus-cleanup-sockets`, `scmp_sys_resolver`,
    `yuvconvert`, `fribidi`, `git-shell` (a login shell for git servers), procps's
    `hugetop` and `tload`, and iproute2's rarely used extras (`dpll`, `netshaper`,
    `genl`, `ifstat`, `lnstat`/`ctstat`/`rtstat`, `rtacct`, `rtmon`, `routel`; `ip`,
    `ss`, `tc`, `bridge` and `nstat` stay).
- `BUSYBOX`: busybox command links for what this system doesn't do: kernel modules
  (`modprobe`, `insmod`…), other inits and logins (`getty`, `login`, `switch_root`,
  `mdev`, `run-parts`), cron, syslog, mail, DHCP, ifupdown, NAND flash, VT tools
  (`vlock`, `loadkmap`), `fsck` (no checkers installed), and busybox's `ipaddr`/`iplink`…
  shortcuts (iproute2's `ip` is there). The build stops if one isn't a busybox link.
  `poweroff`, `reboot`, `halt` stay (they're how to shut down).
- `KEEP_ONLY`: a folder and the only names in it that stay (the build stops if one is
  missing). So far:
  - `/usr/share/mime`: only the files programs read (`mime.cache`, `globs2`, `magic`…)
    and `packages/` (the source). The ~900 per-type XML files, which only give
    descriptions like "PDF document", went (Firefox uses simplewm's File Picker).
  - xkb: only what the US layout compiles from (rules `evdev`, keycodes `evdev` and
    `aliases`, symbols `pc us inet srvr_ctrl keypad`, and the types and compat files it
    includes), 293 files down to 28. Found by compiling with `xkbcli compile-keymap`
    against a trimmed copy until the keymap was byte for byte the same as with the full
    set. Apps don't read xkb files: they get the keymap from simplewm.
  - `/usr/share/X11/locale`: `compose.dir`, `locale.alias`, `locale.dir` and
    `en_US.UTF-8` (the Compose table xkbcommon uses; X11's other locales only matter
    to X).
  - GTK input methods: only `im-wayland.so` (the others were X, Broadway and
    single-language ones); the build regenerates `immodules.cache`.
  - Mesa's `/usr/lib/dri`: drivers for PC GPUs (AMD, Intel, NVIDIA, virtio, VMware,
    DisplayLink) and software rendering; the ~35 names for ARM display controllers
    went.
  - ALSA card configs: `aliases.conf`, `HDA-Intel.conf` (the user's sound and HDMI,
    and QEMU's) and `USB-Audio.conf`; the rest were for old PCI sound cards.
- `REMOVE_DIRS`: folders that should be empty (or hold only empty folders). If a package
  ever puts files in one, the build stops and lists them instead of deleting them. So
  far: `/media`, `/mnt`, `/opt`, `/srv`, `/usr/local`, `/etc/opt`, `/etc/alsa`,
  `/etc/udev/rules.d`, the `modules-load.d` and `sysctl.d` folders, and `/var/mail`,
  `/var/opt`, `/var/local`, `/var/db`, `/var/empty`, `/var/lib/misc`, `/var/lib/dbus`,
  `/var/cache/misc`, `/var/lib/sudo/lectured`,
  `/usr/share/dbus-1/session.d`, `/usr/share/dbus-1/services`, `/usr/lib/gio/modules`,
  `/lib/firmware` (firmware is built into the kernel). (`make install` and the like
  make `/usr/local` again when they need it.) `/usr/share/misc` was on the list until the
  development tools brought libmagic's `magic.mgc` into it; the safety stop caught that.

How to look for more, from the host: file counts per folder (`find <dir> -not -type d |
wc -l`), empty folders, and links that point nowhere, checked from inside the root
(`chroot distro/root /bin/busybox find / -xdev -type l ! -exec test -e {} \; -print`;
busybox's links are absolute, so a check from outside sees them all as dangling).

After trimming, `/etc` holds: bash, dbus-1 (session.conf), fonts, group, hostname,
hosts, inputrc, machine-id, mtab, passwd, profile, profile.d, protocols, services, shadow,
shells, ssl, sudoers, sudoers.d, terminfo, udev. Kept on purpose: `/var/tmp`, `/var/run`
and `/var/lock` (programs expect them). sudo's first-use lecture is turned off (`Defaults lecture=never`). The only setuid program left is
`sudo` (the Network applet needs it).

Checked after trimming, with a test image that writes a report: all services run, the
sound card is found, simplewm's portal is on the session bus, Firefox starts, sudo works.

Accounts are trimmed the same way (`KEEP_ACCOUNTS` in `build.sh`): `/etc/passwd` and
`/etc/shadow` hold only `root` and the user. Alpine's base layout adds accounts for
services that don't run here (bin, daemon, lp, mail, news, uucp, cron, ftp, sshd, games,
ntp, guest, nobody, …), and dbus adds `messagebus` for the system bus, which doesn't run
(simplewm runs the session bus); its setuid helper `dbus-daemon-launch-helper` is removed
with it. The tools' backup copies (`passwd-`, `shadow-`, `group-`) go too. `/etc/group` is
untouched so far: eudev's rules hand devices to groups like tty, disk and kvm. Root's
password is locked (`*`): Alpine ships it empty. busybox `su` isn't setuid, so it was never
usable, but nothing should be able to log in as root without a password; the user gets
root through sudo with their own password (checked).

## Testing a boot

`distro/run.sh` (or `make vm`) boots `out/` in QEMU, straight into simplewm. Claude can
test a boot itself with `distro/run.sh headless [image]` (no window, and changes to the
disk are thrown away). There's no shell on the serial port (it's output only), so drive
the VM through QEMU's monitor (pipe Ctrl+A C, a command, Ctrl+A C into run.sh):
`screendump distro/out/x.ppm` for screenshots (convert PPM to PNG to view), `sendkey` to
type (one key per command: `spc`, `dot`, `shift-4` for `$`…), and `quit` at the end.

**`distro/test-boot.sh [name]`** does a whole check in one go (about 2.5 minutes): boots a
throwaway copy of `out/root.img` with `out/vmlinuz`, whose `~/.bash_profile`
(`test/bash_profile`) runs `test/check.sh` next to simplewm, then leaves in `out/test/`
`<name>-check.log` (services running, `/dev/dri`, `/dev/input`, `/dev/snd`, `wpctl status`,
network cards, mounts, a user namespace, bubblewrap, a headless Firefox screenshot, kernel
warnings and the whole dmesg; it ends with `== END`), `<name>-serial.log` and
`<name>.png` (the screen). Use it for every kernel change; extend `check.sh` rather than
writing one-off scripts. It's how the methods below are put together.

- **Changing a test image:** boot a copy of `out/root.img` changed with `debugfs -w` in
  the container (e2fsprogs-extra), e.g. with a stub `/usr/bin/simplewm` to test failures.
  Run `e2fsck -fy` on the copy first: if QEMU was quit while the image was mounted, its
  journal still holds unwritten changes, which the kernel writes at boot over whatever
  debugfs changed (corrupting it). Make all the changes in one `debugfs -w -f <commands>`
  run. When simplewm exits, simplewm-init gives a user shell on tty1 to type into.
- **Checking what a screenshot can't show:** give the test copy a
  `/home/user/.bash_profile` (simplewm is started through the login shell) that runs
  checks in the background and writes a report ending with `sync` (new data otherwise
  waits up to 30 s to reach the disk). Test copies are written to, so read the report
  afterwards with `debugfs -c -R "cat /home/user/check.log"` (`-c`: QEMU's `quit` leaves
  the filesystem unclean).
- Copy test images on the host with `cp --sparse=always` (the image is a 16 GB sparse
  file; busybox's `cp` in the container can't keep it sparse). `build.sh` empties `out/`,
  so keep test images elsewhere or remake them.
- A kernel change can be tested against the existing `root.img`: `build-kernel.sh` alone
  rebuilds just `out/vmlinuz`. After editing `kernel.config`, run `build-kernel.sh refresh` and
  read its diff: a line that vanishes was ignored by the kernel.

## The kernel

Our own, built from kernel.org's source by `distro/build-kernel.sh` (in the container; source
and build tree kept in `cache/kernel/`, so a rebuild only redoes what changed: about 13
minutes from scratch on the user's 4 cores, seconds when nothing changed). The version is
pinned in `build-kernel.sh` (`VERSION`, with the tarball's `SHA256` from kernel.org's
`sha256sums.asc`): 7.3.4, the newest stable kernel, picked by the user on 2026-11-13
over staying on the 6.18 long-term kernel (the first pick, 6.18.63, the same one Alpine's
`linux-virt` was). A stable series stops getting fixes a few months after the next one
comes out, so move up with each release: 7.4 when it's final (it was at rc2), not its
release candidates. To move: new `VERSION` and `SHA256` (from
`https://cdn.kernel.org/pub/linux/kernel/v7.x/sha256sums.asc`), `build-kernel.sh refresh`,
read the diff, boot-test. The move from 6.18 changed one choice: x86 no longer offers
`PREEMPT_VOLUNTARY` (the defconfig's), so it's lazy preemption (`PREEMPT_LAZY`, the new
default) with `PREEMPT_DYNAMIC`.

**No modules, no initramfs.** Every driver the system needs is built in
(`CONFIG_MODULES` is off), so the kernel finds the disk, mounts `root=` itself and runs
`/sbin/init`, and devtmpfs is mounted on `/dev` before init starts. Before this, Alpine's
`linux-virt` had the disk and ext4 drivers as modules, and our own initramfs (static
busybox) loaded them, mounted the disk and switched to it. Nothing in the system loads
modules now: no `/lib/modules` and no kmod tools.

**`distro/kernel.config`** is the config in the form `make savedefconfig` writes: only
the lines that differ from the kernel's defaults (about 240), so each line is a choice.
Change it with `distro/build-kernel.sh menuconfig` (the kernel's menus; saved back on exit) or
by hand. A build first checks that the file comes back unchanged from the kernel's config
tools, and stops with a diff if not: that catches options that don't exist or whose
dependencies are off, which the kernel would otherwise drop without a word. `build-kernel.sh
refresh` rewrites the file in its proper form (after hand edits, or after raising
`VERSION`) and shows what moved or dropped out.

How it was made: the kernel's `x86_64_defconfig`, plus `kvm_guest.config`, plus our
changes, then `savedefconfig`. Our changes:

- Built in or turned on: the user's AMD GPU (`DRM_AMDGPU`) with its firmware (below),
  NVMe (for other machines; AHCI SATA, USB storage, Intel
  i915, Intel HDA sound, and e1000/e1000e/r8169 network cards are already in the
  defaults), virtio sound, HDA's generic, HDMI and Realtek codecs, USB HID, the DRM
  framebuffer console and simpledrm (the console before the GPU driver takes over),
  `DEVTMPFS_MOUNT`, tmpfs and ext4 ACLs and xattrs, user/PID/net namespaces and seccomp
  (Firefox's sandbox and bubblewrap), `PREEMPT_DYNAMIC`, and `/proc/config.gz`
  (`IKCONFIG_PROC`), so the running kernel's config can always be read.
- Off: modules, initramfs support, SELinux, audit, wireless (cfg80211/mac80211/rfkill
  and Wi-Fi drivers; the machine is wired), kernel IP autoconfiguration, 9p and network
  filesystems, the floppy driver, and the defconfig's debug options (CGROUP_DEBUG,
  PM_DEBUG, DEBUG_DEVRES, DEBUG_STACK_USAGE, DEBUG_BOOT_PARAMS, DEBUG_ENTRY).
- Release name `<VERSION>-simplewm` (`uname -r`), e.g. `7.3.4-simplewm`.

Checked in QEMU: before the switch, the old kernel loaded these modules, all now built
in: virtio_blk, virtio_net, virtio_gpu, virtio_input, ext4, snd_hda_intel and its codec,
evdev, mousedev, psmouse, i2c_piix4 (plus floppy, sr_mod/cdrom, kvm and qemu_fw_cfg, not
needed). With the new kernel simplewm, seatd, udevd, the session bus, PipeWire and
WirePlumber all come up, with `/dev/dri`, `/dev/input` and `/dev/snd` in place.

**Firmware** is compiled into the kernel (`CONFIG_EXTRA_FIRMWARE`, a list of files in
`kernel.config`): a built-in driver starts before the disk is mounted, so it can't load
firmware from it, and without its firmware amdgpu doesn't start. `build-kernel.sh` takes each
listed file from Alpine's `linux-firmware-<folder>` package (`amdgpu/…` from
`linux-firmware-amdgpu`), decompresses it (Alpine ships them zstd-compressed, some as
links to others) into the container's `/lib/firmware`, where the kernel build looks. For
the RX 480/580 that's the 21 `amdgpu/polaris10_*` files, the ones amdgpu names with
`MODULE_FIRMWARE` for Polaris 10 plus its video decode and encode (UVD, VCE). Nothing is
installed in the system's `/lib/firmware`.

Still to do:

- Trimmed on 2026-11-13, in three rounds, each checked with `test-boot.sh` (18.2 to 14.6
  MB): debugging and tracing, kexec and crash dumps, hibernation, 32-bit programs
  (`IA32_EMULATION`), NUMA, cgroups and task accounting (nothing mounts cgroups),
  PCMCIA, PCI hotplug, Mac/Eee PC bits, advanced routing, netfilter, traffic control,
  IPsec, sit, netconsole, network cards other than virtio/e1000/e1000e/r8169, CD-ROM,
  IDE/PATA, `ATA_SFF`, RAID/LVM, quota, autofs, ISO 9660, joystick/tablet/touchscreen,
  odd HID drivers (`HID_SONY` kept for game controllers), USB 1/2 host controllers
  (xHCI handles USB 1/2 devices; the Skylake chipset has only xHCI), the MIDI sequencer,
  serial extras, AGP, watchdog, AMD IOMMU. Kernel crypto went with them (nothing uses it).
  Watch for options another one was `select`ing: `AMD_IOMMU` was what turned on
  `PCI_MSI`, so it's now set explicitly (check.sh lists interrupts). To review a round,
  compare the `=y` lines of the full `.config` before and after.
- What's left is mostly amdgpu (~10 MB), the built-in firmware (~2.4 MB) and core
  kernel. Ideas not done yet: zstd compression (`KERNEL_ZSTD`, the default is gzip),
  dropping `CONFIG_SECURITY` (no LSM is actually built in), and i915 (below, 2.7 MB).
- Intel CPU microcode (`intel-ucode`, also loadable as built-in firmware) isn't
  included; the motherboard firmware's copy is what runs.
- The Skylake CPU's own GPU is disabled on the user's machine, so `DRM_I915` could go.

## On the user's machine

The hardware (`lspci -k` on the host): Intel Core i5-6500 (Skylake), AMD Radeon RX
480/580 (Polaris 10, "Ellesmere"; amdgpu) with the monitor on its HDMI, sound from the
chipset's Intel HDA (Realtek ALC662 codec) and HDMI audio on the AMD card (both
snd_hda_intel), an Intel I219-V network card (e1000e), and two SATA SSDs on the
chipset's AHCI controller: `sda` (1 TB) is the CachyOS host with its systemd-boot,
`sdb` (256 GB Samsung 850 PRO) is the distro's. USB is the chipset's xHCI. All of these
drivers are built into our kernel, and must stay built in when it's trimmed.

**The disk** (`install.sh`, run as root on the host): GPT with three partitions, which the
scripts find by name, never by `sdX` (the disks' letters can swap between boots):

- `distro-esp`, 512 MiB, FAT32 (label DISTRO): the EFI system partition. The kernel is
  `\EFI\distro\vmlinuz.efi`.
- `distro-root`, 32 GiB (`ROOT_SIZE`): `out/root.img` written over it with `dd`, then
  checked and grown to fill the partition (`resize2fs`).
- `distro-home`, the rest (ext4, label home, GPT type Linux /home): the home folders.
  simplewm-init mounts it on `/home` (`nosuid,nodev`), finding it by its GPT name in
  `/sys/class/block/*/uevent` (`PARTNAME`), and unmounts it at shutdown before
  remounting `/` read-only. Without one (the VM) `/home` is the root filesystem's; if
  it won't mount, simplewm-init says so on the console and carries on with the root's.

So the user can change the system here and install it again without losing their home:
the user asked for this. The first run erases the disk, after showing what's on it and
asking for its name to be typed. Later runs replace the root filesystem and kernel
without asking and never touch `distro-home`. The home partition is written only when
it's made, with the build's `/home` (the user's Rust and simplewm's default settings).
So what the build puts in the home later (a new `RUST_VERSION`, a changed `.profile`)
doesn't reach an installed home: the user runs `rustup` there themselves, or Claude
writes them the steps. The user's account must keep UID 1000 (it does: simplewm-init
looks for that UID) so the files on the home partition stay theirs. A disk from before
the home partition (esp, and root filling the rest; the user's sdb as of 2026-11-13)
gets one on the next run, after the same typed confirmation: root's partition shrinks
in place to 32 GiB, keeping its PARTUUID (so the boot entry still works; checked with
sfdisk on a copy of the layout), and the home partition takes the rest. The user had
nothing to keep in the old home. It refuses a disk with anything mounted or that holds
the host's root filesystem.

Tested in QEMU with a second disk holding a `distro-home` partition: mounted on `/home`,
the user's files came from it, files written in the session went to it (not to the
root filesystem), and it was clean after `poweroff`.

**Booting** (`efibootmgr.sh`): no bootloader. The kernel is an EFI program (`EFI_STUB`),
so an EFI boot entry called Distro starts it directly with the command line
`root=PARTUUID=<distro-root's ID> rw rootwait`. The entry goes last in the boot order, so
the machine still starts CachyOS; the user picks Distro from the firmware's boot menu
(they asked for no one-time-boot option). Running it again
replaces the entry; it only needs to after the disk is partitioned again (the entry and
`root=` name the partitions by their IDs). No `console=` for the serial port: messages go
to the screen (simpledrm on the firmware's framebuffer until amdgpu takes over).

Claude can't run either script (sudo needs the user's password). The user runs them,
e.g. `! sudo distro/install.sh /dev/sdb` in Claude Code, or in a terminal.

## Open questions

- Whether simplewm-init should supervise seatd and udevd (restart them if they die).
  What it does when simplewm exits and how it shuts down are settled (see "In the repo").
- What `systemd --user` still does on this host that the distro won't have, if anything.
- How much of that simplewm could take over itself, and what has to stay a separate
  root process (simplewm runs unprivileged, so loading drivers and changing `/dev`
  permissions can't move into it).
- Trimming the kernel to the machine (see "The kernel").
