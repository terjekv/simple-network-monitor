use crate::storage::StorageError;
use rusqlite::Connection;

const SCHEMA_VERSION: i64 = 4;

pub(super) fn migrate(conn: &Connection) -> Result<(), StorageError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StorageError::InvalidData(format!(
            "database schema version {version} is newer than supported version {SCHEMA_VERSION}"
        )));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }

    let txn = conn.unchecked_transaction()?;
    migrate_icmp(&txn)?;
    migrate_usage(&txn)?;
    txn.execute_batch("CREATE TABLE IF NOT EXISTS usage_coverage (host_id TEXT PRIMARY KEY, started_at TEXT NOT NULL, observed_at TEXT NOT NULL); CREATE INDEX IF NOT EXISTS idx_usage_samples_collected ON usage_samples(collected_at); CREATE INDEX IF NOT EXISTS idx_transitions_changed ON transitions(changed_at); CREATE INDEX IF NOT EXISTS idx_usage_history_collected ON usage_history(collected_at);")?;
    txn.execute_batch("CREATE TABLE IF NOT EXISTS host_identity (host_id TEXT PRIMARY KEY, address TEXT NOT NULL, usage_enabled INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS retention_floor (id INTEGER PRIMARY KEY CHECK (id = 1), cutoff TEXT NOT NULL);")?;
    txn.execute_batch("CREATE TABLE IF NOT EXISTS latest_tcp (host_id TEXT NOT NULL, check_id TEXT NOT NULL, address TEXT NOT NULL, port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535), snapshot TEXT NOT NULL, PRIMARY KEY(host_id, check_id));")?;
    super::history::migrate(&txn)?;
    super::jobs::migrate(&txn)?;
    txn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    txn.commit()?;
    Ok(())
}

fn migrate_icmp(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS latest_status (
    host_id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL,
    last_checked_at TEXT,
    last_change_at TEXT,
    latency_ms REAL,
    consecutive_successes INTEGER NOT NULL,
    consecutive_failures INTEGER NOT NULL,
    last_error TEXT
);

CREATE TABLE IF NOT EXISTS transitions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id TEXT NOT NULL,
    previous_status TEXT NOT NULL,
    new_status TEXT NOT NULL,
    changed_at TEXT NOT NULL,
    latency_ms REAL,
    error TEXT,
    backend TEXT NOT NULL,
    reason TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_transitions_host_changed
    ON transitions(host_id, changed_at DESC);
"#,
    )?;
    Ok(())
}

fn migrate_usage(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS latest_usage (
    host_id TEXT PRIMARY KEY NOT NULL,
    collected_at TEXT NOT NULL,
    console_users INTEGER,
    remote_users INTEGER,
    status TEXT NOT NULL,
    error TEXT
);

CREATE TABLE IF NOT EXISTS usage_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id TEXT NOT NULL,
    collected_at TEXT NOT NULL,
    console_users INTEGER,
    remote_users INTEGER,
    status TEXT NOT NULL,
    error TEXT
);

CREATE INDEX IF NOT EXISTS idx_usage_samples_host_collected
    ON usage_samples(host_id, collected_at DESC);

CREATE TABLE IF NOT EXISTS usage_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id TEXT NOT NULL,
    collected_at TEXT NOT NULL,
    console_users INTEGER,
    remote_users INTEGER,
    status TEXT NOT NULL,
    error TEXT
);

CREATE INDEX IF NOT EXISTS idx_usage_history_host_collected
    ON usage_history(host_id, collected_at DESC);
"#,
    )?;
    Ok(())
}
