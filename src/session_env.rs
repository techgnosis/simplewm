//! Publishing our display to the D-Bus / systemd activation environment.
//!
//! Services started on demand over D-Bus (notably `xdg-desktop-portal`, which routes file
//! chooser requests to us by `XDG_CURRENT_DESKTOP`, and other portal backends that open
//! windows) inherit systemd's user environment, not ours.

use std::ffi::OsStr;
use std::process::Command;

pub const DESKTOP: &str = "simplewm";

/// Export `WAYLAND_DISPLAY` and `XDG_CURRENT_DESKTOP` to D-Bus activation and systemd.
pub fn export(socket_name: &OsStr) {
    let mut wayland = std::ffi::OsString::from("WAYLAND_DISPLAY=");
    wayland.push(socket_name);
    run(
        Command::new("dbus-update-activation-environment")
            .arg("--systemd")
            .arg(wayland)
            .arg(format!("XDG_CURRENT_DESKTOP={DESKTOP}")),
    );
    // Stop a portal left over from an earlier session so the next request starts a
    // fresh one that reads our environment and `simplewm-portals.conf`.
    stop_portals();
}

/// Remove what `export` set, so nothing tries to reach our socket after we exit.
pub fn unexport() {
    run(Command::new("systemctl")
        .args(["--user", "unset-environment", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP"]));
    stop_portals();
}

/// Stop the portal frontend; D-Bus restarts it on demand.
fn stop_portals() {
    run(Command::new("systemctl").args(["--user", "stop", "xdg-desktop-portal.service"]));
}

fn run(command: &mut Command) {
    let program = command.get_program().to_string_lossy().into_owned();
    match command.output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            program,
            status = %out.status,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "session environment command failed"
        ),
        Err(err) => tracing::warn!(program, ?err, "could not run session environment command"),
    }
}
