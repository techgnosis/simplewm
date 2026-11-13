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
- **Devices: libudev-zero and mdevd.** libudev-zero is the libudev library that smithay,
  libinput and PipeWire use, with no daemon behind it. mdevd loads drivers, sets
  permissions in `/dev` and passes plug-in events on to libudev-zero. Why, below.
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
| Predictable network names (`enp3s0`) | Yes                          | No; the card stays `eth0`                       |
| Who uses it              | Void, Gentoo, Alpine (optional)          | KISS Linux, Alpine (optional), small embedded systems |

**What simplewm needs from it:** enumerating the GPU, the monitor's connector and the
input devices at startup, plus hotplug for input. Both options give that. PipeWire also
uses libudev to find the sound card, and works with both.

**Chosen: libudev-zero with mdevd.** It fits the goal better: two small, readable pieces, no
rules language, and one machine with known hardware means the missing hardware database
and network-name rules don't cost anything. eudev is the safer fallback if some program
turns out to rely on udev behavior that libudev-zero doesn't copy. A third, less likely
option is building systemd's udev on its own (Chimera Linux does this), which is the
most compatible but the heaviest.

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
2. Start the root services: mdevd (devices) and seatd (screen and input access).
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

Alpine is a musl distro and already packages everything chosen above: seatd, mdevd,
libudev-zero, dbus-daemon, PipeWire, pipewire-pulse, WirePlumber, Mesa, libinput,
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
   needs no root). Rootless podman maps the container's root to the host user's sub-IDs,
   so files owned by root inside the folder look odd from the host; building the image in
   there keeps ownership right.
5. Boot it: a kernel (Alpine's `linux-lts` package to start with, our own later, see Open
   questions) that runs our init as PID 1.

Things to keep in mind:

- **Ask for `libudev-zero` by name.** libinput, Mesa and PipeWire need a libudev, and
  Alpine fills that slot with eudev's library unless told otherwise.
- **No OpenRC.** Alpine's own boot system only comes in with `alpine-base` or `openrc`;
  leave them out and our init is the only thing that runs.
- **Busybox comes along anyway** and provides `/bin/sh`, `mktemp`, `chmod`, `mv` and `rm`,
  which the Network applet's Apply script needs; `ip` comes from iproute2.
- **Package install scripts** run when apk installs into the folder. They expect Alpine's
  usual layout; usually harmless, but the most likely source of surprises.
- **Licensing is fine**: it's all open source and meant to be reused.

**Testing: QEMU first.** QEMU boots the disk image with a virtual graphics card (virtio-gpu)
that simplewm drives through DRM like a real one, so the whole boot can be tried over and
over without touching the real disk. Only once it boots cleanly does it go on a USB stick or
spare partition.

**First step:** a script that builds the root folder and boots it in QEMU as far as a text
console, before simplewm is involved. Then add seatd, mdevd and simplewm.

The host needs `podman` and `qemu` installed for this.

## Open questions

- The init's details: what it does when simplewm exits or crashes (restart it, drop to a
  text console, or shut down), shutting down cleanly, and whether it supervises seatd and
  mdevd.
- What `systemd --user` still does on this host that the distro won't have, if anything.
- How much of that simplewm could take over itself, and what has to stay a separate
  root process (simplewm runs unprivileged, so loading drivers and changing `/dev`
  permissions can't move into it).
- A custom kernel with the machine's drivers built in.
