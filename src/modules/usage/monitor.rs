use crate::{
    backends::usage::{UsageCollectionRequest, UsageCollector},
    domain::{Host, UsageCollectionStatus, UsageSnapshot},
    storage::UsageRepository,
};
use chrono::Utc;
use rand::RngExt;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinSet, time};

#[derive(Clone, Debug)]
pub struct UsageMonitorConfig {
    pub concurrency: usize,
}

pub async fn run_usage_monitor(
    hosts: Vec<Host>,
    config: UsageMonitorConfig,
    collector: Arc<dyn UsageCollector>,
    usage_repository: Arc<dyn UsageRepository>,
) {
    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let mut tasks = JoinSet::new();

    for host in hosts.into_iter().filter(|host| host.modules.usage.enabled) {
        let collector = Arc::clone(&collector);
        let usage_repository = Arc::clone(&usage_repository);
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            monitor_usage(host, collector, usage_repository, semaphore).await;
        });
    }

    while let Some(result) = tasks.join_next().await {
        if let Err(err) = result {
            tracing::error!(%err, "usage monitor task failed");
        }
    }
}

async fn monitor_usage(
    host: Host,
    collector: Arc<dyn UsageCollector>,
    usage_repository: Arc<dyn UsageRepository>,
    semaphore: Arc<Semaphore>,
) {
    jitter(host.modules.usage.interval).await;
    let mut interval = time::interval(host.modules.usage.interval);
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Ok(_permit) = semaphore.acquire().await else {
            return;
        };
        let snapshot = collect_once(&host, collector.as_ref()).await;
        match usage_repository
            .update_usage(&host.id, snapshot.clone())
            .await
        {
            Ok(changed) => {
                if changed && snapshot.status == UsageCollectionStatus::Failed {
                    tracing::warn!(
                        host_id = %host.id,
                        collector = collector.name(),
                        error = %snapshot.error.as_deref().unwrap_or("unknown usage collection error"),
                        "usage collection failed"
                    );
                }
            }
            Err(err) => {
                tracing::error!(host_id = %host.id, %err, "failed to store usage result");
            }
        }
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

    match collector.collect(&request).await {
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

async fn jitter(interval: Duration) {
    let max_millis = (interval.as_millis() / 10).min(2_000) as u64;
    if max_millis > 0 {
        let delay = rand::rng().random_range(0..=max_millis);
        time::sleep(Duration::from_millis(delay)).await;
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
