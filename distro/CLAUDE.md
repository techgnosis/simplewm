# The distro (`distro/`)

A musl system built from Alpine's packages, with our own kernel and simplewm-init as PID 1,
booting straight into simplewm as the user. It's the user's main system: two system slots
on `sda` and a shared home on `sdb`. The scripts are commented; read the script before
changing it. This file covers what the scripts don't make obvious.

## What's in it

- **Packages:** Alpine (`ALPINE` in `container.sh`, now 3.24), installed with
  `apk --root … --initdb` from `PACKAGES` in `build.sh`. After that apk's own records are
  deleted, so the system has no package manager and only changes by being rebuilt. No
  OpenRC.
- **Init:** simplewm-init (`src/bin/simplewm-init/`). No logind, elogind, PAM or login
  prompt: anyone at the keyboard gets the desktop, and sudo still asks for the password.
- **Seat:** seatd (`-g seat`). **Devices:** eudev with its own rules; its hwdb is compiled
  at build time. Don't switch to libudev-zero: with it PipeWire finds no sound cards,
  because nothing sets `SOUND_INITIALIZED`. The network card is `eth0`
  (`eudev-netifnames` isn't installed).
- **Session bus:** dbus-daemon, started by simplewm. Not dbus-broker: its launcher needs
  systemd. There's no system bus and no D-Bus activation:
  `/usr/share/dbus-1/services` is in `REMOVE_DIRS`, so the build stops if a package adds
  a service there.
- **Apps:** Firefox (`firefox.js` turns off tabs in the title bar and sends file dialogs
  to simplewm's portal), alacritty, and Gram (built from source by `build-gram.sh`).
- **Development tools are in the image:** rustup with `RUST_VERSION` in the user's home,
  gcc/build-base, Go, Python, GNU grep/sed, and the `-dev` packages simplewm, the kernel
  and Gram need. The whole project builds natively here.
- `/bin/sh` is busybox; bash is the login shell. Time zone: one zone file
  (`TIME_ZONE`) unpacked to `/etc/localtime`. The hardware clock is UTC. Nothing syncs
  the clock.
- Accounts: only `root` (password locked) and the user (UID 1000, in seat, audio, video,
  input and wheel). `sudo` is the only setuid program.
- The ssh client only, no server. `copy-ssh.sh` copies keys onto the home partition once.
  Never put keys in `root.img`.

## simplewm-init

PID 1 (`/sbin/init` links to `/sbin/simplewm-init`). It refuses to run as any other PID.
In order, it:

1. Mounts the API filesystems, plus efivarfs when the machine booted through EFI, and the
   `distro-home` partition on `/home` (`home.rs`, found by its GPT name).
2. Starts udevd, runs trigger and settle, then loads the Terminus console font (only
   after udev, because before the GPU driver loads the console only takes narrow fonts).
3. Sets the hostname, brings up `lo` and starts seatd.
4. Makes `/run/user/1000` and waits briefly for `/dev/dri/card0`.
5. Runs simplewm as the UID 1000 account through `bash -l -c 'exec simplewm'`, with
   output appended to `~/simplewm.log`.

Children start with no blocked signals. That matters: init blocks the signals it reads
through a signalfd, and when children inherited the mask they ignored SIGTERM.

When simplewm exits, for any reason, init writes how it ended and the last 20 log lines
on tty1, then starts the user's login shell there. When that shell exits, init starts
simplewm again. It never restarts simplewm by itself.

Shutdown: busybox `poweroff`/`reboot`/`halt` signal PID 1. Init SIGKILLs the shell's
session, sends SIGTERM to everything else, waits 5 s, kills what's left, syncs, remounts
`/` read-only and powers off. Logs: `~/simplewm.log` and `/var/log/seatd.log`.

## Building

`build.sh` runs `build-simplewm.sh`, `build-gram.sh` and `build-kernel.sh`, fills `root/`
from scratch, trims it, and writes `out/root.img` (16 GB sparse ext4) and `out/vmlinuz`.
`root/`, `out/` and `cache/` are git-ignored, and each build empties `out/`.

- **Where it runs** (`where.sh`): natively in the distro, which is the normal way, or on
  any other host, where it re-runs itself in an Alpine podman container
  (`container.sh`). Natively, everything runs as the user except the root filesystem
  part, which re-runs as `sudo sh distro/build.sh root-part`. Claude can't type the
  password, so the user runs that part.
- **Packages are verified against keys in git** (`keys/`, Alpine's x86_64 keys). A new
  Alpine key stops the build with "UNTRUSTED signature": check it against aports before
  adding it. Natively apk is `apk.static`, pinned by version and SHA256 in `apk.sh`.
  Raising `ALPINE` needs a new pin.
- **Package versions aren't pinned:** a build gets whatever Alpine has that day.
- **Trimming:** the user wants as few files as possible (counted in files, not bytes),
  while it stays a normal developer userland. `build.sh` has four lists: `REMOVE`,
  `BUSYBOX`, `KEEP_ONLY` and `REMOVE_DIRS`, each with comments. Then it rebuilds the
  affected caches and checks every binary with `ldd`. The build stops on a broken link,
  a missing `KEEP_ONLY` name, or files appearing in a `REMOVE_DIRS` folder. To find more
  to trim, look at file counts per folder, empty folders and dangling links, checked
  inside a chroot, because busybox's links are absolute.
- Gotchas:
  - The distro's home is setgid, so `build.sh` clears the bit on `root/` and
    `cache/home/`.
  - `GIT_CEILING_DIRECTORIES` stops Gram's build from taking simplewm's git commit as
    its own.
  - Don't edit `build-gram.sh` while it runs: the shell reads the script as it goes.
  - Gram takes about 35 minutes from scratch. Nothing new takes seconds.
  - A chroot of `root/` needs `/proc` and `/dev/null` bound in from outside.

## The kernel

`build-kernel.sh` builds kernel.org's stable source (`VERSION` and `SHA256`, now 7.3.4)
with `kernel.config`, in `make savedefconfig` form, so every line is a choice. It has no
modules and no initramfs: every driver is built in, and the kernel mounts `root=` itself.
Firmware is built in too (`CONFIG_EXTRA_FIRMWARE`, taken from Alpine's `linux-firmware-*`
packages). Nothing goes in `/lib/firmware`.

- The build stops if the config doesn't come back unchanged, which catches options the
  kernel would drop silently. After hand edits or a `VERSION` bump, run
  `build-kernel.sh refresh` and read the diff. `build-kernel.sh menuconfig` edits the
  config.
- Follow each stable series: move to 7.4 once it's final, not its release candidates.
  The new SHA256 comes from `cdn.kernel.org/pub/linux/kernel/v7.x/sha256sums.asc`.
- These must stay built in for the user's machine: amdgpu with the Polaris 10 firmware,
  AHCI, xHCI, e1000e, snd_hda_intel with the HDMI and Realtek codecs, USB HID, simpledrm
  with the fbcon console, and user/PID/net namespaces with seccomp (Firefox's sandbox and
  bubblewrap). `PCI_MSI` is set explicitly. When trimming, watch for options another
  option was `select`ing.
- Still possible to trim: `KERNEL_ZSTD`, `CONFIG_SECURITY` (no LSM is built in), and
  `DRM_I915` (the iGPU is disabled).

## Testing a boot (QEMU, needs a host with QEMU; the image has none)

- `run.sh` (`make vm`) boots `out/` with KVM in a GTK window. The serial console is the
  terminal, output only. The guest's screen follows the window size. Mesa has no virgl,
  so the guest draws with llvmpipe/lavapipe. Network: user mode (10.0.2.15/24,
  gateway 10.0.2.2, DNS 10.0.2.3).
- `run.sh headless [image]` runs in snapshot mode, so changes are thrown away. Drive it
  through QEMU's monitor (Ctrl+A C): `screendump x.ppm`, `sendkey`, `quit`.
- **`test-boot.sh [name]`** is the standard check (about 2.5 minutes). It boots a copy
  whose `test/bash_profile` runs `test/check.sh`, and leaves a check log (services,
  devices, sound, mounts, namespaces, a headless Firefox screenshot, dmesg), a serial log
  and a screenshot in `out/test/`. Use it for every kernel change, and extend `check.sh`
  rather than writing one-off scripts.
- To change a test image, run `e2fsck -fy` on a copy first, then make all the changes in
  one `debugfs -w -f` run. Read reports with `debugfs -c`. Copy images with
  `cp --sparse=always`.
- Container mode and the QEMU tests haven't been run since the build moved to native
  (committed keys, `root_part`, the firmware path). Expect fixes the first time.

## On the user's machine

Hardware: i5-6500 (Skylake), RX 480/580 (amdgpu) with the monitor on HDMI, HDMI audio on
the AMD card plus Intel HDA (ALC662), an I219-V network card (e1000e), and two SATA SSDs.
There's no bootloader: firmware boot entries start our kernels directly.

- `sda` holds CachyOS's EFI partition (our kernels are `\EFI\simplewm\a.efi` and
  `b.efi`), CachyOS itself, and the `distro-a` and `distro-b` partitions (150 GiB each).
  CachyOS is still the firmware's default. The user picks "Distro A" or "Distro B" from
  the firmware's boot menu, and wants no bootloader and no one-time-boot option. CachyOS
  is also the only rescue system.
- `sdb` holds `distro-home` (ext4), shared by both slots. It has the project, `cache/`
  and `target/`.
- The cycle: on one slot, run `distro/build.sh`, then `sudo distro/install.sh` (which
  picks the slot that isn't running and refuses the running one), then reboot into the
  other slot.
- Partitions are found by GPT name and never by `sdX` (`disk.sh`). Each slot gets its own
  filesystem UUID and label at install.
- `install.sh` and `efibootmgr.sh` need root, so the user runs them (e.g.
  `! sudo distro/install.sh`). `efibootmgr.sh` has already been run, and partition IDs
  don't change, so it's never needed again.

## Open questions

- Should simplewm-init restart seatd and udevd if they die?
- Clock sync, and shutdown/reboot from simplewm's UI (through the existing sudo path).
