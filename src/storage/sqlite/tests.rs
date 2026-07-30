use super::*;
use crate::domain::{HostStatus, IcmpTransition};
use chrono::Utc;

fn host() -> Host {
    Host {
        id: "r1".into(),
        address: "192.0.2.1".into(),
        name: "Router 1".into(),
        groups: vec!["core".into()],
        metadata: Default::default(),
        modules: Default::default(),
    }
}

#[tokio::test]
async fn stores_latest_state_and_transition_history() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let state = HostRuntimeState {
        status: HostStatus::Down,
        last_checked_at: Some(Utc::now()),
        last_change_at: Some(Utc::now()),
        consecutive_failures: 2,
        last_error: Some("timeout".into()),
        ..Default::default()
    };
    let event = IcmpTransition {
        id: None,
        host_id: "r1".into(),
        previous_status: HostStatus::Unknown,
        new_status: HostStatus::Down,
        changed_at: Utc::now(),
        latency_ms: None,
        error: Some("timeout".into()),
        backend: "fake".into(),
        reason: "failure threshold reached".into(),
    };

    storage
        .update_check_result("r1", state, Some(event))
        .await
        .unwrap();

    let host = storage.host("r1").await.unwrap().unwrap();
    assert_eq!(host.state.status, HostStatus::Down);
    assert_eq!(
        storage
            .hosts(HostFilter {
                icmp_status: Some(HostStatus::Down),
                group: None,
                metadata: HashMap::new(),
                usage: UsageFilter::empty(Utc::now()),
            })
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(storage.history("r1", 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn stores_usage_samples_for_every_poll_and_history_for_changes() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let first = UsageSnapshot::success(Utc::now(), 1, 2);
    let second = UsageSnapshot::success(Utc::now(), 1, 2);
    let third = UsageSnapshot::success(Utc::now(), 2, 2);

    storage.update_usage("r1", first).await.unwrap();
    storage.update_usage("r1", second).await.unwrap();
    storage.update_usage("r1", third).await.unwrap();

    let host = storage.host("r1").await.unwrap().unwrap();
    assert_eq!(host.usage.unwrap().console_users, Some(2));
    assert_eq!(storage.usage_samples("r1", 10).await.unwrap().len(), 3);
    assert_eq!(storage.usage_history("r1", 10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn usage_report_summarizes_latest_usage() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    storage
        .update_usage("r1", UsageSnapshot::success(Utc::now(), 2, 2))
        .await
        .unwrap();

    let summary = storage
        .usage_report(UsageFilter::empty(Utc::now()))
        .await
        .unwrap()
        .summary;

    assert_eq!(summary.console_users, 2);
    assert_eq!(summary.remote_users, 2);
}

#[tokio::test]
async fn update_check_result_is_atomic() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let event = IcmpTransition {
        id: None,
        host_id: "r1".into(),
        previous_status: HostStatus::Unknown,
        new_status: HostStatus::Down,
        changed_at: Utc::now(),
        latency_ms: None,
        error: Some("timeout".into()),
        backend: "fake".into(),
        reason: "test".into(),
    };
    let state = HostRuntimeState {
        status: HostStatus::Down,
        last_checked_at: Some(Utc::now()),
        last_change_at: Some(Utc::now()),
        consecutive_failures: 1,
        last_error: Some("timeout".into()),
        ..Default::default()
    };
    storage
        .update_check_result("r1", state, Some(event))
        .await
        .unwrap();
    let record = storage.host("r1").await.unwrap().unwrap();
    assert_eq!(record.state.status, HostStatus::Down);
    let history = storage.history("r1", 10).await.unwrap();
    assert_eq!(history.len(), 1);
}

#[tokio::test]
async fn update_usage_failed_write_does_not_advance_cache() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let err = storage
        .update_usage("does-not-exist", UsageSnapshot::success(Utc::now(), 1, 2))
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::NotFound(_)));
    let record = storage.host("r1").await.unwrap().unwrap();
    assert!(record.usage.is_none());
}

#[tokio::test]
async fn inactive_console_for_consults_state_at_cutoff() {
    use chrono::Duration as ChronoDuration;
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let now = Utc::now();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(10), 3, 0),
        )
        .await
        .unwrap();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(2), 0, 0),
        )
        .await
        .unwrap();

    let mut filter = UsageFilter::empty(now);
    filter.inactive_console_for = Some(std::time::Duration::from_secs(7 * 24 * 3600));
    let report = storage.usage_report(filter).await.unwrap();
    assert!(
        report.hosts.is_empty(),
        "host was active at cutoff (state=Ok/3 at -10d, transitioned to 0 at -2d) - must NOT be marked inactive for 7d"
    );
}

#[tokio::test]
async fn inactive_console_for_does_not_overshoot_history() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    storage
        .update_usage("r1", UsageSnapshot::success(Utc::now(), 0, 0))
        .await
        .unwrap();

    let mut filter = UsageFilter::empty(Utc::now());
    filter.inactive_console_for = Some(std::time::Duration::from_secs(60 * 24 * 3600));
    let report = storage.usage_report(filter).await.unwrap();
    assert!(
        report.hosts.is_empty(),
        "host claimed inactive for 60d despite ~0d of history: {:?}",
        report.hosts.iter().map(|h| &h.id).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn open_applies_wal_and_foreign_keys() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let _storage = SqliteStorage::open(&path, vec![host()]).unwrap();

    let probe = rusqlite::Connection::open(&path).unwrap();
    let journal: String = probe
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal.to_lowercase(), "wal");
    apply_pragmas(&probe).unwrap();
    let fk: i64 = probe
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
    let version: i64 = probe
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

#[tokio::test]
async fn pooled_reader_connections_are_read_only() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let storage = SqliteStorage::open(tmp.path(), vec![host()]).unwrap();
    let reader = storage
        .readers
        .as_ref()
        .expect("file-backed storage should have reader pool")
        .get()
        .unwrap();

    let err = reader
        .execute("CREATE TABLE should_fail (id INTEGER)", [])
        .unwrap_err();
    assert!(
        err.to_string().contains("readonly") || err.to_string().contains("query only"),
        "unexpected reader write error: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_read_does_not_block_on_writer_mutex() {
    use std::time::{Duration, Instant};

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let storage = SqliteStorage::open(tmp.path(), vec![host()]).unwrap();
    storage
        .update_check_result(
            "r1",
            HostRuntimeState::default(),
            Some(IcmpTransition {
                id: None,
                host_id: "r1".into(),
                previous_status: HostStatus::Unknown,
                new_status: HostStatus::Up,
                changed_at: Utc::now(),
                latency_ms: Some(1.0),
                error: None,
                backend: "test".into(),
                reason: "seed".into(),
            }),
        )
        .await
        .unwrap();

    let inner_holder = Arc::clone(&storage.inner);
    let blocker = tokio::task::spawn_blocking(move || {
        let _guard = inner_holder.lock().unwrap();
        std::thread::sleep(Duration::from_millis(250));
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let start = Instant::now();
    let rows = storage.history("r1", 10).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(rows.len(), 1);
    assert!(
        elapsed < Duration::from_millis(150),
        "history() took {elapsed:?} - appears to be blocked by writer Mutex"
    );

    let _ = blocker.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_read_does_not_block_on_writer_mutex() {
    use std::time::{Duration, Instant};

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let storage = SqliteStorage::open(tmp.path(), vec![host()]).unwrap();
    storage
        .update_check_result("r1", HostRuntimeState::default(), None)
        .await
        .unwrap();

    let inner_holder = Arc::clone(&storage.inner);
    let blocker = tokio::task::spawn_blocking(move || {
        let _guard = inner_holder.lock().unwrap();
        std::thread::sleep(Duration::from_millis(250));
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let start = Instant::now();
    let record = storage.host("r1").await.unwrap();
    let elapsed = start.elapsed();

    assert!(record.is_some());
    assert!(
        elapsed < Duration::from_millis(150),
        "host() took {elapsed:?} - appears to be blocked by writer Mutex"
    );

    let _ = blocker.await;
}

#[tokio::test]
async fn future_dated_snapshot_does_not_wipe_existing_samples() {
    use chrono::Duration as ChronoDuration;
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    storage
        .set_usage_sample_retention(Some(std::time::Duration::from_secs(7 * 24 * 3600)))
        .unwrap();
    let now = Utc::now();

    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(2), 1, 0),
        )
        .await
        .unwrap();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(1), 1, 0),
        )
        .await
        .unwrap();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now + ChronoDuration::days(30), 1, 0),
        )
        .await
        .unwrap();

    let samples = storage.usage_samples("r1", 100).await.unwrap();
    assert_eq!(
        samples.len(),
        3,
        "future-dated snapshot must not wipe samples within retention"
    );
}

#[tokio::test]
async fn usage_sample_retention_prunes_old_rows() {
    use chrono::Duration as ChronoDuration;
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    storage
        .set_usage_sample_retention(Some(std::time::Duration::from_secs(7 * 24 * 3600)))
        .unwrap();
    let now = Utc::now();

    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(10), 1, 0),
        )
        .await
        .unwrap();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(3), 1, 0),
        )
        .await
        .unwrap();
    storage
        .update_usage("r1", UsageSnapshot::success(now, 1, 0))
        .await
        .unwrap();

    let samples = storage.usage_samples("r1", 100).await.unwrap();
    assert_eq!(
        samples.len(),
        2,
        "10-day-old sample should have been pruned"
    );
    for s in &samples {
        assert!(
            s.collected_at >= now - ChronoDuration::days(7),
            "kept sample older than retention: {:?}",
            s.collected_at
        );
    }
}

#[tokio::test]
async fn future_dated_usage_sample_does_not_prune_current_history() {
    use chrono::Duration as ChronoDuration;
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    storage
        .set_usage_sample_retention(Some(std::time::Duration::from_secs(7 * 24 * 3600)))
        .unwrap();
    let now = Utc::now();

    storage
        .update_usage("r1", UsageSnapshot::success(now, 1, 0))
        .await
        .unwrap();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now + ChronoDuration::days(30), 1, 0),
        )
        .await
        .unwrap();

    let samples = storage.usage_samples("r1", 10).await.unwrap();
    assert_eq!(
        samples.len(),
        2,
        "future-dated sample must not advance the retention cutoff"
    );
}

#[tokio::test]
async fn usage_sample_retention_disabled_by_default_in_in_memory() {
    use chrono::Duration as ChronoDuration;
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let now = Utc::now();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(365), 1, 0),
        )
        .await
        .unwrap();
    storage
        .update_usage("r1", UsageSnapshot::success(now, 1, 0))
        .await
        .unwrap();
    let samples = storage.usage_samples("r1", 100).await.unwrap();
    assert_eq!(samples.len(), 2, "no retention configured -> no pruning");
}

#[tokio::test]
async fn changed_error_message_writes_history_row() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let now = Utc::now();
    let first_changed = storage
        .update_usage("r1", UsageSnapshot::error(now, "connection refused".into()))
        .await
        .unwrap();
    let repeated_changed = storage
        .update_usage("r1", UsageSnapshot::error(now, "connection refused".into()))
        .await
        .unwrap();
    let changed_error_changed = storage
        .update_usage("r1", UsageSnapshot::error(now, "host unreachable".into()))
        .await
        .unwrap();
    let history = storage.usage_history("r1", 10).await.unwrap();
    assert!(first_changed);
    assert!(!repeated_changed);
    assert!(changed_error_changed);
    assert_eq!(
        history.len(),
        2,
        "Failed snapshot with different error must produce a new history row"
    );
}

#[tokio::test]
async fn inactive_console_for_treats_outage_as_unknown() {
    use chrono::Duration as ChronoDuration;
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let now = Utc::now();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(now - ChronoDuration::days(30), 0, 0),
        )
        .await
        .unwrap();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::error(
                now - ChronoDuration::days(3),
                "ssh: connection refused".into(),
            ),
        )
        .await
        .unwrap();
    storage
        .update_usage("r1", UsageSnapshot::success(now, 0, 0))
        .await
        .unwrap();

    let mut filter = UsageFilter::empty(now);
    filter.inactive_console_for = Some(std::time::Duration::from_secs(7 * 24 * 3600));
    let report = storage.usage_report(filter).await.unwrap();
    assert!(
        report.hosts.is_empty(),
        "host had a Failed observation in the inactivity window - state was unknown"
    );
}
