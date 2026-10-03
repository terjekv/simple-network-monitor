use super::*;
use crate::{
    app::telemetry::RuntimeMetrics,
    backends::usage::{UsageCollectionRequest, UsageCounts, UsageFailure},
    storage::{HostRepository, SqliteStorage},
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct InjectedCollector(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl UsageCollector for InjectedCollector {
    fn name(&self) -> &'static str {
        "injected"
    }
    async fn collect(&self, _: &UsageCollectionRequest) -> Result<UsageCounts, UsageFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(UsageCounts {
            console_users: 2,
            remote_users: 3,
        })
    }
}

#[tokio::test]
async fn injected_collector_runs_only_after_prepared_module_is_spawned() {
    let config = AppConfig::from_toml_str(
        r#"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["example"] }]
[modules.usage]
enabled = true
interval = "1ms"
"#,
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let prepared =
        UsageModule::prepare_with_collector(&config, Arc::new(InjectedCollector(calls.clone())));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let storage = Arc::new(SqliteStorage::in_memory(config.hosts).unwrap());
    let spawned = prepared.spawn(ModuleRuntimeContext {
        host_repository: storage.clone(),
        icmp_repository: storage.clone(),
        usage_repository: storage.clone(),
        tcp_repository: storage.clone(),
        metrics: Arc::new(RuntimeMetrics::default()),
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(snapshot) = storage.host("r1").await.unwrap().unwrap().usage {
                assert_eq!(snapshot.console_users, Some(2));
                assert_eq!(snapshot.remote_users, Some(3));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    spawned.handle.abort();
    assert!(spawned.handle.await.unwrap_err().is_cancelled());
}
