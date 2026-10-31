# simplewm

A very simple Wayland compositor written in Rust with Smithay.

* No keyboard shortcuts
* No hand-edited config file; settings changed in the Control Panel are saved to
  `~/.config/simplewm/simplewm.db` (SQLite)
* Stacking, floating windows
* Wayland only (no Xwayland / X11)

## Building and running

* `make` builds a release binary at `./simplewm`
* Nested (inside an existing Wayland session, for development): `make run` or `./simplewm`
* On real hardware from a TTY: `./simplewm` (the TTY backend is chosen automatically when
  no `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` or `DISPLAY` is set). `--tty` (or `make tty`)
  forces the TTY backend even inside a host session.
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

* **Launcher** — a list of text fields, one absolute executable path each, with a
  Remove button per entry and an Add button. `/usr/bin/alacritty` is the default
  first entry and can be removed. Click a field to edit it; Enter/Escape stops
  editing, Tab/arrow keys move between fields. Changes are saved immediately.
* **Environment** — a read-only, scrollable (mouse wheel) view of how simplewm was
  started: the Wayland socket clients connect to (`WAYLAND_DISPLAY`, socket and
  lock file paths), backend details (host compositor socket when nested; seat,
  GPU device and connector on a TTY; dmabuf render node), the output, the
  protocols advertised to clients, process info, and all environment variables.
* **Screenshot** — one text field: the folder screenshots are saved in. There is
  no default; `~` is expanded, and the folder is created if it doesn't exist.
* **Help** — a short read-only note: the resolution is the monitor's preferred mode, there is
  no plain text config file, and there are no keyboard shortcuts.

## File dialogs (desktop portal)

Apps like Firefox open file dialogs through `xdg-desktop-portal`, which starts
`xdg-desktop-portal-gtk` on demand as a systemd user service. That service only sees
systemd's environment, so in TTY mode simplewm exports `WAYLAND_DISPLAY` and
`XDG_CURRENT_DESKTOP=simplewm` there at startup (via
`dbus-update-activation-environment --systemd`) and removes them on exit. Nested mode
leaves the host session's environment alone.

Optionally copy `simplewm-portals.conf` to `~/.config/xdg-desktop-portal/` so the
portal picks the GTK backend explicitly rather than as a fallback.

## Defaults (not configurable)

* A font is bundled into the binary for the bar, menus and titles
* Nested mode uses a fixed-size window; TTY mode uses the monitor's preferred mode
* Keyboard input goes to the focused window, US layout
* Default arrow cursor drawn by the compositor; client-set cursors are honored
