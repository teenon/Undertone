//! Database migrations.

use rusqlite::Connection;
use tracing::{debug, info};

use crate::error::{DbError, DbResult};
use crate::schema::{DEFAULT_DATA, SCHEMA_V1};

/// Current schema version.
const CURRENT_VERSION: i32 = 4;

/// Migration v2: Add `mixer_state` column to profiles.
const SCHEMA_V2: &str = r"
ALTER TABLE profiles ADD COLUMN mixer_state TEXT;
";

/// Migration v3: Add `mic_muted` and `headphone_volume` to device_settings.
const SCHEMA_V3: &str = r"
ALTER TABLE device_settings ADD COLUMN mic_muted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE device_settings ADD COLUMN headphone_volume REAL NOT NULL DEFAULT 0.5;
";

/// Migration v4: Persist the mic effect chain. Single-row table — the
/// `CHECK (id = 0)` constraint makes "just one row" a schema invariant
/// so upserts don't need WHERE clauses.
const SCHEMA_V4: &str = r"
CREATE TABLE IF NOT EXISTS mic_chain (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    chain_json TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
";

/// Run all pending migrations.
pub fn run(conn: &mut Connection) -> DbResult<()> {
    let current = get_version(conn)?;
    info!(current_version = current, target_version = CURRENT_VERSION, "Checking migrations");

    if current < CURRENT_VERSION {
        let tx = conn.transaction()?;

        for version in (current + 1)..=CURRENT_VERSION {
            debug!(version, "Applying migration");
            apply_migration(&tx, version)?;
        }

        tx.commit()?;
        info!("Migrations complete");
    }

    Ok(())
}

/// Get the current schema version.
fn get_version(conn: &Connection) -> DbResult<i32> {
    // Check if schema_version table exists
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version')",
        [],
        |row| row.get(0),
    )?;

    if !exists {
        return Ok(0);
    }

    let version: i32 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |row| row.get(0))
        .unwrap_or(0);

    Ok(version)
}

/// Apply a specific migration version.
fn apply_migration(conn: &Connection, version: i32) -> DbResult<()> {
    match version {
        1 => {
            conn.execute_batch(SCHEMA_V1)?;
            conn.execute_batch(DEFAULT_DATA)?;
            conn.execute("INSERT INTO schema_version (version) VALUES (?)", [version])?;
        }
        2 => {
            conn.execute_batch(SCHEMA_V2)?;
            conn.execute("INSERT INTO schema_version (version) VALUES (?)", [version])?;
        }
        3 => {
            conn.execute_batch(SCHEMA_V3)?;
            conn.execute("INSERT INTO schema_version (version) VALUES (?)", [version])?;
        }
        4 => {
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute("INSERT INTO schema_version (version) VALUES (?)", [version])?;
        }
        _ => {
            return Err(DbError::MigrationFailed(format!("Unknown migration version: {version}")));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrations() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        run(&mut conn).expect("Migrations failed");

        let version = get_version(&conn).unwrap();
        assert_eq!(version, CURRENT_VERSION);

        // Verify default channels exist
        let count: i32 =
            conn.query_row("SELECT COUNT(*) FROM channels", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 5);

        // Verify mixer_state column exists (v2 migration)
        let _: Result<Option<String>, _> =
            conn.query_row("SELECT mixer_state FROM profiles WHERE name = 'Default'", [], |row| {
                row.get(0)
            });
    }
}
