use super::*;
fn host() -> Host {
    Host {
        id: "node-a".into(),
        address: "192.0.2.1".into(),
        name: "Node A".into(),
        groups: vec!["core".into(), "shared".into()],
        metadata: Default::default(),
        modules: Default::default(),
    }
}
fn observation(state: &str) -> HistoryObservation {
    HistoryObservation {
        state: state.into(),
        success: state == "up",
        latency_ms: Some(4.0),
        console_users: None,
        remote_users: None,
        error: None,
    }
}
fn request(from: i64, to: i64) -> HistoryRequest {
    HistoryRequest {
        from_ms: from,
        to_ms: to,
        module: "icmp".into(),
        check: None,
        hosts: vec![],
        groups: vec![],
        breakdown: "all".into(),
        max_points: 600,
    }
}
fn fixture() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    migrate(&c).unwrap();
    sync_epochs(&c, &[host()], 0).unwrap();
    c
}
#[test]
fn rollup_is_restartable_and_preserves_live_query() {
    let c = fixture();
    record(&c, "node-a", "icmp", "", 1000, observation("up"), 600000).unwrap();
    record(&c, "node-a", "icmp", "", 31000, observation("down"), 600000).unwrap();
    let query = || query::query(&c, request(0, 600000), HistoryConfig::default(), 600000).unwrap();
    let before = query();
    assert_eq!(before.series[0].buckets[0].stats.up_ms, 30000);
    assert_eq!(before.series[0].buckets[0].stats.down_ms, 61000);
    rollup(&c, 600000, &HistoryConfig::default(), 1).unwrap();
    let after = query();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(
        rollup(&c, 600000, &HistoryConfig::default(), 1).unwrap().0,
        0
    );
}
#[test]
fn overlapping_groups_do_not_double_count_combined() {
    let c = fixture();
    record(&c, "node-a", "icmp", "", 1000, observation("up"), 10000).unwrap();
    let mut q = request(0, 10000);
    q.groups = vec!["core".into(), "shared".into()];
    let result = query::query(&c, q, HistoryConfig::default(), 10000).unwrap();
    assert_eq!(result.series[0].buckets[0].stats.samples, 1);
    assert_eq!(result.series[0].buckets[0].stats.up_ms, 9000);
}
#[test]
fn membership_changes_split_duration_and_keep_old_group() {
    let c = fixture();
    record(&c, "node-a", "icmp", "", 1000, observation("up"), 10000).unwrap();
    let mut changed = host();
    changed.groups = vec!["edge".into()];
    sync_epochs(&c, &[changed], 10000).unwrap();
    record(&c, "node-a", "icmp", "", 11000, observation("up"), 20000).unwrap();
    let mut q = request(0, 20000);
    q.breakdown = "group".into();
    let result = query::query(&c, q, HistoryConfig::default(), 20000).unwrap();
    let core = result.series.iter().find(|s| s.id == "core").unwrap();
    assert_eq!(core.buckets[0].eligible_ms, 10000);
    assert_eq!(core.buckets[0].stats.up_ms, 9000);
    let edge = result.series.iter().find(|s| s.id == "edge").unwrap();
    assert_eq!(edge.buckets[0].stats.up_ms, 9000);
}
#[test]
fn duplicate_or_late_observations_do_not_change_summary() {
    let c = fixture();
    for at in [1000, 1000, 500] {
        record(&c, "node-a", "icmp", "", at, observation("up"), 10000).unwrap();
    }
    let result = query::query(&c, request(0, 10000), HistoryConfig::default(), 10000).unwrap();
    assert_eq!(result.series[0].buckets[0].stats.samples, 1);
}
#[test]
fn raw_retention_keeps_aggregates() {
    let c = fixture();
    record(&c, "node-a", "icmp", "", 1000, observation("up"), 10000).unwrap();
    prune(
        &c,
        4 * 86400000,
        &HistoryConfig::default(),
        Duration::from_secs(30 * 86400),
        500,
    )
    .unwrap();
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM history_raw", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM history_buckets", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
}

#[test]
fn long_interval_crossing_retention_boundary_retains_finer_duration() {
    let c = fixture();
    let now = 40 * 86400000;
    let boundary = 10 * 86400000;
    c.execute("UPDATE history_epochs SET freshness_ms=864000000", [])
        .unwrap();
    record(
        &c,
        "node-a",
        "icmp",
        "",
        boundary - 1000,
        observation("up"),
        now,
    )
    .unwrap();
    record(
        &c,
        "node-a",
        "icmp",
        "",
        boundary + 1000,
        observation("down"),
        now,
    )
    .unwrap();
    rollup(&c, now, &HistoryConfig::default(), 100).unwrap();
    let body: String = c
        .query_row(
            "SELECT stats FROM history_buckets WHERE resolution=300 AND start_ms=?1",
            [boundary],
            |r| r.get(0),
        )
        .unwrap();
    let stats: HistoryStats = serde_json::from_str(&body).unwrap();
    assert_eq!(stats.up_ms, 1000);
}
#[test]
fn freshness_expiry_stays_unknown_instead_of_extending_downtime() {
    let c = fixture();
    record(
        &c,
        "node-a",
        "icmp",
        "",
        1000,
        observation("down"),
        86400000,
    )
    .unwrap();
    let data = query::query(&c, request(0, 86400000), HistoryConfig::default(), 86400000).unwrap();
    assert_eq!(
        data.series[0]
            .buckets
            .iter()
            .map(|b| b.stats.down_ms)
            .sum::<u64>(),
        61000
    );
    assert!(data.series[0].buckets.last().unwrap().stats.down_ms == 0);
}
#[tokio::test]
async fn public_series_and_events_use_reader_transaction_once() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let now = Utc::now().timestamp_millis();
    assert!(
        storage
            .history_series(request(now - 86400000, now))
            .await
            .is_ok()
    );
    assert!(
        storage
            .history_events(request(now - 86400000, now), None, 100, false)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn file_history_queries_do_not_wait_for_the_writer_mutex() {
    let directory = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(directory.path().join("monitor.db"), vec![host()]).unwrap();
    let inner = storage.inner.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let held = tokio::task::spawn_blocking(move || {
        let _guard = inner.lock().unwrap();
        let _ = ready_tx.send(());
        release_rx.recv().unwrap();
    });
    ready_rx.await.unwrap();
    let now = Utc::now().timestamp_millis();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        storage.history_series(request(now - 86400000, now)),
    )
    .await;
    release_tx.send(()).unwrap();
    held.await.unwrap();
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn failed_schedule_update_rolls_back_identity_and_generation() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    storage
        .configure_maintenance(Default::default(), Default::default())
        .unwrap();
    let generation = storage.monitor_generation().unwrap();
    storage.inner.lock().unwrap().conn.execute_batch("CREATE TRIGGER reject_schedule BEFORE UPDATE ON maintenance_jobs BEGIN SELECT RAISE(ABORT,'fake schedule error'); END;").unwrap();
    let mut changed = host();
    changed.address = "192.0.2.2".into();
    changed.groups = vec!["edge".into()];
    assert!(
        storage
            .update_configuration(
                vec![changed],
                Default::default(),
                Default::default(),
                Duration::from_secs(86400),
                Duration::from_secs(86400)
            )
            .is_err()
    );
    assert_eq!(
        storage.host("node-a").await.unwrap().unwrap().host.address,
        "192.0.2.1"
    );
    generation
        .update_check_result("node-a", HostRuntimeState::default(), None)
        .await
        .unwrap();
    let inner = storage.inner.lock().unwrap();
    assert_eq!(inner.history_retention, Duration::from_secs(365 * 86400));
    assert!(inner.usage_sample_retention.is_none());
    let active: String = inner
        .conn
        .query_row(
            "SELECT groups_json FROM history_epochs WHERE ended_ms IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&active).unwrap(),
        vec!["core", "shared"]
    );
}
