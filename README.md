# simplewm

A very simple Wayland compositor written in Rust with Smithay.

* No keyboard shortcuts
* No config file
* Stacking, floating windows
* Wayland only (no Xwayland / X11)

## Building and running

* `make` builds a release binary at `./simplewm`
* Nested (inside an existing Wayland session, for development): `make run` or `./simplewm`
* On real hardware from a TTY: `make tty` or `./simplewm --tty`
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

There is a top bar with two buttons:

* Settings — opens a menu with a single item: Quit
* Launcher — opens a menu with a single item: Alacritty

## Defaults (not configurable)

* A font is bundled into the binary for the bar, menus and titles
* Nested mode uses a fixed-size window; TTY mode uses the monitor's preferred mode
* Keyboard input goes to the focused window, US layout
* Default arrow cursor drawn by the compositor; client-set cursors are honored
