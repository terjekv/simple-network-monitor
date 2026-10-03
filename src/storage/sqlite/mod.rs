mod maintenance;
mod queries;
mod rows;
mod schema;
mod tcp;
use maintenance::*;
use queries::*;

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
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

struct Inner {
    generation: u64,
    history_retention: Duration,
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
    /// Immutable catalog snapshots are replaced atomically during reload.
    hosts: Arc<RwLock<Arc<BTreeMap<String, Host>>>>,
    read_admission: Arc<tokio::sync::Semaphore>,
    write_admission: Arc<tokio::sync::Semaphore>,
    write_generation: Option<u64>,
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
        sync_identities(&conn, &hosts)?;
        let usage_persisted = load_all_latest_usage(&conn)?;
        let mut host_map = BTreeMap::new();
        let mut usage = HashMap::with_capacity(hosts.len());
        for host in hosts {
            if let Some(snapshot) = usage_persisted.get(&host.id) {
                usage.insert(host.id.clone(), snapshot.clone());
            }
            host_map.insert(host.id.clone(), host);
        }
        Ok(Self {
            hosts: Arc::new(RwLock::new(Arc::new(host_map))),
            read_admission: Arc::new(tokio::sync::Semaphore::new(Self::READER_POOL_SIZE as usize)),
            write_admission: Arc::new(tokio::sync::Semaphore::new(1)),
            write_generation: None,
            inner: Arc::new(Mutex::new(Inner {
                generation: 0,
                history_retention: Duration::from_secs(365 * 86400),
                usage,
                conn,
                usage_sample_retention: None,
            })),
            readers,
        })
    }

    pub fn monitor_generation(&self) -> Result<Self, StorageError> {
        let mut storage = self.clone();
        storage.write_generation = Some(
            self.inner
                .lock()
                .map_err(|_| StorageError::LockPoisoned)?
                .generation,
        );
        Ok(storage)
    }

    pub fn set_history_retention(&self, duration: Duration) -> Result<(), StorageError> {
        crate::domain::validation::validate_duration(duration)
            .map_err(|err| StorageError::InvalidData(err.into()))?;
        self.inner
            .lock()
            .map_err(|_| StorageError::LockPoisoned)?
            .history_retention = duration;
        Ok(())
    }

    /// A bounded maintenance batch also covers hosts that no longer receive polls.
    pub async fn prune_history(&self, now: DateTime<Utc>) -> Result<usize, StorageError> {
        self.with_inner(move |inner| {
            let history_cutoff = checked_cutoff(now, inner.history_retention)?;
            let txn = inner.conn.transaction()?;
            let mut removed = 0;
            if let Some(retention) = inner.usage_sample_retention {
                removed += txn.execute("DELETE FROM usage_samples WHERE id IN (SELECT id FROM usage_samples WHERE collected_at < ?1 LIMIT 5000)", [checked_cutoff(now, retention)?])?;
            }
            removed += txn.execute("DELETE FROM transitions WHERE id IN (SELECT id FROM transitions WHERE changed_at < ?1 LIMIT 5000)", [&history_cutoff])?;
            // Keep one cutoff anchor for a still-configured host's inactivity history.
            removed += txn.execute("DELETE FROM usage_history WHERE id IN (SELECT h.id FROM usage_history h WHERE h.collected_at < ?1 AND (h.host_id NOT IN (SELECT host_id FROM latest_usage) OR h.id != (SELECT a.id FROM usage_history a WHERE a.host_id = h.host_id AND a.collected_at < ?1 ORDER BY a.collected_at DESC, a.id DESC LIMIT 1)) LIMIT 5000)", [&history_cutoff])?;
            txn.execute("INSERT INTO retention_floor VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET cutoff = MAX(retention_floor.cutoff, excluded.cutoff)", [&history_cutoff])?;
            txn.commit()?;
            Ok(removed)
        }).await
    }

    pub fn update_hosts(&self, hosts: Vec<Host>) -> Result<(), StorageError> {
        let host_ids = hosts
            .iter()
            .map(|host| host.id.clone())
            .collect::<HashSet<_>>();
        if host_ids.len() != hosts.len() {
            return Err(StorageError::InvalidData("duplicate host ID".into()));
        }
        let host_map = hosts
            .into_iter()
            .map(|host| (host.id.clone(), host))
            .collect::<BTreeMap<_, _>>();

        let mut current = self.hosts.write().map_err(|_| StorageError::LockPoisoned)?;
        let mut inner = self.inner.lock().map_err(|_| StorageError::LockPoisoned)?;
        sync_identities(&inner.conn, &host_map.values().cloned().collect::<Vec<_>>())?;
        inner.generation += 1;
        inner.usage.retain(|id, _| {
            current
                .get(id)
                .zip(host_map.get(id))
                .is_some_and(|(old, new)| old.address == new.address)
        });
        *current = Arc::new(host_map);
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
        if let Some(duration) = retention {
            crate::domain::validation::validate_duration(duration)
                .map_err(|err| StorageError::InvalidData(err.into()))?;
        }
        let mut inner = self.inner.lock().map_err(|_| StorageError::LockPoisoned)?;
        inner.usage_sample_retention = retention;
        Ok(())
    }

    async fn with_inner<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Inner) -> Result<T, StorageError> + Send + 'static,
    {
        let permit = Arc::clone(&self.write_admission)
            .acquire_owned()
            .await
            .map_err(|_| StorageError::Busy)?;
        let inner = Arc::clone(&self.inner);
        let generation = self.write_generation;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut inner = inner.lock().map_err(|_| StorageError::LockPoisoned)?;
            if generation.is_some_and(|expected| expected != inner.generation) {
                return Err(StorageError::StaleGeneration);
            }
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
        let permit = Arc::clone(&self.read_admission)
            .try_acquire_owned()
            .map_err(|_| StorageError::Busy)?;
        let readers = self.readers.clone();
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let read = |conn: &Connection| {
                let txn = conn.unchecked_transaction()?;
                let result = f(&txn)?;
                txn.commit()?;
                Ok(result)
            };
            match readers {
                Some(pool) => {
                    let conn = pool.get().map_err(|err| {
                        StorageError::InvalidData(format!("reader pool acquire failed: {err}"))
                    })?;
                    read(&conn)
                }
                None => {
                    let inner = inner.lock().map_err(|_| StorageError::LockPoisoned)?;
                    read(&inner.conn)
                }
            }
        })
        .await
        .map_err(|err| StorageError::TaskJoin(err.to_string()))?
    }

    /// Check the current catalog under its read lock without taking the writer mutex.
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

    async fn hosts_page(
        &self,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<HostRecord>, StorageError> {
        use std::ops::Bound::{Excluded, Unbounded};
        let hosts = self
            .hosts
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .clone();
        self.with_reader(move |conn| {
            let start = after.map_or(Unbounded, Excluded);
            hosts
                .range((start, Unbounded))
                .take(limit)
                .map(|(_, host)| host_record(conn, host))
                .collect()
        })
        .await
    }

    async fn host(&self, id: &str) -> Result<Option<HostRecord>, StorageError> {
        let host = self
            .hosts
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .get(id)
            .cloned();
        self.with_reader(move |conn| host.map(|host| host_record(conn, &host)).transpose())
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
        snapshot
            .validate()
            .map_err(|err| StorageError::InvalidData(err.into()))?;
        let host = self
            .hosts
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .get(host_id)
            .cloned()
            .ok_or_else(|| StorageError::NotFound(host_id.into()))?;
        let host_id = host_id.to_string();
        self.with_inner(move |inner| {
            let should_insert_history = inner
                .usage
                .get(&host_id)
                .is_none_or(|previous| snapshot.differs_for_history(previous));
            let retention = inner.usage_sample_retention;
            let txn = inner.conn.transaction()?;
            update_coverage(&txn, &host, &snapshot)?;
            upsert_latest_usage(&txn, &host_id, &snapshot)?;
            insert_usage_sample(&txn, &host_id, &snapshot)?;
            if should_insert_history {
                insert_usage_history(&txn, &host_id, &snapshot)?;
            }
            if let Some(retention) = retention {
                let cutoff = checked_cutoff(Utc::now(), retention)?;
                txn.execute(
                    "DELETE FROM usage_samples WHERE host_id = ?1 AND collected_at < ?2",
                    params![&host_id, cutoff],
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
                let Some(host) = hosts_by_id.get(host_id) else {
                    continue;
                };
                if !matches_usage_filter(conn, host, snapshot, &filter)? {
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

#[cfg(test)]
mod tests;
