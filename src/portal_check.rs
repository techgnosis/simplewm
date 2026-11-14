//! Checks behind the Desktop Portal applet: what to look at when apps get no file
//! dialog (or a different one). simplewm is the portal itself on a TTY (see
//! `crate::portal`), so this is mostly about who owns the portal's bus name.
//!
//! The checks talk to D-Bus, so they run on a worker thread and report back to the
//! compositor through a calloop channel.

use std::time::Duration;

use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, OwnedVariant, fdo::DBusProxy, names::BusName};

use crate::{
    panel::EnvRow,
    portal::{self, BUS_NAME, OBJECT_PATH},
    services::{self, describe_pid},
};

const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";

/// Run the checks on a worker thread and send the rows to `tx`.
pub fn spawn(tx: Sender<Vec<EnvRow>>) {
    let spawned = std::thread::Builder::new()
        .name("portal-check".into())
        .spawn(move || {
            let _ = tx.send(run());
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

/// Problems found, shown in the summary at the top: a short description of each
/// and what to do about it. Keep both short; rows are cut off at the window edge.
#[derive(Default)]
struct Problems(Vec<(String, String)>);

impl Problems {
    fn add(&mut self, what: impl Into<String>, fix: impl Into<String>) {
        self.0.push((what.into(), fix.into()));
    }
}

fn run() -> Vec<EnvRow> {
    let mut problems = Problems::default();
    let mut rows = Vec::new();
    let status = portal::status();
    let nested = matches!(status.state, portal::State::Off);
    portal_rows(&status, &mut rows, &mut problems);
    zbus::block_on(bus_rows(nested, &mut rows, &mut problems));

    let checked = crate::clock::now("%H:%M:%S");
    let mut out = vec![EnvRow::Header("Summary".into())];
    match problems.0.len() {
        0 => out.push(EnvRow::Line(format!(
            "No problems found (checked {checked})."
        ))),
        n => {
            let s = if n == 1 { "" } else { "s" };
            out.push(EnvRow::Line(format!(
                "{n} problem{s} found (checked {checked}):"
            )));
            for (what, fix) in problems.0 {
                out.push(EnvRow::Line(format!("• {what}")));
                out.push(EnvRow::Line(format!("    {fix}")));
            }
        }
    }
    out.extend(rows);
    out
}

// --- simplewm's portal -------------------------------------------------------------

fn portal_rows(status: &portal::Status, rows: &mut Vec<EnvRow>, problems: &mut Problems) {
    rows.push(EnvRow::Header("simplewm file dialogs".into()));
    let state = match &status.state {
        portal::State::Off => "off: nested, apps get the host desktop's file dialogs".to_string(),
        portal::State::Starting => "starting".into(),
        portal::State::Serving => format!("serving as {BUS_NAME}"),
        portal::State::Waiting { owner } => {
            let who = owner
                .map(describe_pid)
                .unwrap_or_else(|| "another program".into());
            if who.contains("xdg-desktop-portal") {
                problems.add(
                    "xdg-desktop-portal holds the portal's D-Bus name",
                    "simplewm stopped it; if it's back, something restarts it",
                );
            } else {
                problems.add(
                    format!("{who} holds the portal's D-Bus name"),
                    "Quit it; simplewm takes the name over then",
                );
            }
            format!("waiting for {who} to let go of {BUS_NAME}")
        }
        portal::State::NoBus => {
            problems.add("There is no session bus", "Start D-Bus in the D-Bus applet");
            "waiting for the session bus".into()
        }
        portal::State::Failed(why) => {
            problems.add(
                "simplewm's file dialog service is not running",
                "See State below for why",
            );
            format!("not running: {why}")
        }
    };
    rows.push(item("State", state));
    rows.push(item("Requests received", status.requests.to_string()));
    rows.push(item(
        "Last request",
        status
            .last_request
            .clone()
            .unwrap_or_else(|| "(none yet)".into()),
    ));
    rows.push(item(
        "Session bus address",
        services::bus_address().unwrap_or_else(|| "(none: XDG_RUNTIME_DIR is not set)".into()),
    ));
}

// --- session bus -----------------------------------------------------------------

/// Who owns the portal name, and whether apps can see its file chooser.
/// Run on a worker thread, inside `zbus::block_on` (see `run`).
async fn bus_rows(nested: bool, rows: &mut Vec<EnvRow>, problems: &mut Problems) {
    rows.push(EnvRow::Header("What apps see on the session bus".into()));
    let conn = zbus::connection::Builder::session()
        .method_timeout(Duration::from_secs(3))
        .build()
        .await;
    let conn = match conn {
        Ok(c) => c,
        Err(err) => {
            problems.add(
                "Cannot connect to the session D-Bus",
                "Check the D-Bus applet",
            );
            rows.push(item("Connection", format!("failed: {err}")));
            return;
        }
    };
    let dbus = match DBusProxy::new(&conn).await {
        Ok(p) => p,
        Err(err) => {
            rows.push(item("org.freedesktop.DBus", format!("failed: {err}")));
            return;
        }
    };

    let owner = owner_pid(&dbus, BUS_NAME).await;
    rows.push(item(
        BUS_NAME,
        match owner {
            Some(pid) if pid == std::process::id() => "this simplewm".to_string(),
            Some(pid) => describe_pid(pid),
            None => "no owner".into(),
        },
    ));
    if owner.is_some() {
        let chooser = match file_chooser_version(&conn).await {
            Ok(version) => format!("available (version {version})"),
            Err(err) => {
                if !nested {
                    problems.add("The portal has no file dialogs", "See FileChooser below");
                }
                format!("missing: {err}")
            }
        };
        rows.push(item("FileChooser", chooser));
    }
}

async fn owner_pid(dbus: &DBusProxy<'_>, name: &str) -> Option<u32> {
    let name = BusName::try_from(name).ok()?;
    if !dbus.name_has_owner(name.clone()).await.unwrap_or(false) {
        return None;
    }
    dbus.get_connection_unix_process_id(name).await.ok()
}

/// The portal's FileChooser interface version, read the way apps read it.
async fn file_chooser_version(conn: &Connection) -> Result<u32, String> {
    let reply = conn
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(FILE_CHOOSER, "version"),
        )
        .await
        .map_err(|e| e.to_string())?;
    let value: OwnedVariant = reply.body().deserialize().map_err(|e| e.to_string())?;
    u32::try_from(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On simplewm's own bus, on a scratch socket:
    /// `cargo test reads_an_empty_bus -- --ignored`.
    #[test]
    #[ignore]
    fn reads_an_empty_bus() {
        crate::bus::scratch_session();
        let mut rows = Vec::new();
        let mut problems = Problems::default();
        zbus::block_on(bus_rows(false, &mut rows, &mut problems));
        assert!(problems.0.is_empty(), "{:?}", problems.0);
        let value = rows.iter().find_map(|r| match r {
            EnvRow::Item(k, v) if k == BUS_NAME => Some(v.clone()),
            _ => None,
        });
        // `cargo test bus` also runs the portal's own test, which may own the name by now.
        assert!(
            matches!(value.as_deref(), Some("no owner" | "this simplewm")),
            "{value:?}"
        );
    }
}
