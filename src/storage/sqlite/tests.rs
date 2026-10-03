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
    assert_eq!(version, 4);
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
#[tokio::test]
async fn old_generation_cannot_write_after_replacement() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let old = storage.monitor_generation().unwrap();
    let permit = storage
        .write_admission
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let queued = tokio::spawn(async move {
        old.update_check_result(
            "r1",
            HostRuntimeState {
                status: HostStatus::Up,
                ..Default::default()
            },
            None,
        )
        .await
    });
    tokio::task::yield_now().await;
    let mut replacement = host();
    replacement.address = "192.0.2.2".into();
    storage.update_hosts(vec![replacement]).unwrap();
    drop(permit);
    assert!(matches!(
        queued.await.unwrap(),
        Err(StorageError::StaleGeneration)
    ));
    assert_eq!(
        storage.host("r1").await.unwrap().unwrap().state.status,
        HostStatus::Unknown
    );
}

#[tokio::test]
async fn address_changes_across_restart_reset_current_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let storage = SqliteStorage::open(&path, vec![host()]).unwrap();
    storage
        .update_check_result(
            "r1",
            HostRuntimeState {
                status: HostStatus::Up,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    drop(storage);
    let mut replacement = host();
    replacement.address = "192.0.2.2".into();
    let storage = SqliteStorage::open(&path, vec![replacement]).unwrap();
    assert_eq!(
        storage.host("r1").await.unwrap().unwrap().state.status,
        HostStatus::Unknown
    );
}

#[tokio::test]
async fn saturated_readers_reject_without_spawning_blocking_work() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let _all = storage
        .read_admission
        .clone()
        .acquire_many_owned(4)
        .await
        .unwrap();
    assert!(matches!(storage.host("r1").await, Err(StorageError::Busy)));
}

#[tokio::test]
async fn maintenance_prunes_samples_for_removed_hosts() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    storage
        .update_usage(
            "r1",
            UsageSnapshot::success(Utc::now() - chrono::Duration::days(40), 0, 0),
        )
        .await
        .unwrap();
    storage
        .set_usage_sample_retention(Some(Duration::from_secs(30 * 86400)))
        .unwrap();
    storage.update_hosts(vec![]).unwrap();
    assert_eq!(storage.prune_history(Utc::now()).await.unwrap(), 1);
}

#[rstest::rstest]
#[case(true, false, false, true)]
#[case(false, false, false, false)]
#[case(true, true, false, false)]
#[case(true, false, true, false)]
#[tokio::test]
async fn inactivity_requires_enabled_fresh_continuous_observation(
    #[case] enabled: bool,
    #[case] gap: bool,
    #[case] stale: bool,
    #[case] expected: bool,
) {
    let now = Utc::now();
    let mut fixture = host();
    fixture.modules.usage.enabled = enabled;
    fixture.modules.usage.interval = Duration::from_secs(60);
    let storage = SqliteStorage::in_memory(vec![fixture]).unwrap();
    let times = if gap {
        vec![now - chrono::Duration::minutes(10), now]
    } else {
        (0..=10)
            .rev()
            .map(|minute| now - chrono::Duration::minutes(minute + if stale { 60 } else { 0 }))
            .collect()
    };
    for time in times {
        storage
            .update_usage("r1", UsageSnapshot::success(time, 0, 0))
            .await
            .unwrap();
    }
    let mut filter = UsageFilter::empty(now);
    filter.no_users_for = Some(Duration::from_secs(300));
    assert_eq!(
        !storage.usage_report(filter).await.unwrap().hosts.is_empty(),
        expected
    );
}

#[tokio::test]
async fn usage_summary_does_not_overflow_u32() {
    let mut second = host();
    second.id = "r2".into();
    let storage = SqliteStorage::in_memory(vec![host(), second]).unwrap();
    for id in ["r1", "r2"] {
        storage
            .update_usage(id, UsageSnapshot::success(Utc::now(), u32::MAX, 0))
            .await
            .unwrap();
    }
    assert_eq!(
        storage
            .usage_report(UsageFilter::empty(Utc::now()))
            .await
            .unwrap()
            .summary
            .console_users,
        u64::from(u32::MAX) * 2
    );
}

#[test]
fn corrupt_latency_returns_error_instead_of_panicking() {
    assert!(runtime_state_from_row("up".into(), None, None, Some(-1.0), 1, 0, None).is_err());
}

#[test]
fn v1_schema_migrates_transactionally() {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    migrate(&conn).unwrap();
    assert_eq!(
        conn.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        4
    );
    assert!(
        conn.prepare("SELECT host_id, started_at FROM usage_coverage")
            .is_ok()
    );
    migrate(&conn).unwrap();
}

#[tokio::test]
async fn repeating_a_transition_write_does_not_duplicate_history() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let event = IcmpTransition {
        id: None,
        host_id: "r1".into(),
        previous_status: HostStatus::Unknown,
        new_status: HostStatus::Up,
        changed_at: Utc::now(),
        latency_ms: Some(1.0),
        error: None,
        backend: "fake".into(),
        reason: "test".into(),
    };
    for _ in 0..2 {
        storage
            .update_check_result("r1", HostRuntimeState::default(), Some(event.clone()))
            .await
            .unwrap();
    }
    assert_eq!(storage.history("r1", 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn retention_cannot_make_an_unobserved_long_window_look_inactive() {
    let now = Utc::now();
    let mut fixture = host();
    fixture.modules.usage.enabled = true;
    fixture.modules.usage.interval = Duration::from_secs(60);
    let storage = SqliteStorage::in_memory(vec![fixture]).unwrap();
    for minute in (0..=10).rev() {
        storage
            .update_usage(
                "r1",
                UsageSnapshot::success(now - chrono::Duration::minutes(minute), 0, 0),
            )
            .await
            .unwrap();
    }
    storage
        .set_history_retention(Duration::from_secs(120))
        .unwrap();
    storage.prune_history(now).await.unwrap();
    let mut filter = UsageFilter::empty(now);
    filter.no_users_for = Some(Duration::from_secs(300));
    assert!(storage.usage_report(filter).await.unwrap().hosts.is_empty());
}
