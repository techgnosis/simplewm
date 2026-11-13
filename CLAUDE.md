# simplewm

A very simple Wayland compositor written in Rust with Smithay.

This file is the spec for how simplewm behaves (the sections after **Working on the code**)
and a guide to its code. Keep it up to date when either changes. The user doesn't read the
code and judges simplewm only by using it, and simplewm is feature complete: expect small
fixes and UI polish more than new features.

* No keyboard shortcuts
* No hand-edited config file; settings changed in the Control Panel are saved to
  `~/.config/simplewm/simplewm.db` (SQLite)
* Stacking, floating windows
* Wayland only (no Xwayland / X11)

## Building and running

* `make` builds a release binary at `./simplewm` and gives it `CAP_NET_ADMIN` with
  `sudo setcap` (asked only when the binary changed), so the Network applet can set the
  network card's address and routes without simplewm running as root. Programs simplewm
  launches don't get the capability.
* Nested (inside an existing Wayland session, for development): `make run` or `./simplewm`
* On real hardware from a TTY: `./simplewm`. simplewm takes no flags: it uses the TTY
  backend when no `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` or `DISPLAY` is set, and runs nested
  otherwise.
* `make clean` removes build output

Only the first connected monitor is used.

## Windows

The compositor always draws its own title bar (server-side decorations are forced).
The title bar shows the window title, and has two buttons on the right:

* A red close button
* A button that says Size. It opens a row of small screen icons, each showing the
  region the window will fill (the hovered one is named underneath):
  * Left half
  * Right half
  * Top left, Top right, Bottom left, Bottom right (quarters)
  * Full screen (fills the area below the top bar; the title bar stays)

No windows are magnetic/snapped; they are always floating. Size moves and resizes the
window once, after which it can be dragged away freely.

Mouse:

* Drag the title bar to move a window
* Drag window edges/corners to resize
* Click a window to focus it and raise it to the top

## Top menu

There is a top bar. Its items (three buttons and a clock) are grouped in the middle:

* Control Panel — opens the Control Panel window (see below)
* Launcher — opens a menu listing the executables configured in the Control Panel,
  by file name (e.g. `alacritty`). Clicking one runs it.
* Screenshot — the screen dims and the cursor becomes a crosshair; drag out a
  rectangle and the shot is taken on release. Esc or a right click cancels. The
  PNG is saved as `simplewm-screenshot-<date>-<time>.png` in the folder set in the
  Screenshot applet, and a short "Saved …" message appears below the bar. If no
  folder is set yet, the button opens the Screenshot applet instead.

After the buttons is a clock (local time, e.g. `Sat 31 Oct  14:05`).

## Control Panel

Settings follow a Windows-style Control Panel model. The Control Panel is a
compositor-drawn window showing one icon (with a name) per settings applet, plus a
**Quit simplewm** button. Clicking an icon opens a separate window dedicated to
that applet's settings. Panel windows stack, focus and move like any other window
(drag the title bar; close with the red button).

Applets:

* **Launcher** — the executables in the Launcher menu, one path per row, with Change… and
  Remove buttons per entry and an Add… button. Add… and Change… open the File Picker
  (Add… starts in `/usr/bin` and accepts several files with Ctrl-click). `/usr/bin/alacritty`
  is the default first entry and can be removed. Changes are saved immediately.
* **Environment** — a read-only, scrollable (mouse wheel) view of how simplewm was
  started: the Wayland socket clients connect to (`WAYLAND_DISPLAY`, socket and
  lock file paths), backend details (host compositor socket when nested; seat,
  GPU device and connector on a TTY; dmabuf render node), the output, and all
  environment variables.
* **Screenshot** — the folder screenshots are saved in, with a Choose… button that opens
  the File Picker to pick it. There is no default; the folder is created if it no longer
  exists.
* **Desktop Portal** — checks for when apps get no file dialog (or a different one): whether
  simplewm owns its portal bus name, whether `xdg-desktop-portal` is running and offers a
  FileChooser, the `XDG_CURRENT_DESKTOP` that systemd and the running portal see, the installed
  `simplewm.portal` and portal config, the portal's log since simplewm started, and commands
  and per-toolkit settings to try. A summary at the top lists problems found; Refresh reruns
  the checks.
* **Network** — a static IPv4 address for the first wired network card. It shows the card's
  state (interface on/off, cable, speed, current addresses and default gateway), and has an
  Address field (`192.168.1.50/24`) and a Gateway field (`192.168.1.1`, or empty for no
  default route). Apply (or Enter) switches the interface on, replaces its IPv4 addresses and
  default route with these, and saves them; simplewm sets them again each time it starts.
  Clearing both fields and applying stops that. There is no DHCP or DNS. If NetworkManager is
  running it may undo the settings, so mask it (`sudo systemctl mask --now NetworkManager`).
* **Sound** — where sound goes and its volume. With one PipeWire output (audio sink) it is
  just named; with several they are listed and clicking one sends sound to it (the chosen one
  is highlighted). Below is that output's volume: click the bar to set it, or use − and +
  (5% steps). PipeWire (WirePlumber) remembers both. Uses `pw-dump` and `wpctl`.
* **Help** — a short read-only note: simplewm takes no flags or environment variables, the
  resolution is the monitor's preferred mode, there is no plain text config file, and there
  are no keyboard shortcuts.

## File dialogs (desktop portal)

simplewm draws file dialogs itself. Apps like Firefox ask `xdg-desktop-portal` for a file
dialog; simplewm registers as that portal's file chooser backend (D-Bus name
`org.freedesktop.impl.portal.desktop.simplewm`) and answers by opening the **File Picker**
window:

* A Home button and the current folder; double-click a folder (or select it and press
  Enter) to enter it, Backspace goes up a folder (in Save dialogs: Backspace in an empty
  Name). A hint under the list says so
* Double-click a file (or select it and press Open / Enter) to choose it; Ctrl-click
  selects several when the app allows it
* Save dialogs have a Name field (pre-filled by the app); saving over an existing file
  asks you to press Save a second time
* Hidden files toggle, and a Type button that cycles through the app's file filters
* Type to search: the list narrows to names containing what you typed (names starting
  with it first, the first match selected); Backspace edits the search, Esc clears it
* Esc, Cancel or the close button cancels; arrow keys, Page Up / Page Down and Home / End
  move the selection (held keys repeat); the mouse wheel scrolls three rows a notch, and
  clicking the scrollbar jumps there
* Only one File Picker is shown at a time; further requests wait their turn

The Launcher and Screenshot applets use the same File Picker to choose executables and
the screenshot folder.

One-time setup: `make install`. It installs simplewm to `/usr/bin/`, copies `simplewm.portal` to
`/usr/share/xdg-desktop-portal/portals/` (with sudo; the portal only looks there) and
`simplewm-portals.conf` to `~/.config/xdg-desktop-portal/`. That config makes simplewm the
only portal backend: no other backend is ever started, and portals simplewm does not
implement yet are simply unavailable.

The `xdg-desktop-portal` frontend itself is still needed, but D-Bus starts it on demand.
It runs as a systemd user service and only sees systemd's environment, so in TTY mode
simplewm exports `WAYLAND_DISPLAY` and `XDG_CURRENT_DESKTOP=simplewm` there at startup
(via `dbus-update-activation-environment --systemd`), restarts the portal so it picks them
up, and removes them on exit. Nested mode leaves the host session's environment alone, so
nested apps get the host's file dialogs.

## Defaults (not configurable)

* A font is bundled into the binary for the bar, menus and titles
* Nested mode uses a fixed-size window; TTY mode uses the monitor's preferred mode
* Keyboard input goes to the focused window, US layout
* Default arrow cursor drawn by the compositor; client-set cursors are honored

## Working on the code

### Checking a change

* `cargo build`, `cargo clippy --all-targets` (keep it free of warnings) and `cargo test`.
  Run `cargo fmt` freely; the formatting churn doesn't matter.
* Tests cover the pure logic: settings database, text fields, row lists, File Picker listing
  and search, screenshot PNGs, portal globs and checks, network address parsing, PipeWire
  output parsing. Some only do something when the machine has the thing (PipeWire, a wired
  card). `network::namespace_test` is `#[ignore]`d; its doc comment says how to run it.
* Claude can't see the screen. The user tries UI changes, nested (`make run`) or on a TTY
  (`./simplewm`, or `debug.sh` for DRM/session logging to `simplewm-debug.log`), so say what
  to look at. On this machine there is one sound output (HDMI) and one wired card.

### Map

* `main.rs`: picks the backend (TTY when there's no host session, nested otherwise) and runs
  the calloop event loop.
* `state.rs`: `State`, which holds everything: the Smithay protocol states, the window
  `Space`, the stacking order of windows and panels (`stack`), `Ui`, `Panels`, the backend.
* `handlers.rs`: the Smithay protocol handlers (compositor, xdg-shell, decorations forced to
  server side, seat, dmabuf, and the protocols gamescope needs).
* `udev.rs` (TTY: DRM/KMS, libinput, libseat; one GPU, first connected output) and
  `winit.rs` (nested window). Both rebuild the whole scene every frame from
  `State::render_elements` and let the damage tracker find what changed, so a state change
  shows up by itself with no redraw request.
* `input.rs`: all keyboard and pointer input. `handle_press` routes a click to the top bar,
  a menu, a panel (`PanelHit` → the action) or a client window. Keys are filtered in this
  order: VT switching (Ctrl+Alt+F1…, the one exception to "no keyboard shortcuts"), Esc
  during a screenshot, the File Picker when it's focused, the text field being edited, and
  otherwise the focused client.
* `grabs.rs`: move and resize grabs for windows and panels.
* `ui.rs`: the top bar, menus, the Size picker, window decorations, the cursor, toasts and
  `build_elements` (the whole scene, front-most first). Also small drawing helpers
  (`text_in`, `text_centered`, `text_in_alpha`, `solid`) that the panels use.
* `text.rs`: the bundled DejaVu Sans font, rasterized with fontdue (`measure`, `fit`).
* `panel.rs` and `panel/`: the compositor-drawn windows (see below).
* Behind the applets (no UI code): `network.rs` (rtnetlink, needs CAP_NET_ADMIN), `sound.rs`
  (`pw-dump`, `wpctl`), `screenshot.rs` (offscreen re-render and PNG on a worker thread),
  `portal.rs` (the zbus FileChooser backend on its own thread), `portal_check.rs` (Desktop
  Portal checks on a worker thread), `session_env.rs` (exporting our display to systemd).
* Worker threads report back over calloop channels (`State::screenshot_tx`,
  `portal_check_tx`, the portal's `PortalEvent` channel).

### Panels

`Panels` (in `panel.rs`) owns every compositor-drawn window: one `PanelWindow` (open flag,
position, frame buffers, content buffers) per `PanelId`, plus each applet's state. `panel.rs`
holds what they share: placement, the frame, hit-test and render dispatch, `PanelHit`, and
the shared sizes and colors. Each window lives in its own file under `panel/` as
`impl super::Panels` blocks: `control.rs` (Control Panel and Help), `launcher.rs`,
`screenshot.rs`, `rows.rs` (Environment and Desktop Portal, and the `RowList` they share
with Network), `network.rs`, `sound.rs` and `picker.rs` (File Picker). Also `text_field.rs`
(editable fields) and `db.rs` (SQLite settings; add a step to `migrate` to change the
schema).

The usual shape of an applet file: layout constants, a `*Layout` struct of rects computed
from the content rect, then `*_size`, `*_hit` (point → `PanelHit`), actions, and `render_*`.

Drawing rules:

* Elements are pushed front-most first, so push text before the background under it.
* Keep `SolidColorBuffer`s from frame to frame (`bufs(&mut self.<window>.solids, n)`, with a
  comment listing what each slot is for). The damage tracker tells elements apart by their
  buffer, so a new buffer every frame would redraw everything.
* Colors are premultiplied RGBA. Hover shows as `*_HOT` colors from the `hovered` hit.

Adding an applet touches: `PanelId` (and `title`), `APPLETS`, a `PanelWindow` in `Panels`
(`new`, `win`, `win_mut`), the `content_size`/`hit`/`render` matches, a glyph in
`control::applet_glyph` (a test checks it fits its tile), any `PanelHit` variants and their
actions in `input.rs` (plus a refresh when it opens, if it shows live state), its own file
under `panel/`, and its section in this file.
