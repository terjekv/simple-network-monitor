//! Persistent, bounded maintenance jobs. The runner serializes all executions.
use super::*;
use crate::domain::maintenance::*;

pub(super) fn migrate(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS maintenance_jobs (
        id TEXT PRIMARY KEY, status TEXT NOT NULL, last_started_ms INTEGER, last_success_ms INTEGER,
        next_run_ms INTEGER NOT NULL, duration_ms INTEGER, work_done INTEGER NOT NULL DEFAULT 0,
        failures INTEGER NOT NULL DEFAULT 0, message TEXT
    ); CREATE TABLE IF NOT EXISTS maintenance_runs (
        id INTEGER PRIMARY KEY, job_id TEXT NOT NULL, started_ms INTEGER NOT NULL,
        finished_ms INTEGER NOT NULL, status TEXT NOT NULL, work_done INTEGER NOT NULL, message TEXT
    ); CREATE INDEX IF NOT EXISTS maintenance_run_time ON maintenance_runs(finished_ms);",
    )?;
    Ok(())
}

pub(super) fn register_jobs(
    conn: &Connection,
    config: &MaintenanceConfig,
) -> Result<(), StorageError> {
    let now = Utc::now().timestamp_millis();
    for kind in JobKind::ALL {
        conn.execute("INSERT INTO maintenance_jobs(id,status,next_run_ms) VALUES (?1,'pending',?2) ON CONFLICT(id) DO UPDATE SET next_run_ms=MIN(next_run_ms,?2+?3)",params![kind.id(),now,kind.interval(config).as_millis() as i64])?;
    }
    Ok(())
}

impl SqliteStorage {
    pub fn configure_maintenance(
        &self,
        history: HistoryConfig,
        maintenance: MaintenanceConfig,
    ) -> Result<(), StorageError> {
        history
            .validate()
            .map_err(|e| StorageError::InvalidData(e.into()))?;
        maintenance
            .validate()
            .map_err(|e| StorageError::InvalidData(e.into()))?;
        let mut inner = self.inner.lock().map_err(|_| StorageError::LockPoisoned)?;
        let mut policy = self
            .history_policy
            .write()
            .map_err(|_| StorageError::LockPoisoned)?;
        let tx = inner.conn.transaction()?;
        register_jobs(&tx, &maintenance)?;
        tx.commit()?;
        *policy = history.clone();
        inner.history_config = history;
        inner.maintenance_config = maintenance;
        Ok(())
    }

    pub async fn recover_maintenance(&self) -> Result<(), StorageError> {
        self.with_inner(|inner| {
            inner.conn.execute(concat!("UPDATE maintenance_jobs SET status='interrupted', message='Interrupted by service ", "restart; retry scheduled', next_run_ms=?1 WHERE status='running'"), [Utc::now().timestamp_millis()])?;
            Ok(())
        })
        .await
    }

    /// Claim one due job. A persisted claim prevents overlap even if two runners are accidentally started.
    pub async fn run_due_maintenance(&self) -> Result<bool, StorageError> {
        let Ok(_run) = self.maintenance_admission.clone().try_acquire_owned() else {
            return Ok(false);
        };
        let claimed = self
            .with_inner(|inner| {
                let now = Utc::now().timestamp_millis();
                let tx = inner.conn.transaction()?;
                let running: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM maintenance_jobs WHERE status='running')", [], |r| r.get(0))?;
                if running {
                    // An earlier operation finished but its completion transaction failed.
                    // No other in-process job owns admission, so retry the durable work safely.
                    tx.execute("UPDATE maintenance_jobs SET status='error',failures=failures+1,next_run_ms=?1,message='Completion could not be persisted; retry scheduled' WHERE status='running'", [now+5000])?;
                }
                let id: Option<String> = tx.query_row("SELECT id FROM maintenance_jobs WHERE next_run_ms<=?1 ORDER BY next_run_ms,id LIMIT 1", [now], |r| r.get(0)).optional()?;
                let kind = id.and_then(|id| JobKind::ALL.into_iter().find(|k| k.id() == id));
                if let Some(kind) = kind {
                    tx.execute("UPDATE maintenance_jobs SET status='running',last_started_ms=?2,message=NULL WHERE id=?1", params![kind.id(), now])?;
                }
                tx.commit()?;
                Ok(kind.map(|kind| (kind, now)))
            })
            .await?;
        let Some((kind, started)) = claimed else {
            return Ok(false);
        };
        let clock = std::time::Instant::now();
        let result = self.perform_job(kind).await;
        let duration = clock.elapsed().as_millis().min(i64::MAX as u128) as i64;
        self.with_inner(move |inner| {
            let now = Utc::now().timestamp_millis();
            let (status, work, delay, message) = match result {
                Ok((work, more, message)) => ("ok", work, if more { 1000 } else { kind.interval(&inner.maintenance_config).as_millis() as i64 }, message),
                Err(err) => {
                    tracing::error!(job=kind.id(),%err,"maintenance job failed");
                    let failures: u32 = inner.conn.query_row("SELECT failures FROM maintenance_jobs WHERE id=?1", [kind.id()], |r| r.get(0))?;
                    ("error", 0, (5000_i64 * 2_i64.pow(failures.min(9))).min(3600000), Some("Database maintenance failed; see server logs. Retry scheduled.".into()))
                }
            };
            let tx = inner.conn.transaction()?;
            tx.execute(concat!("UPDATE maintenance_jobs SET status=?2,last_success_ms=CASE WHEN ?2='ok' THEN ?3 ELSE ", "last_success_ms END,next_run_ms=?4,duration_ms=?5,work_done=?6,failures=CASE WHEN ", "?2='ok' THEN 0 ELSE failures+1 END,message=?7 WHERE id=?1"), params![kind.id(), status, now, now + delay, duration, work as i64, message])?;
            tx.execute("INSERT INTO maintenance_runs(job_id,started_ms,finished_ms,status,work_done,message) VALUES (?1,?2,?3,?4,?5,?6)", params![kind.id(), started, now, status, work as i64, message])?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    async fn perform_job(
        &self,
        kind: JobKind,
    ) -> Result<(u64, bool, Option<String>), StorageError> {
        // The legacy event/usage tables retain their existing cutoff-anchor semantics.
        if matches!(kind, JobKind::Retention) {
            self.prune_history(Utc::now()).await?;
        }
        self.with_inner(move |inner| {
            let now = Utc::now().timestamp_millis();
            let config = &inner.maintenance_config;
            match kind {
                JobKind::Rollup | JobKind::Retention => {
                    let tx = inner.conn.transaction()?;
                    let (work, more) = if matches!(kind, JobKind::Rollup) { history::rollup(&tx, now, &inner.history_config, config.batch_size)? } else { history::prune(&tx, now, &inner.history_config, inner.usage_sample_retention.unwrap_or(Duration::from_secs(30 * 86400)), config.batch_size)? };
                    tx.commit()?;
                    Ok((work, more, None))
                }
                JobKind::Reclaim => {
                    let mode: u32 = inner.conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
                    if mode != 2 {
                        return Ok((0, false, Some("Run --compact-database with the service stopped to enable incremental reclamation".into())));
                    }
                    let free: i64 = inner.conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
                    let pages: i64 = inner.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
                    let size: i64 = inner.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
                    if free * size < config.reclaim_min_bytes as i64 || free * 100 < pages * i64::from(config.reclaim_free_percent) {
                        return Ok((0, false, Some("Reusable space below reclamation thresholds".into())));
                    }
                    let mut stmt = inner.conn.prepare(&format!("PRAGMA incremental_vacuum({})", config.reclaim_pages))?;
                    let mut rows = stmt.query([])?;
                    while rows.next()?.is_some() {}
                    let after: i64 = inner.conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
                    Ok((free.saturating_sub(after) as u64, false, None))
                }
                JobKind::Optimize => {
                    inner.conn.execute_batch("PRAGMA optimize")?;
                    Ok((0, false, None))
                }
                JobKind::Checkpoint => {
                    let (_, pages, done): (i64, i64, i64) = inner.conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                    Ok((done.max(0) as u64, false, Some(format!("{done} of {pages} WAL pages checkpointed; automatic checkpoints remain enabled"))))
                }
                JobKind::JobHistory => {
                    let work = inner.conn.execute("DELETE FROM maintenance_runs WHERE id IN (SELECT id FROM maintenance_runs WHERE finished_ms<?1 LIMIT ?2)", params![now - 30 * 86400000_i64, config.batch_size as i64])?;
                    Ok((work as u64, work == config.batch_size, None))
                }
            }
        })
        .await
    }

    pub async fn maintenance_status(&self) -> Result<MaintenanceStatus, StorageError> {
        let wal_bytes = self
            .database_path
            .as_ref()
            .and_then(|p| std::fs::metadata(format!("{}-wal", p.display())).ok())
            .map_or(0, |m| m.len());
        self.with_reader(move |conn| {
            let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
            let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            let mode: u32 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
            let (pending, oldest): (i64, Option<i64>) = conn.query_row("SELECT COUNT(*),MIN(cursor_ms) FROM history_spans", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let jobs = conn
                .prepare("SELECT id,status,last_started_ms,last_success_ms,next_run_ms,duration_ms,work_done,failures,message FROM maintenance_jobs ORDER BY id")?
                .query_map([], |r| {
                    Ok(MaintenanceJobStatus {
                        id: r.get(0)?,
                        status: r.get(1)?,
                        last_started_ms: r.get(2)?,
                        last_success_ms: r.get(3)?,
                        next_run_ms: r.get(4)?,
                        duration_ms: r.get(5)?,
                        work_done: r.get::<_, i64>(6)? as u64,
                        failures: r.get(7)?,
                        message: r.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(MaintenanceStatus {
                database: DatabaseStatus { allocated_bytes: (pages * size) as u64, reusable_bytes: (free * size) as u64, wal_bytes, incremental_vacuum: mode == 2, pending_spans: pending as u64, oldest_pending_ms: oldest },
                jobs,
            })
        })
        .await
    }

    /// Offline only: takes the same process lock as the running server, without changing identities.
    pub fn compact_database(path: impl AsRef<Path>) -> Result<(), StorageError> {
        if !path.as_ref().is_file() {
            return Err(StorageError::InvalidData("database does not exist".into()));
        }
        let _lock = database_lock(path.as_ref())?;
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA busy_timeout=5000; PRAGMA auto_vacuum=INCREMENTAL; VACUUM; PRAGMA optimize;",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn runs_due_jobs_and_persists_next_run() {
        let storage = SqliteStorage::in_memory(vec![]).unwrap();
        storage
            .configure_maintenance(Default::default(), Default::default())
            .unwrap();
        for _ in 0..6 {
            assert!(storage.run_due_maintenance().await.unwrap());
        }
        assert!(!storage.run_due_maintenance().await.unwrap());
        let status = storage.maintenance_status().await.unwrap();
        assert_eq!(status.jobs.len(), 6);
        assert!(
            status
                .jobs
                .iter()
                .all(|job| job.status == "ok" && job.last_success_ms.is_some())
        );
    }
    #[tokio::test]
    async fn failed_job_retries_without_stalling_other_jobs() {
        let storage = SqliteStorage::in_memory(vec![]).unwrap();
        storage
            .configure_maintenance(Default::default(), Default::default())
            .unwrap();
        storage
            .inner
            .lock()
            .unwrap()
            .conn
            .execute_batch("DROP TABLE history_spans;")
            .unwrap();
        assert!(storage.run_due_maintenance().await.unwrap());
        let inner = storage.inner.lock().unwrap();
        let (status, failures, next): (String, u32, i64) = inner.conn.query_row("SELECT status,failures,next_run_ms FROM maintenance_jobs WHERE id='history_rollup'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(status, "error");
        assert_eq!(failures, 1);
        assert!(next > Utc::now().timestamp_millis());
    }
    #[tokio::test]
    async fn restart_recovers_an_interrupted_claim() {
        let storage = SqliteStorage::in_memory(vec![]).unwrap();
        storage
            .configure_maintenance(Default::default(), Default::default())
            .unwrap();
        storage
            .inner
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE maintenance_jobs SET status='running' WHERE id='history_rollup'",
                [],
            )
            .unwrap();
        storage.recover_maintenance().await.unwrap();
        assert!(storage.run_due_maintenance().await.unwrap());
    }
    #[test]
    fn existing_database_requires_explicit_offline_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("monitor.db");
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE original(value TEXT); INSERT INTO original VALUES ('kept');",
        )
        .unwrap();
        drop(old);
        let storage = SqliteStorage::open(&path, vec![]).unwrap();
        assert_eq!(
            storage
                .inner
                .lock()
                .unwrap()
                .conn
                .query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(SqliteStorage::compact_database(&path).is_err());
        assert!(SqliteStorage::open(&path, vec![]).is_err());
        drop(storage);
        SqliteStorage::compact_database(&path).unwrap();
        let storage = SqliteStorage::open(&path, vec![]).unwrap();
        let inner = storage.inner.lock().unwrap();
        assert_eq!(
            inner
                .conn
                .query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            inner
                .conn
                .query_row("SELECT value FROM original", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "kept"
        );
    }
    #[test]
    fn fresh_database_enables_incremental_vacuum() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("monitor.db");
        let storage = SqliteStorage::open(&path, vec![]).unwrap();
        assert_eq!(
            storage
                .inner
                .lock()
                .unwrap()
                .conn
                .query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    #[tokio::test]
    async fn failed_completion_record_does_not_leave_scheduler_stuck() {
        let storage = SqliteStorage::in_memory(vec![]).unwrap();
        storage
            .configure_maintenance(Default::default(), Default::default())
            .unwrap();
        storage.inner.lock().unwrap().conn.execute_batch("CREATE TRIGGER fail_completion BEFORE UPDATE ON maintenance_jobs WHEN NEW.status='ok' BEGIN SELECT RAISE(ABORT,'fake completion failure'); END;").unwrap();
        assert!(storage.run_due_maintenance().await.is_err());
        storage
            .inner
            .lock()
            .unwrap()
            .conn
            .execute_batch("DROP TRIGGER fail_completion;")
            .unwrap();
        assert!(storage.run_due_maintenance().await.unwrap());
        let status = storage.maintenance_status().await.unwrap();
        assert!(
            status
                .jobs
                .iter()
                .any(|j| j.status == "error" && j.failures == 1)
        );
        assert!(status.jobs.iter().any(|j| j.status == "ok"));
        assert!(!status.jobs.iter().any(|j| j.status == "running"));
    }
}
