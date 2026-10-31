//! Checks behind the Desktop Portal applet: what to look at when apps get no file
//! dialog (or a different one) from `xdg-desktop-portal`.
//!
//! The checks talk to D-Bus and run `systemctl` / `journalctl`, so they run on a
//! worker thread and report back to the compositor through a calloop channel.

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use smithay::reexports::calloop::channel::Sender;
use zbus::{blocking::Connection, blocking::fdo::DBusProxy, names::BusName, zvariant::OwnedValue};

use crate::{panel::EnvRow, portal, session_env::DESKTOP};

const FRONTEND: &str = "org.freedesktop.portal.Desktop";
const GTK_BACKEND: &str = "org.freedesktop.impl.portal.desktop.gtk";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";
const PORTAL_FILE: &str = "/usr/share/xdg-desktop-portal/portals/simplewm.portal";
const JOURNAL_LINES: &str = "15";

/// What the checks need to know about this simplewm.
pub struct Context {
    /// Our Wayland socket name.
    pub wayland_display: String,
    /// When simplewm started (limits the journal to this session).
    pub started: SystemTime,
}

/// Run the checks on a worker thread and send the rows to `tx`.
pub fn spawn(ctx: Context, tx: Sender<Vec<EnvRow>>) {
    let spawned = std::thread::Builder::new().name("portal-check".into()).spawn(move || {
        let _ = tx.send(run(&ctx));
    });
    if let Err(err) = spawned {
        tracing::warn!(?err, "portal check: cannot start thread");
    }
}

/// Rows shown while the checks run.
pub fn pending_rows() -> Vec<EnvRow> {
    vec![EnvRow::Header("Checking…".into())]
}

fn item(k: &str, v: impl Into<String>) -> EnvRow {
    EnvRow::Item(k.to_string(), v.into())
}

/// Problems found, shown in the summary at the top.
#[derive(Default)]
struct Problems(Vec<String>);

impl Problems {
    fn add(&mut self, p: impl Into<String>) {
        self.0.push(p.into());
    }
}

fn run(ctx: &Context) -> Vec<EnvRow> {
    let mut problems = Problems::default();
    let mut rows = Vec::new();
    backend_rows(&mut rows, &mut problems);
    let frontend_pid = bus_rows(&mut rows, &mut problems);
    environment_rows(ctx, frontend_pid, &mut rows, &mut problems);
    file_rows(&mut rows, &mut problems);
    journal_rows(ctx, &mut rows);
    tips_rows(&mut rows);

    let mut out = vec![EnvRow::Header("Summary".into())];
    if problems.0.is_empty() {
        out.push(EnvRow::Line("No problems found.".into()));
    } else {
        out.extend(problems.0.into_iter().map(|p| EnvRow::Line(format!("• {p}"))));
    }
    out.push(EnvRow::Line(format!("Checked at {}", chrono::Local::now().format("%H:%M:%S"))));
    out.extend(rows);
    out
}

// --- simplewm's own backend ----------------------------------------------------

fn backend_rows(rows: &mut Vec<EnvRow>, problems: &mut Problems) {
    let status = portal::status();
    rows.push(EnvRow::Header("simplewm file chooser backend".into()));
    rows.push(item("Bus name", portal::BUS_NAME));
    let state = match (&status.problem, status.serving) {
        (Some(p), _) => {
            problems.add(format!("simplewm's backend is not running: {p}"));
            format!("not serving: {p}")
        }
        (None, true) => "serving".into(),
        (None, false) => "starting".into(),
    };
    rows.push(item("State", state));
    rows.push(item("Requests received", status.requests.to_string()));
    rows.push(item("Last request", status.last_request.unwrap_or_else(|| "(none yet)".into())));
    rows.push(item(
        "DBUS_SESSION_BUS_ADDRESS",
        std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_else(|_| "(not set)".into()),
    ));
}

// --- session bus -----------------------------------------------------------------

/// Bus name owners. Returns the frontend's PID if it is running.
fn bus_rows(rows: &mut Vec<EnvRow>, problems: &mut Problems) -> Option<u32> {
    rows.push(EnvRow::Header("Session bus".into()));
    let conn = zbus::blocking::connection::Builder::session()
        .map(|b| b.method_timeout(Duration::from_secs(3)))
        .and_then(|b| b.build());
    let conn = match conn {
        Ok(c) => c,
        Err(err) => {
            problems.add(format!("cannot connect to the session bus: {err}"));
            rows.push(item("Connection", format!("failed: {err}")));
            return None;
        }
    };
    let dbus = match DBusProxy::new(&conn) {
        Ok(p) => p,
        Err(err) => {
            rows.push(item("org.freedesktop.DBus", format!("failed: {err}")));
            return None;
        }
    };

    let ours = owner_pid(&dbus, portal::BUS_NAME);
    let ours_text = match ours {
        Some(pid) if pid == std::process::id() => "owned by this simplewm".to_string(),
        Some(pid) => {
            problems.add(format!("{} is owned by another process (PID {pid})", portal::BUS_NAME));
            format!("owned by another process: {}", describe_pid(pid))
        }
        None => "no owner".into(),
    };
    rows.push(item("simplewm backend", ours_text));

    let frontend = owner_pid(&dbus, FRONTEND);
    rows.push(item(
        "xdg-desktop-portal",
        match frontend {
            Some(pid) => format!("running: {}", describe_pid(pid)),
            None => "not running (D-Bus starts it when an app asks)".into(),
        },
    ));
    if frontend.is_some() {
        let chooser = match frontend_file_chooser(&conn) {
            Ok(version) => format!("available (version {version})"),
            Err(err) => {
                problems.add(
                    "the running xdg-desktop-portal has no FileChooser: it found no backend for it \
                     (check the files below, then restart it)",
                );
                format!("missing: {err}")
            }
        };
        rows.push(item("Frontend FileChooser", chooser));
    }
    rows.push(item(
        "GTK backend",
        match owner_pid(&dbus, GTK_BACKEND) {
            Some(pid) => format!("running: {}", describe_pid(pid)),
            None => "not running".into(),
        },
    ));
    frontend
}

fn owner_pid(dbus: &DBusProxy, name: &str) -> Option<u32> {
    let name = BusName::try_from(name).ok()?;
    if !dbus.name_has_owner(name.clone()).unwrap_or(false) {
        return None;
    }
    dbus.get_connection_unix_process_id(name).ok()
}

/// The frontend's FileChooser interface version (fails if it did not export it).
fn frontend_file_chooser(conn: &Connection) -> Result<u32, String> {
    let reply = conn
        .call_method(
            Some(FRONTEND),
            PORTAL_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(FILE_CHOOSER, "version"),
        )
        .map_err(|e| e.to_string())?;
    let value: OwnedValue = reply.body().deserialize().map_err(|e| e.to_string())?;
    u32::try_from(value).map_err(|e| e.to_string())
}

/// `PID 123 (/usr/lib/xdg-desktop-portal)`.
fn describe_pid(pid: u32) -> String {
    let cmd = std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .and_then(|b| b.split(|&c| c == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()))
        .filter(|s| !s.is_empty());
    match cmd {
        Some(cmd) => format!("PID {pid} ({cmd})"),
        None => format!("PID {pid}"),
    }
}

// --- environment -------------------------------------------------------------------

fn environment_rows(ctx: &Context, frontend_pid: Option<u32>, rows: &mut Vec<EnvRow>, problems: &mut Problems) {
    rows.push(EnvRow::Header("Environment seen by D-Bus-started services".into()));
    rows.push(item("Ours: WAYLAND_DISPLAY", ctx.wayland_display.clone()));
    rows.push(item(
        "Ours: XDG_CURRENT_DESKTOP",
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "(not set)".into()),
    ));

    match command_output(Command::new("systemctl").args(["--user", "show-environment"])) {
        Ok(out) => {
            let get = |key: &str| {
                out.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('='))).map(str::to_string)
            };
            let desktop = get("XDG_CURRENT_DESKTOP");
            let wayland = get("WAYLAND_DISPLAY");
            if !desktop_matches(desktop.as_deref()) {
                problems.add(format!(
                    "systemd's XDG_CURRENT_DESKTOP is {}, not {DESKTOP}: newly started portals won't pick simplewm",
                    desktop.as_deref().unwrap_or("unset")
                ));
            }
            rows.push(item("systemd: XDG_CURRENT_DESKTOP", desktop.unwrap_or_else(|| "(not set)".into())));
            rows.push(item("systemd: WAYLAND_DISPLAY", wayland.unwrap_or_else(|| "(not set)".into())));
        }
        Err(err) => rows.push(item("systemctl --user show-environment", format!("failed: {err}"))),
    }

    // The frontend picks backends by the XDG_CURRENT_DESKTOP it was started with.
    if let Some(pid) = frontend_pid {
        match process_env(pid, "XDG_CURRENT_DESKTOP") {
            Ok(desktop) => {
                if !desktop_matches(desktop.as_deref()) {
                    problems.add(format!(
                        "the running xdg-desktop-portal was started with XDG_CURRENT_DESKTOP={}: restart it",
                        desktop.as_deref().unwrap_or("(unset)")
                    ));
                }
                rows.push(item(
                    "Running portal: XDG_CURRENT_DESKTOP",
                    desktop.unwrap_or_else(|| "(not set)".into()),
                ));
            }
            Err(err) => rows.push(item("Running portal: XDG_CURRENT_DESKTOP", format!("cannot read: {err}"))),
        }
    }
}

fn desktop_matches(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.split(':').any(|d| d.eq_ignore_ascii_case(DESKTOP)))
}

/// One variable from `/proc/<pid>/environ` (`Ok(None)` if unset).
fn process_env(pid: u32, key: &str) -> std::io::Result<Option<String>> {
    let environ = std::fs::read(format!("/proc/{pid}/environ"))?;
    let prefix = format!("{key}=");
    Ok(environ
        .split(|&c| c == 0)
        .find_map(|kv| kv.strip_prefix(prefix.as_bytes()))
        .map(|v| String::from_utf8_lossy(v).into_owned()))
}

// --- files ----------------------------------------------------------------------

fn file_rows(rows: &mut Vec<EnvRow>, problems: &mut Problems) {
    rows.push(EnvRow::Header("Files".into()));
    match std::fs::read_to_string(PORTAL_FILE) {
        Ok(text) => {
            let ok = text.contains(portal::BUS_NAME)
                && text.contains("org.freedesktop.impl.portal.FileChooser")
                && text.lines().any(|l| l.starts_with("UseIn=") && l.contains(DESKTOP));
            if !ok {
                problems.add(format!("{PORTAL_FILE} does not match simplewm.portal: run make install-portal"));
            }
            rows.push(item(PORTAL_FILE, if ok { "installed" } else { "installed, but outdated" }));
        }
        Err(err) => {
            problems.add("simplewm.portal is not installed: run make install-portal");
            rows.push(item(PORTAL_FILE, format!("missing ({err})")));
        }
    }

    // Portal configs, in the frontend's search order; the first one found is used.
    let found: Vec<(PathBuf, String)> = config_candidates()
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect();
    if found.is_empty() {
        problems.add("no portal config found (simplewm-portals.conf): run make install-portal");
        rows.push(item("Portal config", "none found"));
    }
    for (i, (path, text)) in found.iter().enumerate() {
        let chooser = config_value(text, "org.freedesktop.impl.portal.FileChooser");
        let default = config_value(text, "default");
        if i == 0 && chooser.as_deref() != Some(DESKTOP) {
            problems.add(format!(
                "{} is used first and does not set org.freedesktop.impl.portal.FileChooser=simplewm",
                path.display()
            ));
        }
        let label = if i == 0 { "Portal config (used)" } else { "Portal config (ignored)" };
        rows.push(item(label, path.display().to_string()));
        rows.push(item(
            "    settings",
            format!(
                "FileChooser={}, default={}",
                chooser.as_deref().unwrap_or("(unset)"),
                default.as_deref().unwrap_or("(unset)")
            ),
        ));
    }
}

/// `<desktop>-portals.conf` then `portals.conf`, in each config and data directory.
fn config_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let env_dir = |var: &str, fallback: Option<PathBuf>| std::env::var_os(var).map(PathBuf::from).or(fallback);
    let env_dirs = |var: &str, fallback: &str| {
        std::env::var(var)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| fallback.to_string())
            .split(':')
            .map(PathBuf::from)
            .collect::<Vec<_>>()
    };

    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(env_dir("XDG_CONFIG_HOME", home.as_ref().map(|h| h.join(".config"))));
    dirs.extend(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
    dirs.push(PathBuf::from("/etc"));
    dirs.extend(env_dir("XDG_DATA_HOME", home.as_ref().map(|h| h.join(".local/share"))));
    dirs.extend(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
    dirs.push(PathBuf::from("/usr/share"));

    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        let dir = dir.join("xdg-desktop-portal");
        for name in [format!("{DESKTOP}-portals.conf"), "portals.conf".to_string()] {
            let p = dir.join(name);
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// A `key=value` line in a portals.conf `[preferred]` section.
fn config_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
}

// --- journal ----------------------------------------------------------------------

fn journal_rows(ctx: &Context, rows: &mut Vec<EnvRow>) {
    rows.push(EnvRow::Header("xdg-desktop-portal log since simplewm started".into()));
    let since = ctx.started.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let out = command_output(Command::new("journalctl").args([
        "--user",
        "--unit=xdg-desktop-portal.service",
        "--no-pager",
        "--output=short",
        &format!("--since=@{since}"),
        &format!("--lines={JOURNAL_LINES}"),
    ]));
    match out {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with("-- ")).collect();
            if lines.is_empty() {
                rows.push(EnvRow::Line("(nothing logged)".into()));
            }
            rows.extend(lines.into_iter().map(|l| EnvRow::Line(shorten_journal_line(l))));
        }
        Err(err) => rows.push(EnvRow::Line(format!("journalctl failed: {err}"))),
    }
}

/// `Oct 31 12:00:01 host xdg-desktop-portal[123]: msg` → `12:00:01 msg`.
fn shorten_journal_line(line: &str) -> String {
    let parts: Vec<&str> = line.splitn(5, ' ').collect();
    match parts.as_slice() {
        [_, _, time, _, rest] => {
            let msg = rest.split_once(": ").map(|(_, m)| m).unwrap_or(rest);
            format!("{time} {msg}")
        }
        _ => line.to_string(),
    }
}

// --- tips ---------------------------------------------------------------------------

fn tips_rows(rows: &mut Vec<EnvRow>) {
    rows.push(EnvRow::Header("Things to try".into()));
    for (k, v) in [
        ("make install-portal", "install simplewm.portal and simplewm-portals.conf"),
        ("systemctl --user restart xdg-desktop-portal", "reread the config and environment, then Refresh"),
        ("/usr/lib/xdg-desktop-portal -r -v", "run the frontend in a terminal with debug output"),
        ("busctl --user list | grep portal", "see which portal services are on the bus"),
        ("journalctl --user -u xdg-desktop-portal", "the frontend's full log"),
        ("Firefox", "about:config widget.use-xdg-desktop-portal.file-picker = 1"),
        ("GTK 3 apps", "start with GTK_USE_PORTAL=1"),
        ("GTK 4 apps", "start with GDK_DEBUG=portals"),
        ("Qt apps", "start with QT_QPA_PLATFORMTHEME=xdgdesktopportal"),
    ] {
        rows.push(item(k, v));
    }
}

fn command_output(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { out.status.to_string() } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

