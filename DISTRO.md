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
- **D-Bus: dbus-broker, without its launcher.** Why and how, below.

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

**Chosen: dbus-broker, without its launcher.** The broker runs fine without systemd. A
launcher's job is small: create the bus socket, start the broker, and tell it who may
connect through a documented control channel (see `man dbus-broker`). The one thing we give
up is activation, where the bus starts a service when an app first calls it. simplewm
already relies on nothing being activated: it takes the portal name itself, and everything
else (PipeWire, WirePlumber) is started at boot anyway. Apps that try activation for
optional extras (GTK looking for the accessibility bus, for example) just carry on without
it.

The plan is for simplewm itself to be the launcher, in line with doing as much as possible
in one process: simplewm already speaks D-Bus through zbus. Until then, the small
`dbus-controllers` project can launch the broker without systemd. If dbus-broker gives
trouble, dbus-daemon is the drop-in fallback.

Still to check: that dbus-broker builds cleanly with musl.

## Open questions

- Init and service supervision (what starts seatd, mdevd, PipeWire and WirePlumber, then
  simplewm).
- Who creates `XDG_RUNTIME_DIR` (`/run/user/<uid>`, where the Wayland, D-Bus and PipeWire
  sockets live). logind does this on a systemd system; seatd doesn't.
- How much of that simplewm could take over itself, and what has to stay a separate
  root process (simplewm runs unprivileged, so loading drivers and changing `/dev`
  permissions can't move into it).
- What replaces `systemctl --user` in the portal clean-up and the Desktop Portal checks.
- A custom kernel with the machine's drivers built in.
