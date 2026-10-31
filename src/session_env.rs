//! Publishing our display to the D-Bus / systemd activation environment.
//!
//! Services started on demand over D-Bus (notably `xdg-desktop-portal-gtk`, which draws
//! file chooser dialogs) inherit systemd's user environment, not ours. Without
//! `WAYLAND_DISPLAY` there they fail with "cannot open display" and apps get no dialog.

use std::ffi::OsStr;
use std::process::Command;

const DESKTOP: &str = "simplewm";

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
    // Stop a backend left over from an earlier session so the next request starts a
    // fresh one with the new environment.
    stop_portal_backend();
}

/// Remove what `export` set, so nothing tries to reach our socket after we exit.
pub fn unexport() {
    run(Command::new("systemctl")
        .args(["--user", "unset-environment", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP"]));
    stop_portal_backend();
}

fn stop_portal_backend() {
    run(Command::new("systemctl").args(["--user", "stop", "xdg-desktop-portal-gtk.service"]));
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
