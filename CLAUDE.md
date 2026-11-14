# simplewm

A small stacking Wayland compositor in Rust on Smithay. It draws all of its own UI: a top bar,
window title bars, and a Windows-style Control Panel of settings applets. It's the desktop of
the user's own distro (`distro/`, see `distro/CLAUDE.md`), where the goal is for simplewm to do
as much as it can in one process. The user welcomes experiments and new features, within the
rules below.

## Working with the user

- They never read the code. Report changes in terms of what they'll see and do.
- Claude can't see the screen. After a UI change, say exactly what to try. `make run` runs the
  release build (left in `target/release`), nested in the running simplewm or alone on a TTY.
  `make debug` writes a debug build's log to `simplewm-debug.log`. The installed system copy
  changes only through the distro's build → install → reboot cycle.
- The user's machine has one sound output in use (HDMI), one wired network card and one
  monitor.
- Keep this file and `distro/CLAUDE.md` current and short. Record what *is*, not history
  (that's what git is for). Fix them whenever they're wrong, without asking.
- Claude owns git: work on `main`, commit each change once it's checked, and push to
  `origin main` when it makes sense, without asking.

## Rules simplewm keeps

- No keyboard shortcuts, including VT switching. Escape cancelling a screenshot is part of
  that tool, not a shortcut.
- No config file, flags or environment variables (the log filter is fixed in
  `main::init_logging`). Every setting lives in a Control Panel applet and is saved in
  `~/.config/simplewm/simplewm.dat`, a small binary file. The backend picks itself: TTY when
  none of `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` or `DISPLAY` is set, nested otherwise.
- Wayland only (no Xwayland). One monitor: the first connected one, at its preferred mode,
  following changes to it. US keyboard layout. One bundled font (DejaVu Sans).
- Windows always float. simplewm always draws the title bars: server-side decorations are
  forced through xdg-decoration and KDE's server-decoration protocol (the only one GTK 3
  asks). Size moves and resizes a window once (halves, quarters, full screen below the
  bar). It doesn't snap.
- Runs with no special privileges. Anything root goes through the Password window: `sudo -k
  -S` on a worker thread, password on stdin and wiped afterwards, values passed only as
  separate checked arguments, never shell text. polkit is masked, so don't use it. Never
  test the sudo path with a wrong password: failures count toward faillock.
- **Portal (TTY only):** simplewm is the desktop portal itself. It owns
  `org.freedesktop.portal.Desktop` and implements only FileChooser (OpenFile and SaveFile,
  v4), answered with its File Picker, returning plain paths (Flatpak isn't supported). It
  queues for the name and sends SIGTERM to a leftover `xdg-desktop-portal`. It starts once
  the bus is up and restarts when the bus comes back (`State::ensure_portal`). Nested, it
  leaves the name alone.
- **User services** (`services.rs`), in place of `systemd --user`: the session bus,
  pipewire, pipewire-pulse and wireplumber, started on a TTY in that order, each once the
  one before answers on its socket. The session bus is simplewm itself (`bus.rs`); the
  rest are programs it runs. One that's already running is left alone and shown as "Run
  by PID n (program)". Each keeps its last 2000 output lines as a log. Nothing restarts
  them: the user presses Start. They're stopped when simplewm quits, and get SIGTERM if it dies
  (`PR_SET_PDEATHSIG`). No unit files, no dependency engine, no other services. Nothing
  uses `systemctl`, and nothing is exported to D-Bus activation.
- **Network applet:** a static IPv4 address, gateway and DNS server (no DHCP), set only
  when the user presses Apply, through one fixed `ip` script run by sudo (`network::apply`,
  `panel/sudo.rs`). With a DNS server set, the script replaces `/etc/resolv.conf` with a
  "simplewm owns this file" comment and one `nameserver` line. With the DNS field empty,
  the file is left alone. Nothing is applied at startup; the applet says when the card
  doesn't match the saved settings.
- **Sound applet:** uses `pw-dump` and `wpctl`, and WirePlumber remembers the choices.
  With one output it just names it; with several it lists them to choose from.
- **simplewm's own bus** (`bus.rs`, `bus/rule.rs`): zbus's server side does each
  connection's handshake; `bus.rs` does names, queues, match rules, routing and
  `org.freedesktop.DBus`. Only the user's own uid may connect. No activation
  (`StartServiceByName` only answers for names already owned), no policy, no monitors
  (`dbus-monitor` falls back to seeing signals only). It runs on its own threads, and a
  panic there stops the bus, not simplewm.

## Checking a change

- `cargo build`, `cargo clippy --all-targets` (zero warnings), `cargo test`, and
  `cargo fmt` whenever you like.
- Tests cover the logic that doesn't need a screen. `#[ignore]`d tests need a setup, which
  each one's doc comment explains:
  - After touching `bus.rs`, `portal.rs`, `portal_check.rs` or zbus:
    `cargo test bus -- --ignored`. It runs them on simplewm's own bus on a scratch
    socket (`bus::scratch_session`), and talks to it with libdbus (dlopened) and
    `gdbus`. The other bus tests run with plain `cargo test`.
  - After touching `services.rs`: `cargo test runs_the_session_service -- --ignored`.
  - `network::namespace_test` runs the root `ip` script in a throwaway namespace.
- Don't start PipeWire in tests: it would fight the running one for HDMI.
- `make` builds the release with `--locked`. `make install` copies it to `/usr/bin`.

## Dependencies

- Add crates only when they're really needed. About 200 are already in the build.
- `.cargo/config.toml` sets `global-min-publish-age = "21 days"`. If a version is "too
  new", lower the requirement. The only exception is an urgent security fix:
  `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow cargo update <crate> --precise <v>`.
- Run `make audit` (`audit.py`, OSV/RustSec) after changing dependencies. Its `ACCEPTED`
  list says why each advisory there doesn't affect us.
- zbus 6 has no blocking API, and a connection only runs while some thread is inside
  `zbus::block_on`. The portal thread stays inside it. Never call `block_on` from inside a
  served D-Bus method.

## Runtime dependencies

Check what's linked with `readelf -d target/release/simplewm | grep NEEDED`. Update this
list when something changes.

- To start: libseat (seatd or logind), libudev, libinput, libgbm, `libEGL.so.1`,
  libxkbcommon with xkeyboard-config data (it panics without it), and the C library.
  Nested also needs the host's Wayland client libraries, which winit loads.
- By feature, starting without them: `pipewire`, `pipewire-pulse` and
  `wireplumber` on `PATH`; `pw-dump` and `wpctl` (Sound); a session bus (portal);
  `/usr/share/mime/globs2` (File Picker filters); `/usr/bin/sudo`, `/bin/sh`, `ip`,
  `mktemp`, `chmod`, `mv` and `rm` (Network Apply); and whatever the Launcher lists (on
  first run, alacritty, gram and firefox in `/usr/bin`).

## Code map (`src/`)

- `main.rs`: picks the backend and runs the calloop loop.
- `state.rs`: `State`, which holds the protocol states, the window `Space`, `stack` (the
  stacking order of windows and panels together), `Ui`, `Panels` and the backend.
- `handlers.rs`: Smithay protocol handlers, including the extras gamescope needs.
- `udev.rs` (TTY: DRM/KMS, libinput, libseat, following the preferred mode) and `winit.rs`
  (nested). Both rebuild the whole scene every frame and let the damage tracker find the
  changes.
- `input.rs`: `handle_press` routes clicks to the bar, a menu, a panel (`PanelHit`) or a
  client. Keys go, in order, to: Esc during a screenshot, the focused File Picker, the text
  field being edited, the focused client.
- `grabs.rs`: moving and resizing.
- `ui.rs`: the bar, menus, Size picker, decorations, cursor, toasts and `build_elements`
  (the whole scene, front-most first). The bar has a tab per window (its open number and
  `/proc/<pid>/comm`, a `WindowTag` in user data): hover outlines the window in red, left
  click raises it, right click centers it.
- `text.rs`: fontdue rasterizing. `clock.rs`: libc `strftime`.
- `services.rs`: the user services, plus `/proc` helpers and `bus_address`.
- `bus.rs`: simplewm as the session bus (threads "bus-accept" and "bus"; one future
  polls every connection, each with its own send queue). `bus/rule.rs`: match rules.
- Logic behind the applets, with no UI: `network.rs`, `sound.rs`, `screenshot.rs`
  (offscreen render, PNG written on a worker), `portal.rs` (its own thread, answers sent
  later as Response signals), `portal_check.rs`. Workers report back over calloop
  channels.
- `bin/simplewm-init/`: PID 1 of the distro, std + libc only (see `distro/CLAUDE.md`).

## Panels (`panel.rs`, `panel/`)

`Panels` owns every window simplewm draws itself: a `PanelWindow` per `PanelId`, plus each
applet's state. `panel.rs` has the shared parts (placement, frame, hit-test/render
dispatch, `PanelHit`, sizes and colors). Each window has its own file of
`impl super::Panels` blocks: `control.rs` (Control Panel and Help), `launcher.rs`,
`screenshot.rs`, `network.rs`, `sound.rs`, `rows.rs` (Environment, Desktop Portal and the
shared `RowList`), `services.rs` (the service row, the D-Bus applet with its list of
connected programs, the Log window), `picker.rs` (File Picker) and `sudo.rs` (Password window: `ask_password(SudoJob)` →
`job_finished`). Shared helpers: `text_field.rs` and `store.rs` (to change the settings
format, bump `VERSION` and keep reading the old one).

Applet files usually run in this order: layout constants, a `*Layout` struct, `*_size`,
`*_hit`, actions, `render_*`.

Drawing rules:

- Push elements front-most first: text before its background.
- Keep `SolidColorBuffer`s from frame to frame with `bufs(&mut self.<window>.solids, n)`,
  and comment each slot. The damage tracker tells elements apart by their buffer.
- Colors are premultiplied RGBA. Hovered controls use `*_HOT` colors (`button_color`).

A new applet needs all of these:

- `PanelId`, its `title`, `PanelId::ALL` (in enum order, which a test checks) and
  `APPLETS`.
- The `content_size`, `hit` and `render` matches.
- A glyph in `control::applet_glyph` (a test checks that it fits).
- New `PanelHit` variants with their actions in `input.rs`, and a refresh in
  `State::open_applet` if it shows live state.
- A file under `panel/`.
