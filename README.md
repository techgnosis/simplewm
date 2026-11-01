# simplewm

A very simple Wayland compositor written in Rust with Smithay.

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
* **Help** — a short read-only note: the resolution is the monitor's preferred mode, there is
  no plain text config file, and there are no keyboard shortcuts.

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

One-time setup: `make install-portal`. It copies `simplewm.portal` to
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
