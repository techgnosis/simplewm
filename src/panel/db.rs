//! Settings storage: SQLite at ~/.config/simplewm/simplewm.db.
//!
//! Tables: `launcher` (the Launcher menu's executables, in order) and
//! `settings` (key/value pairs: the screenshot folder, the Network applet's
//! address and gateway). `PRAGMA user_version` is the schema version; add a
//! new `if version < N` step to `migrate` to change the schema.

use std::path::PathBuf;

use rusqlite::Connection;

use super::DEFAULT_LAUNCHER_ENTRY;

pub fn db_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/simplewm/simplewm.db"))
}

/// Open (creating and migrating if needed) the settings database. Without
/// one, settings still work but are not remembered.
pub(super) fn open_db() -> Option<Connection> {
    let path = db_path()?;
    if let Some(dir) = path.parent()
        && let Err(err) = std::fs::create_dir_all(dir)
    {
        tracing::error!(?err, ?dir, "failed to create config directory");
        return None;
    }
    let conn = match Connection::open(&path) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(?err, ?path, "failed to open database");
            return None;
        }
    };
    if let Err(err) = migrate(&conn) {
        tracing::error!(?err, "failed to initialize database");
        return None;
    }
    tracing::info!(?path, "opened database");
    Some(conn)
}

/// Bring the schema up to date.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);
    if version < 1 {
        // First run: create the schema and the default launcher entry. After
        // this the entry can be deleted and stays deleted.
        conn.execute_batch(&format!(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS launcher (position INTEGER PRIMARY KEY, path TEXT NOT NULL);
             INSERT INTO launcher (position, path) VALUES (0, '{DEFAULT_LAUNCHER_ENTRY}');
             PRAGMA user_version = 1;
             COMMIT;"
        ))?;
    }
    if version < 2 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    Ok(())
}

pub(super) fn load_entries(conn: &Connection) -> Vec<String> {
    let result = (|| -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare("SELECT path FROM launcher ORDER BY position")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    })();
    result.unwrap_or_else(|err| {
        tracing::error!(?err, "failed to load launcher entries");
        Vec::new()
    })
}

/// Replace the launcher entries with `paths`, in order.
pub(super) fn save_entries(conn: &mut Connection, paths: &[String]) {
    let result = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM launcher", [])?;
        for (i, p) in paths.iter().enumerate() {
            tx.execute(
                "INSERT INTO launcher (position, path) VALUES (?1, ?2)",
                (i as i64, p),
            )?;
        }
        tx.commit()
    })();
    if let Err(err) = result {
        tracing::error!(?err, "failed to save launcher entries");
    }
}

pub(super) fn load_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .map_err(|err| {
        if !matches!(err, rusqlite::Error::QueryReturnedNoRows) {
            tracing::error!(?err, key, "failed to load setting");
        }
    })
    .ok()
}

/// Store `(key, value)` pairs in the settings table.
pub(super) fn save_settings(conn: &mut Connection, pairs: &[(&str, &str)]) {
    let result = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        for (key, value) in pairs {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                [key, value],
            )?;
        }
        tx.commit()
    })();
    if let Err(err) = result {
        tracing::error!(?err, "failed to save settings");
    }
}

impl super::Panels {
    /// Store `(key, value)` pairs in the settings table.
    pub(super) fn save_settings(&mut self, pairs: &[(&str, &str)]) {
        if let Some(conn) = self.db.as_mut() {
            save_settings(conn, pairs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn new_database_has_the_default_entry() {
        let conn = fresh();
        assert_eq!(load_entries(&conn), [DEFAULT_LAUNCHER_ENTRY]);
        assert_eq!(load_setting(&conn, "screenshot_dir"), None);
    }

    #[test]
    fn migrating_twice_keeps_settings() {
        let mut conn = fresh();
        save_entries(&mut conn, &[]);
        save_settings(&mut conn, &[("screenshot_dir", "/tmp")]);
        migrate(&conn).unwrap();
        // A deleted default entry stays deleted.
        assert!(load_entries(&conn).is_empty());
        assert_eq!(
            load_setting(&conn, "screenshot_dir").as_deref(),
            Some("/tmp")
        );
    }

    #[test]
    fn saves_entries_in_order() {
        let mut conn = fresh();
        let paths = ["/usr/bin/b", "/usr/bin/a", "/usr/bin/c"].map(String::from);
        save_entries(&mut conn, &paths);
        assert_eq!(load_entries(&conn), paths);
    }

    #[test]
    fn settings_overwrite() {
        let mut conn = fresh();
        save_settings(
            &mut conn,
            &[("net_address", "10.0.0.2/24"), ("net_gateway", "")],
        );
        save_settings(&mut conn, &[("net_address", "10.0.0.3/24")]);
        assert_eq!(
            load_setting(&conn, "net_address").as_deref(),
            Some("10.0.0.3/24")
        );
        assert_eq!(load_setting(&conn, "net_gateway").as_deref(), Some(""));
    }
}
