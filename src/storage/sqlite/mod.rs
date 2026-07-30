mod rows;
mod schema;

use self::{
    rows::{TransitionRow, UsageRow, runtime_state_from_row},
    schema::migrate,
};
use crate::{
    domain::{
        Host, HostFilter, HostRecord, HostRuntimeState, HostUsage, IcmpTransition,
        UsageCollectionStatus, UsageFilter, UsageHistory, UsageReport, UsageSample, UsageSnapshot,
        UsageSummary,
    },
    storage::{HostRepository, IcmpRepository, StorageError, UsageRepository},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

struct Inner {
    usage: HashMap<String, UsageSnapshot>,
    conn: Connection,
    /// When set, `update_usage` deletes any usage_samples row older than
    /// `Utc::now() - retention` for the same host. `None` disables
    /// pruning (used by the in-memory test fixtures).
    usage_sample_retention: Option<std::time::Duration>,
}

/// Pool of read-only SQLite connections. Populated only
/// for file-backed storage — each `:memory:` open creates an independent
/// database, so an in-memory pool can't share state with the writer. Reads on
/// in-memory storage fall back to the writer connection (no concurrency win
/// but tests still work).
type ReaderPool = r2d2::Pool<ReadOnlySqliteConnectionManager>;

struct ReadOnlySqliteConnectionManager {
    path: std::path::PathBuf,
    flags: OpenFlags,
}

impl ReadOnlySqliteConnectionManager {
    fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            flags: OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        }
    }
}

impl r2d2::ManageConnection for ReadOnlySqliteConnectionManager {
    type Connection = Connection;
    type Error = rusqlite::Error;

    fn connect(&self) -> Result<Self::Connection, Self::Error> {
        let conn = Connection::open_with_flags(&self.path, self.flags)?;
        apply_reader_pragmas(&conn).map_err(rusqlite_io_error)?;
        Ok(conn)
    }

    fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        conn.execute_batch("SELECT 1")?;
        Ok(())
    }

    fn has_broken(&self, _conn: &mut Self::Connection) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct SqliteStorage {
    /// Host catalog. Built once in `new()` and never mutated, so reads can
    /// query it without taking any lock — which is what lets `history()` /
    /// `usage_history()` / `usage_samples()` truly bypass the writer Mutex.
    hosts: Arc<RwLock<HashMap<String, Host>>>,
    inner: Arc<Mutex<Inner>>,
    /// `Some` for file-backed storage; `None` for in-memory.
    readers: Option<ReaderPool>,
}

impl SqliteStorage {
    /// Number of read-only connections in the pool. Small constant — enough
    /// for typical API fan-out without piling on file descriptors. Bump if
    /// `/v1/hosts/{id}/history` shows reader contention under load.
    const READER_POOL_SIZE: u32 = 4;

    pub fn open(path: impl AsRef<Path>, hosts: Vec<Host>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        let writer = Connection::open(&path)?;
        apply_pragmas(&writer)?;
        migrate(&writer)?;

        // Reader connections are opened read-only and additionally set to
        // query_only so future read helpers cannot accidentally mutate SQLite.
        let manager = ReadOnlySqliteConnectionManager::new(path);
        let readers = r2d2::Pool::builder()
            .max_size(Self::READER_POOL_SIZE)
            .build(manager)
            .map_err(|err| {
                StorageError::InvalidData(format!("failed to build reader pool: {err}"))
            })?;
        Self::new_initialized(writer, hosts, Some(readers))
    }

    pub fn in_memory(hosts: Vec<Host>) -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        apply_pragmas(&conn)?;
        migrate(&conn)?;
        Self::new_initialized(conn, hosts, None)
    }

    fn new_initialized(
        conn: Connection,
        hosts: Vec<Host>,
        readers: Option<ReaderPool>,
    ) -> Result<Self, StorageError> {
        // Keep only the cache needed to decide whether a usage update should
        // produce a history row. API reads load current state from SQLite.
        let usage_persisted = load_all_latest_usage(&conn)?;
        let mut host_map = HashMap::with_capacity(hosts.len());
        let mut usage = HashMap::with_capacity(hosts.len());
        for host in hosts {
            if let Some(snapshot) = usage_persisted.get(&host.id) {
                usage.insert(host.id.clone(), snapshot.clone());
            }
            host_map.insert(host.id.clone(), host);
        }
        Ok(Self {
            hosts: Arc::new(RwLock::new(host_map)),
            inner: Arc::new(Mutex::new(Inner {
                usage,
                conn,
                usage_sample_retention: None,
            })),
            readers,
        })
    }

    pub fn update_hosts(&self, hosts: Vec<Host>) -> Result<(), StorageError> {
        let host_ids = hosts.iter().map(|host| host.id.clone()).collect::<Vec<_>>();
        let host_map = hosts
            .into_iter()
            .map(|host| (host.id.clone(), host))
            .collect::<HashMap<_, _>>();

        let mut current = self.hosts.write().map_err(|_| StorageError::LockPoisoned)?;
        let mut inner = self.inner.lock().map_err(|_| StorageError::LockPoisoned)?;
        *current = host_map;
        inner.usage.retain(|host_id, _| host_ids.contains(host_id));
        Ok(())
    }

    /// Configure how long `usage_samples` rows are retained. On every
    /// `update_usage`, rows older than `Utc::now() - retention`
    /// are deleted inside the same transaction. Pass `None` to disable
    /// pruning entirely (the default for `in_memory` / `open` until set).
    pub fn set_usage_sample_retention(
        &self,
        retention: Option<std::time::Duration>,
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().map_err(|_| StorageError::LockPoisoned)?;
        inner.usage_sample_retention = retention;
        Ok(())
    }

    async fn with_inner<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Inner) -> Result<T, StorageError> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut inner = inner.lock().map_err(|_| StorageError::LockPoisoned)?;
            f(&mut inner)
        })
        .await
        .map_err(|err| StorageError::TaskJoin(err.to_string()))?
    }

    /// Run `f` on a read-only connection from the r2d2 pool. Falls back to a
    /// brief writer-Mutex acquisition for in-memory storage (no pool there).
    /// DB-heavy reads should prefer this over `with_inner` so they don't park
    /// behind an in-flight write transaction on the writer connection — WAL
    /// gives us writer↔reader concurrency at the SQLite level, but only if
    /// the read path isn't also serialised behind the writer Mutex.
    async fn with_reader<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let readers = self.readers.clone();
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || match readers {
            Some(pool) => {
                let conn = pool.get().map_err(|err| {
                    StorageError::InvalidData(format!("reader pool acquire failed: {err}"))
                })?;
                f(&conn)
            }
            None => {
                let inner = inner.lock().map_err(|_| StorageError::LockPoisoned)?;
                f(&inner.conn)
            }
        })
        .await
        .map_err(|err| StorageError::TaskJoin(err.to_string()))?
    }

    /// Lock-free existence check. `hosts` is read-only after construction so
    /// the read path doesn't touch the writer Mutex at all — the whole point
    /// of the reader pool would be undone otherwise.
    fn ensure_host_exists_sync(&self, host_id: &str) -> Result<(), StorageError> {
        let hosts = self.hosts.read().map_err(|_| StorageError::LockPoisoned)?;
        if hosts.contains_key(host_id) {
            Ok(())
        } else {
            Err(StorageError::NotFound(host_id.to_string()))
        }
    }
}

/// r2d2's `with_init` callback wants a `rusqlite::Error`, so we wrap our
/// internal `StorageError` as a synthetic SqliteFailure with the error
/// rendered in the message slot. The wrapping only fires on init failure
/// (e.g., apply_pragmas couldn't enable WAL), which is itself rare.
fn rusqlite_io_error(err: StorageError) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(err.to_string()),
    )
}

#[async_trait]
impl HostRepository for SqliteStorage {
    async fn hosts(&self, filter: HostFilter) -> Result<Vec<HostRecord>, StorageError> {
        let hosts = self
            .hosts
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .clone();
        self.with_reader(move |conn| sorted_records(conn, &hosts, &filter))
            .await
    }

    async fn host(&self, id: &str) -> Result<Option<HostRecord>, StorageError> {
        let id = id.to_string();
        let hosts = self
            .hosts
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .clone();
        self.with_reader(move |conn| {
            hosts
                .get(&id)
                .map(|host| host_record(conn, host))
                .transpose()
        })
        .await
    }
}

#[async_trait]
impl IcmpRepository for SqliteStorage {
    async fn update_check_result(
        &self,
        host_id: &str,
        state: HostRuntimeState,
        transition: Option<IcmpTransition>,
    ) -> Result<(), StorageError> {
        self.ensure_host_exists_sync(host_id)?;
        let host_id = host_id.to_string();
        self.with_inner(move |inner| {
            let txn = inner.conn.transaction()?;
            upsert_latest_state(&txn, &host_id, &state)?;
            if let Some(event) = &transition {
                insert_transition(&txn, event)?;
            }
            txn.commit()?;
            Ok(())
        })
        .await
    }

    async fn history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<IcmpTransition>, StorageError> {
        self.ensure_host_exists_sync(host_id)?;
        let host_id = host_id.to_string();
        self.with_reader(move |conn| load_history(conn, &host_id, limit))
            .await
    }
}

#[async_trait]
impl UsageRepository for SqliteStorage {
    async fn update_usage(
        &self,
        host_id: &str,
        snapshot: UsageSnapshot,
    ) -> Result<bool, StorageError> {
        self.ensure_host_exists_sync(host_id)?;
        let host_id = host_id.to_string();
        self.with_inner(move |inner| {
            let should_insert_history = inner
                .usage
                .get(&host_id)
                .is_none_or(|previous| snapshot.differs_for_history(previous));
            let retention = inner.usage_sample_retention;
            let txn = inner.conn.transaction()?;
            upsert_latest_usage(&txn, &host_id, &snapshot)?;
            insert_usage_sample(&txn, &host_id, &snapshot)?;
            if should_insert_history {
                insert_usage_history(&txn, &host_id, &snapshot)?;
            }
            if let Some(retention) = retention {
                let cutoff = Utc::now()
                    - chrono::Duration::from_std(retention).map_err(|err| {
                        StorageError::InvalidData(format!(
                            "invalid usage_sample_retention duration: {err}"
                        ))
                    })?;
                txn.execute(
                    "DELETE FROM usage_samples WHERE host_id = ?1 AND collected_at < ?2",
                    params![&host_id, cutoff.to_rfc3339()],
                )?;
            }
            txn.commit()?;
            inner.usage.insert(host_id, snapshot);
            Ok(should_insert_history)
        })
        .await
    }

    async fn usage_history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageHistory>, StorageError> {
        self.ensure_host_exists_sync(host_id)?;
        let host_id = host_id.to_string();
        self.with_reader(move |conn| load_usage_history(conn, &host_id, limit))
            .await
    }

    async fn usage_samples(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageSample>, StorageError> {
        self.ensure_host_exists_sync(host_id)?;
        let host_id = host_id.to_string();
        self.with_reader(move |conn| load_usage_samples(conn, &host_id, limit))
            .await
    }

    async fn usage_report(&self, filter: UsageFilter) -> Result<UsageReport, StorageError> {
        let hosts_by_id = self
            .hosts
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .clone();
        self.with_reader(move |conn| {
            let usage = load_all_latest_usage(conn)?;
            let mut hosts = Vec::new();
            for (host_id, snapshot) in &usage {
                if !matches_usage_filter(conn, host_id, snapshot, &filter)? {
                    continue;
                }
                if let Some(host) = hosts_by_id.get(host_id) {
                    hosts.push(HostUsage {
                        id: host.id.clone(),
                        address: host.address.clone(),
                        name: host.name.clone(),
                        groups: host.groups.clone(),
                        usage: snapshot.clone(),
                    });
                }
            }
            hosts.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(UsageReport {
                summary: usage_summary(hosts.iter().map(|host| &host.usage)),
                hosts,
            })
        })
        .await
    }
}

/// Apply per-connection PRAGMAs. WAL downgrades to `memory` on in-memory
/// databases; any other journal mode means the requested durability/concurrency
/// mode did not take effect.
fn apply_pragmas(conn: &Connection) -> Result<(), StorageError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let journal_mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !matches!(journal_mode.to_ascii_lowercase().as_str(), "wal" | "memory") {
        return Err(StorageError::InvalidData(format!(
            "failed to enable WAL journal mode; sqlite reported {journal_mode:?}"
        )));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn apply_reader_pragmas(conn: &Connection) -> Result<(), StorageError> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "query_only", "ON")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn host_record(conn: &Connection, host: &Host) -> Result<HostRecord, StorageError> {
    let state = load_latest_state(conn, &host.id)?.unwrap_or_default();
    let usage = load_latest_usage(conn, &host.id)?;
    Ok(state.to_record(host, usage))
}

fn sorted_records(
    conn: &Connection,
    hosts: &HashMap<String, Host>,
    filter: &HostFilter,
) -> Result<Vec<HostRecord>, StorageError> {
    let mut states = load_all_latest_states(conn)?;
    let usage = load_all_latest_usage(conn)?;
    let mut records = Vec::new();
    for host in hosts.values() {
        let state = states.remove(&host.id).unwrap_or_default();
        let record = state.to_record(host, usage.get(&host.id).cloned());
        if matches_host_filter(conn, &record, filter)? {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.host.id.cmp(&b.host.id));
    Ok(records)
}

fn matches_host_filter(
    conn: &Connection,
    record: &HostRecord,
    filter: &HostFilter,
) -> Result<bool, StorageError> {
    if let Some(status) = &filter.icmp_status
        && &record.state.status != status
    {
        return Ok(false);
    }
    if let Some(group) = &filter.group
        && !record
            .host
            .groups
            .iter()
            .any(|candidate| candidate == group)
    {
        return Ok(false);
    }
    if !filter.metadata.is_empty()
        && !filter.metadata.iter().all(|(key, expected)| {
            record
                .host
                .metadata
                .get(key)
                .is_some_and(|actual| metadata_value_matches(actual, expected))
        })
    {
        return Ok(false);
    }
    if !filter.usage.is_empty() {
        let Some(snapshot) = &record.usage else {
            return Ok(false);
        };
        if !matches_usage_filter(conn, &record.host.id, snapshot, &filter.usage)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn metadata_value_matches(actual: &Value, expected: &str) -> bool {
    match actual {
        Value::String(value) => value == expected,
        Value::Bool(value) => value.to_string() == expected,
        Value::Number(value) => value.to_string() == expected,
        _ => false,
    }
}

fn usage_summary<'a>(snapshots: impl IntoIterator<Item = &'a UsageSnapshot>) -> UsageSummary {
    let mut summary = UsageSummary {
        hosts_reporting: 0,
        hosts_with_errors: 0,
        console_users: 0,
        remote_users: 0,
    };
    for snapshot in snapshots {
        summary.hosts_reporting += 1;
        match snapshot.status {
            UsageCollectionStatus::Ok => {
                summary.console_users += snapshot.console_users.unwrap_or(0);
                summary.remote_users += snapshot.remote_users.unwrap_or(0);
            }
            UsageCollectionStatus::Failed => {
                summary.hosts_with_errors += 1;
            }
        }
    }
    summary
}

fn matches_usage_filter(
    conn: &Connection,
    host_id: &str,
    snapshot: &UsageSnapshot,
    filter: &UsageFilter,
) -> Result<bool, StorageError> {
    if let Some(status) = &filter.status
        && &snapshot.status != status
    {
        return Ok(false);
    }
    let history = SqliteInactivityHistory { conn, host_id };
    if let Some(duration) = filter.inactive_console_for {
        let inactive = crate::domain::evaluate_inactivity(
            snapshot,
            duration,
            filter.now,
            crate::domain::ActivityPredicate::ConsoleOnly,
            &history,
        )
        .map_err(map_inactivity_error)?;
        if !inactive {
            return Ok(false);
        }
    }
    if let Some(duration) = filter.no_users_for {
        let inactive = crate::domain::evaluate_inactivity(
            snapshot,
            duration,
            filter.now,
            crate::domain::ActivityPredicate::AnyUser,
            &history,
        )
        .map_err(map_inactivity_error)?;
        if !inactive {
            return Ok(false);
        }
    }
    Ok(true)
}

fn map_inactivity_error(err: crate::domain::InactivityError<StorageError>) -> StorageError {
    match err {
        crate::domain::InactivityError::InvalidDuration(msg) => {
            StorageError::InvalidData(format!("invalid inactivity duration: {msg}"))
        }
        crate::domain::InactivityError::Storage(e) => e,
    }
}

/// SQL implementation of the [`InactivityHistory`](crate::domain::InactivityHistory)
/// trait — every method is a single SELECT against `usage_history`.
struct SqliteInactivityHistory<'a> {
    conn: &'a Connection,
    host_id: &'a str,
}

impl crate::domain::InactivityHistory for SqliteInactivityHistory<'_> {
    type Error = StorageError;

    fn state_at_or_before(
        &self,
        cutoff: DateTime<Utc>,
        predicate: crate::domain::ActivityPredicate,
    ) -> Result<Option<crate::domain::StateAtCutoff>, StorageError> {
        let pred = activity_sql(predicate);
        let sql = format!(
            r#"
SELECT status, CASE WHEN {pred} THEN 1 ELSE 0 END AS activity
FROM usage_history
WHERE host_id = ?1 AND collected_at <= ?2
ORDER BY collected_at DESC, id DESC
LIMIT 1
"#
        );
        let row: Option<(String, i64)> = self
            .conn
            .query_row(&sql, params![self.host_id, cutoff.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?;
        Ok(row.map(|(status, activity)| match status.as_str() {
            "ok" => crate::domain::StateAtCutoff::Ok {
                activity_matched: activity != 0,
            },
            _ => crate::domain::StateAtCutoff::Unknown,
        }))
    }

    fn window_breaks_inactivity(
        &self,
        cutoff: DateTime<Utc>,
        predicate: crate::domain::ActivityPredicate,
    ) -> Result<bool, StorageError> {
        let pred = activity_sql(predicate);
        let sql = format!(
            r#"
SELECT EXISTS (
    SELECT 1
    FROM usage_history
    WHERE host_id = ?1
      AND collected_at > ?2
      AND (status != 'ok' OR ({pred}))
)
"#
        );
        let exists: i64 =
            self.conn
                .query_row(&sql, params![self.host_id, cutoff.to_rfc3339()], |row| {
                    row.get(0)
                })?;
        Ok(exists != 0)
    }
}

fn activity_sql(predicate: crate::domain::ActivityPredicate) -> &'static str {
    match predicate {
        crate::domain::ActivityPredicate::ConsoleOnly => "console_users > 0",
        crate::domain::ActivityPredicate::AnyUser => "console_users > 0 OR remote_users > 0",
    }
}

fn upsert_latest_usage(
    conn: &Connection,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    conn.execute(
        r#"
INSERT INTO latest_usage (
    host_id, collected_at, console_users, remote_users, status, error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(host_id) DO UPDATE SET
    collected_at = excluded.collected_at,
    console_users = excluded.console_users,
    remote_users = excluded.remote_users,
    status = excluded.status,
    error = excluded.error
"#,
        params![
            host_id,
            snapshot.collected_at.to_rfc3339(),
            snapshot.console_users,
            snapshot.remote_users,
            snapshot.status.as_str(),
            snapshot.error,
        ],
    )?;
    Ok(())
}

fn insert_usage_sample(
    conn: &Connection,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    insert_usage_row(conn, UsageTable::Samples, host_id, snapshot)
}

fn insert_usage_history(
    conn: &Connection,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    insert_usage_row(conn, UsageTable::History, host_id, snapshot)
}

#[derive(Clone, Copy)]
enum UsageTable {
    Samples,
    History,
}

impl UsageTable {
    fn name(self) -> &'static str {
        match self {
            Self::Samples => "usage_samples",
            Self::History => "usage_history",
        }
    }
}

fn insert_usage_row(
    conn: &Connection,
    table: UsageTable,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    let table = table.name();
    let sql = format!(
        r#"
INSERT INTO {table} (
    host_id, collected_at, console_users, remote_users, status, error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
"#
    );
    conn.execute(
        &sql,
        params![
            host_id,
            snapshot.collected_at.to_rfc3339(),
            snapshot.console_users,
            snapshot.remote_users,
            snapshot.status.as_str(),
            snapshot.error,
        ],
    )?;
    Ok(())
}

fn load_usage_history(
    conn: &Connection,
    host_id: &str,
    limit: usize,
) -> Result<Vec<UsageHistory>, StorageError> {
    load_usage_rows(conn, UsageTable::History, host_id, limit)
}

fn load_usage_samples(
    conn: &Connection,
    host_id: &str,
    limit: usize,
) -> Result<Vec<UsageSample>, StorageError> {
    load_usage_rows(conn, UsageTable::Samples, host_id, limit)
}

fn load_usage_rows<T>(
    conn: &Connection,
    table: UsageTable,
    host_id: &str,
    limit: usize,
) -> Result<Vec<T>, StorageError>
where
    T: TryFrom<UsageRow, Error = StorageError>,
{
    let table = table.name();
    let sql = format!(
        r#"
SELECT id, host_id, collected_at, console_users, remote_users, status, error
FROM {table}
WHERE host_id = ?1
ORDER BY collected_at DESC, id DESC
LIMIT ?2
"#
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![host_id, limit as i64], |row| {
        Ok(UsageRow {
            id: row.get(0)?,
            host_id: row.get(1)?,
            collected_at: row.get(2)?,
            console_users: row.get(3)?,
            remote_users: row.get(4)?,
            status: row.get(5)?,
            error: row.get(6)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?.try_into()?);
    }
    Ok(events)
}

fn upsert_latest_state(
    conn: &Connection,
    host_id: &str,
    state: &HostRuntimeState,
) -> Result<(), StorageError> {
    conn.execute(
        r#"
INSERT INTO latest_status (
    host_id, status, last_checked_at, last_change_at, latency_ms,
    consecutive_successes, consecutive_failures, last_error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
ON CONFLICT(host_id) DO UPDATE SET
    status = excluded.status,
    last_checked_at = excluded.last_checked_at,
    last_change_at = excluded.last_change_at,
    latency_ms = excluded.latency_ms,
    consecutive_successes = excluded.consecutive_successes,
    consecutive_failures = excluded.consecutive_failures,
    last_error = excluded.last_error
"#,
        params![
            host_id,
            state.status.as_str(),
            state.last_checked_at.map(|ts| ts.to_rfc3339()),
            state.last_change_at.map(|ts| ts.to_rfc3339()),
            state.latency.map(crate::domain::duration_ms),
            state.consecutive_successes,
            state.consecutive_failures,
            state.last_error,
        ],
    )?;
    Ok(())
}

fn load_latest_state(
    conn: &Connection,
    host_id: &str,
) -> Result<Option<HostRuntimeState>, StorageError> {
    let row = conn
        .query_row(
            r#"
SELECT status, last_checked_at, last_change_at, latency_ms,
       consecutive_successes, consecutive_failures, last_error
FROM latest_status
WHERE host_id = ?1
"#,
            params![host_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(status, last_checked_at, last_change_at, latency_ms, succ, fail, last_err)| {
            runtime_state_from_row(
                status,
                last_checked_at,
                last_change_at,
                latency_ms,
                succ,
                fail,
                last_err,
            )
        },
    )
    .transpose()
}

/// Bulk-load every persisted latest_status row in one SELECT — one round-trip
/// instead of N. Hosts without a row are absent from the map; the caller
/// supplies the default.
fn load_all_latest_states(
    conn: &Connection,
) -> Result<HashMap<String, HostRuntimeState>, StorageError> {
    let mut stmt = conn.prepare(
        r#"
SELECT host_id, status, last_checked_at, last_change_at, latency_ms,
       consecutive_successes, consecutive_failures, last_error
FROM latest_status
"#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, u32>(5)?,
            row.get::<_, u32>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (host_id, status, last_checked_at, last_change_at, latency_ms, succ, fail, last_err) =
            row?;
        let state = runtime_state_from_row(
            status,
            last_checked_at,
            last_change_at,
            latency_ms,
            succ,
            fail,
            last_err,
        )?;
        map.insert(host_id, state);
    }
    Ok(map)
}

fn load_latest_usage(
    conn: &Connection,
    host_id: &str,
) -> Result<Option<UsageSnapshot>, StorageError> {
    let row = conn
        .query_row(
            r#"
SELECT host_id, collected_at, console_users, remote_users, status, error
FROM latest_usage
WHERE host_id = ?1
"#,
            params![host_id],
            |row| {
                Ok(UsageRow {
                    id: None,
                    host_id: row.get(0)?,
                    collected_at: row.get(1)?,
                    console_users: row.get(2)?,
                    remote_users: row.get(3)?,
                    status: row.get(4)?,
                    error: row.get(5)?,
                })
            },
        )
        .optional()?;
    row.map(TryInto::try_into).transpose()
}

/// Bulk-load every persisted latest_usage row in one SELECT.
fn load_all_latest_usage(
    conn: &Connection,
) -> Result<HashMap<String, UsageSnapshot>, StorageError> {
    let mut stmt = conn.prepare(
        r#"
SELECT host_id, collected_at, console_users, remote_users, status, error
FROM latest_usage
"#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UsageRow {
            id: None,
            host_id: row.get(0)?,
            collected_at: row.get(1)?,
            console_users: row.get(2)?,
            remote_users: row.get(3)?,
            status: row.get(4)?,
            error: row.get(5)?,
        })
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let usage_row = row?;
        let host_id = usage_row.host_id.clone();
        let snapshot: UsageSnapshot = usage_row.try_into()?;
        map.insert(host_id, snapshot);
    }
    Ok(map)
}

fn insert_transition(conn: &Connection, event: &IcmpTransition) -> Result<(), StorageError> {
    conn.execute(
        r#"
INSERT INTO transitions (
    host_id, previous_status, new_status, changed_at, latency_ms, error, backend, reason
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
        params![
            event.host_id,
            event.previous_status.as_str(),
            event.new_status.as_str(),
            event.changed_at.to_rfc3339(),
            event.latency_ms,
            event.error,
            event.backend,
            event.reason,
        ],
    )?;
    Ok(())
}

fn load_history(
    conn: &Connection,
    host_id: &str,
    limit: usize,
) -> Result<Vec<IcmpTransition>, StorageError> {
    let mut stmt = conn.prepare(
        r#"
SELECT id, host_id, previous_status, new_status, changed_at, latency_ms, error, backend, reason
FROM transitions
WHERE host_id = ?1
ORDER BY changed_at DESC, id DESC
LIMIT ?2
"#,
    )?;
    let rows = stmt.query_map(params![host_id, limit as i64], |row| {
        Ok(TransitionRow {
            id: row.get(0)?,
            host_id: row.get(1)?,
            previous_status: row.get(2)?,
            new_status: row.get(3)?,
            changed_at: row.get(4)?,
            latency_ms: row.get(5)?,
            error: row.get(6)?,
            backend: row.get(7)?,
            reason: row.get(8)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?.try_into()?);
    }
    Ok(events)
}

#[cfg(test)]
mod tests;
