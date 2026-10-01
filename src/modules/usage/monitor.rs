use crate::{
    app::telemetry::RuntimeMetrics,
    backends::check::UsageCheck,
    domain::check::Check,
    modules::runner::{self, MonitorJob},
};
use crate::{
    backends::usage::{UsageCollectionRequest, UsageCollector},
    domain::{Host, UsageCollectionStatus, UsageSnapshot},
    storage::UsageRepository,
};
use async_trait::async_trait;
use chrono::Utc;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinSet};

#[derive(Clone, Debug)]
pub struct UsageMonitorConfig {
    pub concurrency: usize,
}

pub async fn run_usage_monitor(
    hosts: Vec<Host>,
    config: UsageMonitorConfig,
    collector: Arc<dyn UsageCollector>,
    usage_repository: Arc<dyn UsageRepository>,
    metrics: Arc<RuntimeMetrics>,
) {
    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let mut tasks = JoinSet::new();

    for host in hosts.into_iter().filter(|host| host.modules.usage.enabled) {
        let collector = Arc::clone(&collector);
        let usage_repository = Arc::clone(&usage_repository);
        let semaphore = Arc::clone(&semaphore);
        let metrics = metrics.clone();
        tasks.spawn(async move {
            runner::run_job(
                UsageJob {
                    host,
                    collector,
                    repository: usage_repository,
                },
                semaphore,
                metrics,
            )
            .await;
        });
    }

    runner::supervise(tasks).await;
}

struct UsageJob {
    host: Host,
    collector: Arc<dyn UsageCollector>,
    repository: Arc<dyn UsageRepository>,
}

#[async_trait]
impl MonitorJob for UsageJob {
    type Observation = UsageSnapshot;
    fn kind(&self) -> &'static str {
        "usage"
    }
    fn interval(&self) -> Duration {
        self.host.modules.usage.interval
    }
    async fn observe(&mut self) -> (UsageSnapshot, bool) {
        let snapshot = collect_once(&self.host, self.collector.as_ref()).await;
        let success = snapshot.status == UsageCollectionStatus::Ok;
        (snapshot, success)
    }
    async fn persist(&self, snapshot: &UsageSnapshot) -> Result<(), crate::storage::StorageError> {
        let changed = self
            .repository
            .update_usage(&self.host.id, snapshot.clone())
            .await?;
        if changed && snapshot.status == UsageCollectionStatus::Failed {
            tracing::warn!(host_id = %self.host.id, collector = self.collector.name(), error = ?snapshot.error, "usage collection failed");
        }
        Ok(())
    }
}

pub async fn collect_once(host: &Host, collector: &dyn UsageCollector) -> UsageSnapshot {
    let collected_at = Utc::now();
    let request = UsageCollectionRequest {
        host_id: host.id.clone(),
        address: host.address.clone(),
        os: host.modules.usage.os,
        ssh_verify_host_key: host.modules.usage.ssh_verify_host_key,
        timeout: host.modules.usage.timeout,
        linux_min_uid: host.modules.usage.linux_min_uid,
        macos_min_uid: host.modules.usage.macos_min_uid,
    };

    match UsageCheck(collector).run(&request).await {
        Ok(counts) => {
            tracing::debug!(
                host_id = %host.id,
                collector = collector.name(),
                console_users = counts.console_users,
                remote_users = counts.remote_users,
                "usage collection succeeded"
            );
            UsageSnapshot::success(collected_at, counts.console_users, counts.remote_users)
        }
        Err(err) => UsageSnapshot::error(collected_at, err.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backends::usage::{UsageCounts, UsageFailure},
        domain::Host,
    };
    use async_trait::async_trait;

    struct FakeCollector;

    #[async_trait]
    impl UsageCollector for FakeCollector {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn collect(
            &self,
            _request: &UsageCollectionRequest,
        ) -> Result<UsageCounts, UsageFailure> {
            Ok(UsageCounts {
                console_users: 1,
                remote_users: 2,
            })
        }
    }

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
    async fn collect_once_returns_success_snapshot() {
        let snapshot = collect_once(&host(), &FakeCollector).await;
        assert_eq!(snapshot.console_users, Some(1));
        assert_eq!(snapshot.remote_users, Some(2));
    }
}
