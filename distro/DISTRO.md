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
- **Boot: a custom init that starts simplewm as you, with no login.** It sets up the bare
  minimum as root, then runs simplewm as a normal user. No logind, elogind, PAM or login
  prompt. Details below.
- **Packages: Alpine's binaries, installed by apk into a folder.** No compiling the
  system ourselves; Alpine's musl packages become the root filesystem. Tested in QEMU
  first. Details below.

## Devices: the jobs involved

When the machine boots, and whenever something is plugged in, the kernel announces each
device (a "uevent") and makes its file appear in `/dev` (via devtmpfs). Several separate
jobs then follow, and different tools split them up differently:

1. **Loading drivers.** Each device says which driver module it needs (its "modalias").
   Something has to run `modprobe` for those, at boot for what's already there
   ("coldplug") and later for what gets plugged in. This isn't needed if the custom kernel
   has the machine's drivers built in, which is realistic for one known machine.
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
5. Boot it: a kernel and a small initramfs that mounts the disk and runs our init as
   PID 1.

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

- `container.sh [command…]` runs a command (a shell by default) in an Alpine 3.24
  container with the project root mounted at `/work`. Root in the container is the host
  user, so files owned by root come out as the host user's; anything owned by another
  user needs `podman unshare rm -rf` to delete. It passes `--cgroup-manager=cgroupfs
  --events-backend=file`, because when simplewm runs the session bus on a TTY, systemd
  isn't on it and podman's default (asking systemd over D-Bus) fails.
- `build-simplewm.sh` builds simplewm for musl in the container into
  `target/alpine/release/`, with Rust from rustup (Alpine's is older than simplewm's
  `rust-version`) and `-C target-feature=-crt-static` so it links Alpine's C libraries
  dynamically. Rust and Cargo's downloads are kept in `cache/`.
- `build.sh` (run on the host; it re-runs itself in the container) runs
  `build-simplewm.sh`, then builds everything else from scratch: `root/` (from `PACKAGES`
  via `apk --root … --initdb`), and in `out/` the kernel,
  the initramfs and `root.img` (ext4). It adds simplewm, the init, udev's compiled hardware database
  (`udevadm hwdb --update`), a machine ID for
  dbus-daemon, and the user (`USER_NAME`, uid 1000, in seat, audio, video, input and
  wheel, with the development password `USER_PASSWORD` for sudo; wheel may use sudo).
  bash is the login shell for the user and root, and the development consoles run it;
  `/bin/sh` stays busybox, which Alpine's package scripts and the Network applet's script
  expect. procps-ng replaces busybox's `ps`, `top`, `pgrep` and friends (busybox's `ps`
  has no `ps auxf` tree).
  Each build starts from an empty root, so simplewm's settings in the user's home start
  over too. Downloaded packages are kept in `cache/`. `root/`, `out/` and `cache/` are
  git-ignored. busybox's install script warns it can't create device nodes; harmless.
- `run.sh` boots `out/` in QEMU with KVM: the serial console is the terminal (Ctrl+A
  then X quits; Ctrl+A then C switches to QEMU's monitor, where `screendump file.ppm` takes
  a screenshot). The screen is a plain virtio-vga card sized to fill what a window can use
  on the host's monitor (its preferred mode from `/sys/class/drm`, less simplewm's bar and
  a window's title bar and borders; 3832x2096 on the user's 4K monitor), shown pixel for
  pixel with no GTK menu bar (Ctrl+Alt+M shows it) and no scaling. QEMU's GTK window
  passes its own size to the guest as the preferred mode, so the screen really follows the
  window: it opens small, and simplewm switches mode when the window is resized or
  maximized (it follows preferred-mode changes since this turned up). In a GTK window
  (`qemu-ui-gtk` and `qemu-hw-display-virtio-vga` on Arch; the user installed
  `qemu-desktop`), or with `run.sh headless` no window, for testing from a script. No
  OpenGL passthrough: Alpine's Mesa is built without virgl (its gallium driver list for
  x86_64 has no `virgl`; Mesa says "virtio_gpu: driver missing"), so everything in the
  guest draws with llvmpipe whatever QEMU offers. (virtio-vga-gl with `egl-headless` also
  crashed QEMU on exit and couldn't take screenshots: "no surface".) 4 CPUs, because
  llvmpipe at that size is busy. A virtio network card on QEMU's user network (address 10.0.2.15/24, gateway 10.0.2.2, DNS 10.0.2.3), and an
  Intel HDA sound card played through the host's PipeWire if QEMU has that driver
  (`qemu-audio-pipewire`).
- `initramfs-init` is the initramfs's `/init` (static busybox): it loads the drivers
  listed in `INITRAMFS_MODULES` (virtio disk, ext4, in dependency order worked out by
  kmod at build time), mounts `root=` and switches to its `/sbin/init`.
- `init` is the root filesystem's `/sbin/init` (a bash script for now): it mounts
  `/proc`, `/sys`, `/dev/pts`, `/dev/shm`, `/run` and `/tmp`, starts udevd
  (`--daemon`), replays the devices already there with `udevadm trigger` (subsystems,
  then devices) so their drivers load, and `udevadm settle`s, starts seatd (`-g seat`), makes `/run/user/1000`, waits briefly for
  `/dev/dri/card0`, and runs simplewm through `su -l` with `XDG_RUNTIME_DIR` set,
  starting it again 2 s after it exits. Logs: `~user/simplewm.log`,
  `/var/log/seatd.log` (udevd's output goes nowhere for now). Development root shells with no password
  run on ttyS0 (the terminal) and tty2. busybox's `poweroff`, `reboot` and `halt` work
  (they signal PID 1).
- Device rules are eudev's own (`/usr/lib/udev/rules.d`): drivers load by modalias
  through its built-in kmod, `/dev/dri` goes to video (render nodes 0666), `/dev/snd` to
  audio and input devices to input, and sound cards get `SOUND_INITIALIZED`. kmod is also
  installed as `modprobe`: busybox's first hands the kernel the compressed module, which
  logs "Invalid ELF header magic" for every driver before busybox falls back.

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

**The kernel** is Alpine's `linux-virt` for now (smaller, meant for VMs; `KERNEL` in
`build.sh`), switched to `linux-lts` or our own for the real machine. It's unpacked from
its package rather than installed, because Alpine's kernel packages insist on an
initramfs generator (mkinitfs), whose trigger fails here and which we don't need. Disk and
filesystem drivers are modules in both flavors, hence our own initramfs.

## Open questions

- The init's details: what it does when simplewm exits or crashes (restart it, drop to a
  text console, or shut down), shutting down cleanly, and whether it supervises seatd and
  udevd.
- What `systemd --user` still does on this host that the distro won't have, if anything.
- How much of that simplewm could take over itself, and what has to stay a separate
  root process (simplewm runs unprivileged, so loading drivers and changing `/dev`
  permissions can't move into it).
- A custom kernel with the machine's drivers built in.
