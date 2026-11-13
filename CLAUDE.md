# simplewm

A small stacking Wayland compositor in Rust on Smithay. It draws all of its own UI: a top
bar, window title bars and a Windows-style Control Panel of settings applets. simplewm is
still growing: the user is open to experiments and new features, within the rules below.

## Working with the user

- They never read the code. They judge simplewm by using it, so report changes in terms of
  what they'll see and do, not in terms of code.
- Claude can't see the screen. After a UI change, say exactly what to try. The user runs it
  nested (`make run`) or on a TTY (`./simplewm`, or `./debug.sh`, which runs a debug build that logs DRM
  and session details to `simplewm-debug.log`). Their machine has one sound output (HDMI), one wired
  network card and one monitor.
- Keep this file current when behavior or structure changes, and fix it whenever you find
  it wrong, without asking. It's for Claude; nobody else reads it.
- Claude owns git: commit when a change is done and checked, and push to `origin main`
  when it sees fit, without asking. Work directly on `main`; no branches or PRs needed.

## Rules simplewm keeps

- No keyboard shortcuts. The one exception is Ctrl+Alt+F1… for switching virtual terminals
  on a TTY.
- No config file, flags or environment variables (not even `RUST_LOG`: the log filter
  is fixed in `main::init_logging`, more verbose in debug builds). Every setting is changed in a Control
  Panel applet and saved in `~/.config/simplewm/simplewm.dat`, a small binary file
  so nobody is tempted to hand-edit it (SQLite was dropped as overkill). The backend is
  chosen automatically: TTY when there's no `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` or
  `DISPLAY`, nested otherwise.
- Wayland only (no Xwayland), one monitor (the first connected one, at its preferred mode),
  US keyboard layout, one bundled font (DejaVu Sans).
- Windows always float. Title bars are always drawn by simplewm (server-side decorations
  are forced). Size moves and resizes a window once (halves, quarters, full screen below
  the bar) and doesn't snap it there.
- On a TTY simplewm is the desktop portal itself: it owns `org.freedesktop.portal.Desktop`
  on the session bus (no `xdg-desktop-portal` process) and only implements FileChooser
  (OpenFile and SaveFile, version 4), answered with its own File Picker. Other portal
  interfaces are simply missing. Results are plain file paths, so Flatpak apps can't open
  them (that would need xdg-document-portal); the user doesn't care about Flatpak. If
  another process has the name, simplewm queues for it, and stops a leftover
  `xdg-desktop-portal` by sending its PID SIGTERM. Nothing in simplewm uses `systemctl`.
  simplewm exports nothing to systemd or D-Bus activation. The portal starts once the
  session bus is up and restarts when the bus comes back after a stop
  (`State::ensure_portal`). Nested, it doesn't take the name, so nested apps get the
  host's file dialogs.
- simplewm runs the user services it always needs, in place of `systemd --user`
  (`services.rs`): the session bus (`dbus-daemon`, not dbus-broker, whose launcher needs
  systemd), `pipewire`, `pipewire-pulse` and `wireplumber`. On a TTY it starts them in
  that order at startup, each once the one it needs answers on its socket. One that's
  already running (systemd's socket, a host desktop) is left alone and shown as "Run by
  PID n (program)". Their output is kept as a log (last 2000 lines each). Nothing restarts
  them when they exit; the user presses Start. They're stopped when simplewm quits, and
  get SIGTERM if it dies (`PR_SET_PDEATHSIG`). The Sound applet has Start/Stop and Log for
  the three sound services, the D-Bus applet for the bus. The user doesn't want a smaller
  systemd: no unit files, no dependency engine, no other services. To try it on their
  systemd host they mask the user units `pipewire.socket pipewire.service
  pipewire-pulse.socket pipewire-pulse.service wireplumber.service dbus.socket`.
- Long-term goal: boot simplewm in a minimal custom distro, doing as much as it can in one
  process. `distro/DISTRO.md` records the plan and decisions so far (musl, seatd, libudev-zero +
  mdevd, dbus-daemon started by simplewm, a custom init that runs simplewm as the user with no login, the system
  built from Alpine's binary packages installed by apk into a folder and tested in QEMU);
  keep it current as they're made. The PipeWire tools and the `ip` script are staying.
- simplewm runs with no special privileges. The Network applet sets a static IPv4 address,
  gateway and DNS server (no DHCP) only when the user presses Apply: the Password window
  asks for their password and runs one fixed `ip` script through `sudo -k -S` on a worker
  thread (`network::apply`, `panel/sudo.rs`). simplewm owns `/etc/resolv.conf`: when a DNS
  server is set, the script replaces the whole file (renamed into place, replacing any
  symlink) with a "simplewm owns this file" comment and one `nameserver` line
  (`network::resolv_conf`). An empty DNS field leaves the file alone. The values reach the
  script only as separate arguments after `network::parse` has checked them, never as
  shell text. The password is passed to sudo on stdin and wiped afterwards. Nothing is set
  at startup; the applet says when the card or resolv.conf lacks the saved settings. polkit
  is masked on the user's machine, so don't use it. NetworkManager and systemd-resolved
  must stay masked or they undo the settings.
- The Sound applet drives PipeWire through `pw-dump` and `wpctl`, and WirePlumber remembers
  the choices. With one output it just names it; with several it lists them to choose from.

## Checking a change

- `cargo build`, `cargo clippy --all-targets` (keep it at zero warnings), `cargo test`. Run
  `cargo fmt` whenever you like.
- Tests cover the logic that doesn't need a screen: the settings file, text fields, row
  lists, File Picker listing and search, screenshot PNGs, portal globs and checks, network
  address and DNS parsing, resolv.conf contents, PipeWire output parsing, and the service
  manager's process handling (exit descriptions, PID handling, output cleaning). A few only do
  something on a machine that has the hardware or service.
- The `#[ignore]`d tests need a setup; each one's doc comment says how to run it. Two
  exercise D-Bus on a scratch bus: `dbus-run-session -- cargo test bus -- --ignored` drives
  the portal the way apps do (including the hand-over of the bus name) and checks the
  Desktop Portal bus checks. Run it after touching `portal.rs`, `portal_check.rs` or zbus.
  `cargo test runs_the_session_daemon -- --ignored` starts and stops a real dbus-daemon on a
  scratch socket through `Services`; run it after touching `services.rs`. Don't test
  starting PipeWire for real: it would fight the user's running PipeWire for the HDMI
  output. `network::namespace_test`
  runs the root `ip` script on a dummy card in a throwaway namespace, writing a scratch
  resolv.conf instead of the real one (no sudo, nothing real touched). Never test the sudo
  path with a wrong password: failed attempts count toward the account's faillock lockout.
- `make` builds the release binary with `--locked`; `make install` puts it in `/usr/bin`
  and removes the files that made it an `xdg-desktop-portal` backend before.

## Dependencies

- Add crates only when they're really needed: about 200 crates already go into the build,
  mostly through smithay and zbus.
- `.cargo/config.toml` sets `global-min-publish-age = "21 days"` (Cargo 1.100+, which is
  also `rust-version`), so Cargo never picks a version younger than that. If the resolver
  says a version is "too new", lower the requirement instead of overriding the check. The
  only exception is an urgent security fix:
  `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow cargo update <crate> --precise <version>`.
- `make audit` (`audit.py`) checks `Cargo.lock` against the OSV/RustSec database. Its
  `ACCEPTED` list records advisories that don't affect us, and why. Run it after changing
  dependencies.
- zbus 6 has no blocking API, and a connection only runs while some thread is inside
  `zbus::block_on`. The portal thread stays in it forever, and the Desktop Portal checks
  wrap their D-Bus calls in it on their worker thread. Never call `block_on` from inside a
  served D-Bus method.

## Runtime dependencies

What simplewm itself links, loads, runs or reads, on a systemd host (udevd and logind
running, and a user session that provides `XDG_RUNTIME_DIR`). Check
linked libraries with `readelf -d target/release/simplewm | grep NEEDED`.

- To start (TTY): libseat (talks to logind; seatd would also work), libudev, libinput,
  libgbm and `libEGL.so.1` (loaded at startup) from Mesa or another GL stack,
  libxkbcommon plus xkeyboard-config data (startup panics without it), glibc. Nested also needs the host's Wayland or X11 client libraries, which winit loads.
  The font is compiled in and the cursor is drawn by simplewm.
- Per feature (simplewm starts without them):
  - Services simplewm runs (unless something else already does): `dbus-daemon`,
    `pipewire`, `pipewire-pulse`, `wireplumber`, found on `PATH`.
  - Sound applet: `pw-dump` (PipeWire) and `wpctl` (WirePlumber).
  - Portal: the D-Bus session bus (zbus is pure Rust, no libdbus).
  - File Picker filters by MIME type: `/usr/share/mime/globs2` (shared-mime-info).
  - Network applet Apply: `/usr/bin/sudo`, `/bin/sh`, `ip` (iproute2), and `mktemp`,
    `chmod`, `mv`, `rm`. Reading the card uses only `/sys`, `/proc` and netlink.
  - Launcher: whatever is listed (default `/usr/bin/alacritty`).

Update this list when a change adds or drops one.

## The distro (`distro/`)

Scripts for building the custom distro from Alpine packages; `distro/DISTRO.md` has the
plan and describes each script. `distro/build.sh` builds simplewm for musl
(`build-simplewm.sh`, into `target/alpine/`), the root filesystem, kernel, initramfs and
disk image (inside an Alpine container, `distro/container.sh`), and `distro/run.sh` boots
them in QEMU, which starts straight into simplewm. Claude can test a boot itself with
`distro/run.sh headless` (no window): pipe commands into the serial root shell, take
screenshots with Ctrl+A C then `screendump distro/out/x.ppm` (convert PPM to PNG to view),
and end with `poweroff`. The outputs (`distro/root/`, `out/`, `cache/`) are git-ignored.

## Code map (`src/`)

- `main.rs`: picks the backend and runs the calloop event loop.
- `state.rs`: `State` holds everything: the Smithay protocol states, the window `Space`,
  `stack` (the stacking order of client windows and panels together), `Ui`, `Panels` and
  the backend.
- `handlers.rs`: the Smithay protocol handlers, including the extra protocols gamescope
  needs.
- `udev.rs` (TTY: DRM/KMS, libinput, libseat) and `winit.rs` (nested). Both rebuild the
  whole scene every frame and let the damage tracker work out what changed, so state
  changes show up without asking for a redraw.
- `input.rs`: all input. `handle_press` sends a click to the bar, a menu, a panel
  (`PanelHit` → its action) or a client. Keys are filtered in this order: VT switch, Esc
  during a screenshot, the focused File Picker, the text field being edited, and otherwise
  the focused client.
- `grabs.rs`: moving and resizing windows and panels.
- `ui.rs`: the bar, menus, the Size picker, decorations, the cursor, toasts and
  `build_elements` (the whole scene, front-most first), plus drawing helpers (`text_in`,
  `text_centered`, `text_in_alpha`, `solid`).
- `text.rs`: font rasterizing with fontdue (`measure`, `fit`).
- `services.rs`: starts, stops and logs the user services (session bus, PipeWire, its
  Pulse server, WirePlumber); see the module docs for the threads and PID handling. Also
  `/proc` helpers (`find_process`, `describe_pid`) and `bus_address`.
- `clock.rs`: local time through libc's `strftime` (no date crate).
- Behind the applets, with no UI code:
  - `network.rs`: reads the card (sysfs, rtnetlink dumps) and `/etc/resolv.conf`, and sets
    both (sudo + `ip`, then the resolv.conf rewrite).
  - `sound.rs`: PipeWire.
  - `screenshot.rs`: offscreen re-render, then the PNG is written on a worker thread.
  - `portal.rs`: the desktop portal (FileChooser and its Request objects), on its own
    thread inside `zbus::block_on`. Calls return a handle at once; `serve` sends each
    answer later as the Response signal, and closes dialogs of apps that leave the bus.
  - `portal_check.rs`: the Desktop Portal applet's checks, on a worker thread.

  Worker threads report back over calloop channels.

## Panels (`panel.rs`, `panel/`)

`Panels` owns every window simplewm draws itself. Each `PanelId` gets a `PanelWindow`
(open flag, position, buffers), and `Panels` also holds each applet's state. `panel.rs`
has what they all share: placement, the frame, hit-test and render dispatch, `PanelHit`,
and the shared sizes and colors. Each window has its own file of `impl super::Panels`
blocks:

- `control.rs`: the Control Panel and Help.
- `launcher.rs`, `screenshot.rs`, `network.rs` and `sound.rs`: one applet each.
- `rows.rs`: Environment, Desktop Portal and the `RowList` they share with Network and Log
  (`set_following` keeps a log scrolled to its end).
- `services.rs`: the service row (status, Start/Stop, Log) the Sound and D-Bus applets
  share, the D-Bus applet, and the Log window (opened by a Log button, not in the Control
  Panel). Service events reach `Panels::service_event`.
- `picker.rs`: the File Picker.
- `sudo.rs`: the Password window. An action asks for it with `ask_password(SudoJob)`, and
  `State::after_panel_action` opens and focuses it. A worker runs the job, and the result
  comes back as `SudoDone` to `job_finished`.

Two shared files support them: `text_field.rs` (editable fields) and `store.rs` (the settings
file; bump `VERSION` and keep reading the old one to change the format). Applet files usually go in this order:
layout constants, a `*Layout` struct of rects, `*_size`, `*_hit`, actions, `render_*`.

Drawing rules:

- Push elements front-most first: text before the background behind it.
- Keep each panel's `SolidColorBuffer`s from frame to frame with
  `bufs(&mut self.<window>.solids, n)`, and comment what each slot holds. The damage
  tracker tells elements apart by their buffer.
- Colors are premultiplied RGBA. Hovered controls use the `*_HOT` colors.

Adding an applet means touching all of these:

- `PanelId` and its `title`, and `APPLETS`.
- A `PanelWindow` field in `Panels`, in `new`, `win` and `win_mut`.
- The `content_size`, `hit` and `render` matches.
- A glyph in `control::applet_glyph` (a test checks that it fits its tile).
- New `PanelHit` variants and their actions in `input.rs`, plus a refresh when the applet
  opens if it shows live state.
- A new file under `panel/`.
