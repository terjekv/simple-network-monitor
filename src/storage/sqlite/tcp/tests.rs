use super::*;

fn host() -> Host {
    crate::AppConfig::from_toml_str(r#"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["example"], modules.tcp.checks = [{ id = "web", port = 443 }, { id = "ssh", port = 22 }] }]
[modules.tcp]
enabled = true
"#).unwrap().hosts.remove(0)
}

fn id() -> CheckId {
    CheckId::new("web".into()).unwrap()
}

async fn store(storage: &SqliteStorage) {
    storage
        .update_tcp(
            "r1",
            &id(),
            TcpSnapshot::new(Utc::now(), Ok(Duration::from_millis(5))),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn persists_independent_named_check_results_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite3");
    {
        let storage = SqliteStorage::open(&path, vec![host()]).unwrap();
        store(&storage).await;
        storage
            .update_tcp(
                "r1",
                &CheckId::new("ssh".into()).unwrap(),
                TcpSnapshot::new(Utc::now(), Err("connection refused".into())),
            )
            .await
            .unwrap();
    }
    let storage = SqliteStorage::open(&path, vec![host()]).unwrap();
    let record = storage.host("r1").await.unwrap().unwrap();
    assert!(record.tcp[&id()].success());
    assert!(!record.tcp[&CheckId::new("ssh".into()).unwrap()].success());
}

#[rstest::rstest]
#[case("address")]
#[case("port")]
#[case("disabled")]
#[case("removed")]
#[tokio::test]
async fn invalidates_results_when_check_identity_changes(#[case] change: &str) {
    let mut host = host();
    let storage = SqliteStorage::in_memory(vec![host.clone()]).unwrap();
    store(&storage).await;
    match change {
        "address" => host.address = "192.0.2.2".into(),
        "port" => host.modules.tcp.checks[0].port = 8443.try_into().unwrap(),
        "disabled" => host.modules.tcp.enabled = false,
        "removed" => host.modules.tcp.checks.clear(),
        _ => unreachable!(),
    }
    storage.update_hosts(vec![host]).unwrap();
    assert!(storage.host("r1").await.unwrap().unwrap().tcp.is_empty());
}

#[tokio::test]
async fn rejects_tcp_write_from_replaced_generation() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    let old = storage.monitor_generation().unwrap();
    let mut next = host();
    next.modules.tcp.checks[0].port = 8443.try_into().unwrap();
    storage.update_hosts(vec![next]).unwrap();
    let result = old
        .update_tcp(
            "r1",
            &id(),
            TcpSnapshot::new(Utc::now(), Ok(Duration::ZERO)),
        )
        .await;
    assert!(matches!(result, Err(StorageError::StaleGeneration)));
    assert!(storage.host("r1").await.unwrap().unwrap().tcp.is_empty());
}

#[tokio::test]
async fn failed_inventory_change_keeps_tcp_observations() {
    let storage = SqliteStorage::in_memory(vec![host()]).unwrap();
    store(&storage).await;
    let mut invalid = host();
    invalid
        .modules
        .tcp
        .checks
        .push(invalid.modules.tcp.checks[0].clone());
    assert!(storage.update_hosts(vec![invalid]).is_err());
    assert!(storage.host("r1").await.unwrap().unwrap().tcp[&id()].success());
}

#[tokio::test]
async fn migrates_v2_without_losing_existing_observations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite3");
    {
        let storage = SqliteStorage::open(&path, vec![host()]).unwrap();
        storage
            .update_check_result(
                "r1",
                HostRuntimeState {
                    status: crate::domain::HostStatus::Up,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE latest_tcp; PRAGMA user_version = 2;")
        .unwrap();
    drop(conn);
    let storage = SqliteStorage::open(&path, vec![host()]).unwrap();
    assert_eq!(
        storage.host("r1").await.unwrap().unwrap().state.status,
        crate::domain::HostStatus::Up
    );
    store(&storage).await;
    assert!(storage.host("r1").await.unwrap().unwrap().tcp[&id()].success());
}
