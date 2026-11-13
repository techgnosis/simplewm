# simplewm

A small stacking Wayland compositor in Rust on Smithay. It draws all of its own UI: a top
bar, window title bars and a Windows-style Control Panel of settings applets. simplewm is
feature complete, so expect bug fixes and UI polish, not new features.

## Working with the user

- They never read the code. They judge simplewm by using it, so report changes in terms of
  what they'll see and do, not in terms of code.
- Claude can't see the screen. After a UI change, say exactly what to try. The user runs it
  nested (`make run`) or on a TTY (`./simplewm`, or `./debug.sh` to log DRM and session
  details to `simplewm-debug.log`). Their machine has one sound output (HDMI), one wired
  network card and one monitor.
- Keep this file current when behavior or structure changes. It's for Claude; nobody else
  reads it.

## Rules simplewm keeps

- No keyboard shortcuts. The one exception is Ctrl+Alt+F1… for switching virtual terminals
  on a TTY.
- No config file, flags or environment variables. Every setting is changed in a Control
  Panel applet and saved in `~/.config/simplewm/simplewm.db` (SQLite). The backend is
  chosen automatically: TTY when there's no `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` or
  `DISPLAY`, nested otherwise.
- Wayland only (no Xwayland), one monitor (the first connected one, at its preferred mode),
  US keyboard layout, one bundled font (DejaVu Sans).
- Windows always float. Title bars are always drawn by simplewm (server-side decorations
  are forced). Size moves and resizes a window once (halves, quarters, full screen below
  the bar) and doesn't snap it there.
- simplewm is the only `xdg-desktop-portal` backend (`make install` sets that up) and only
  implements FileChooser, answered with its own File Picker. On a TTY it exports
  `WAYLAND_DISPLAY` and `XDG_CURRENT_DESKTOP=simplewm` to systemd at startup, restarts the
  portal, and removes them on exit. Nested, it leaves the host session alone, so nested
  apps get the host's file dialogs.
- The Network applet sets a static IPv4 address and gateway itself over rtnetlink (no
  DHCP, no DNS). That needs `CAP_NET_ADMIN`, which `make` gives the binary with `sudo
  setcap` (only when the binary changed). Programs simplewm launches don't get it.
  NetworkManager must be masked or it undoes the settings.
- The Sound applet drives PipeWire through `pw-dump` and `wpctl`, and WirePlumber remembers
  the choices. With one output it just names it; with several it lists them to choose from.

## Checking a change

- `cargo build`, `cargo clippy --all-targets` (keep it at zero warnings), `cargo test`. Run
  `cargo fmt` whenever you like.
- Tests cover the logic that doesn't need a screen: the settings database, text fields, row
  lists, File Picker listing and search, screenshot PNGs, portal globs and checks, network
  address parsing and PipeWire output parsing. A few only do something on a machine that has
  the hardware or service.
- The `#[ignore]`d tests need a setup; each one's doc comment says how to run it. Two
  exercise D-Bus on a scratch bus: `dbus-run-session -- cargo test bus -- --ignored` drives
  the file dialog backend the way an app would and checks the Desktop Portal bus checks.
  Run it after touching `portal.rs`, `portal_check.rs` or zbus. `network::namespace_test`
  applies addresses to a dummy card.
- `make` builds the release binary with `--locked`; `make install` puts it in `/usr/bin`
  and installs the portal files.

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
- SQLite is the system library, not bundled, so SQLite fixes arrive with OS updates.
- zbus 6 has no blocking API, and a connection only runs while some thread is inside
  `zbus::block_on`. The portal thread stays in it forever, and the Desktop Portal checks
  wrap their D-Bus calls in it on their worker thread. Never call `block_on` from inside a
  served D-Bus method.

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
- Behind the applets, with no UI code:
  - `network.rs`: rtnetlink.
  - `sound.rs`: PipeWire.
  - `screenshot.rs`: offscreen re-render, then the PNG is written on a worker thread.
  - `portal.rs`: the zbus FileChooser backend, on its own thread inside `zbus::block_on`.
  - `portal_check.rs`: the Desktop Portal applet's checks, on a worker thread.
  - `session_env.rs`: exporting simplewm's display to systemd.

  Worker threads report back over calloop channels.

## Panels (`panel.rs`, `panel/`)

`Panels` owns every window simplewm draws itself. Each `PanelId` gets a `PanelWindow`
(open flag, position, buffers), and `Panels` also holds each applet's state. `panel.rs`
has what they all share: placement, the frame, hit-test and render dispatch, `PanelHit`,
and the shared sizes and colors. Each window has its own file of `impl super::Panels`
blocks:

- `control.rs`: the Control Panel and Help.
- `launcher.rs`, `screenshot.rs`, `network.rs` and `sound.rs`: one applet each.
- `rows.rs`: Environment, Desktop Portal and the `RowList` they share with Network.
- `picker.rs`: the File Picker.

Two shared files support them: `text_field.rs` (editable fields) and `db.rs` (settings;
add a step to `migrate` to change the schema). Applet files usually go in this order:
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
