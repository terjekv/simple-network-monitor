use crate::{
    app::telemetry::RuntimeMetrics,
    backends::check::PingCheck,
    domain::check::Check,
    modules::runner::{self, MonitorJob},
};
use crate::{
    backends::ping::{PingBackend, PingCheckRequest},
    domain::{Host, HostRuntimeState, HostStatus, IcmpTransition, duration_ms},
    storage::{HostRepository, IcmpRepository},
};
use async_trait::async_trait;
use chrono::Utc;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinSet, time};

#[derive(Clone, Debug)]
pub struct IcmpMonitorConfig {
    pub concurrency: usize,
    pub failure_threshold: u32,
    pub success_threshold: u32,
}

pub async fn run_icmp_monitor(
    hosts: Vec<Host>,
    config: IcmpMonitorConfig,
    backend: Arc<dyn PingBackend>,
    host_repository: Arc<dyn HostRepository>,
    icmp_repository: Arc<dyn IcmpRepository>,
    metrics: Arc<RuntimeMetrics>,
) {
    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let mut tasks = JoinSet::new();

    for host in hosts {
        let backend = Arc::clone(&backend);
        let host_repository = Arc::clone(&host_repository);
        let icmp_repository = Arc::clone(&icmp_repository);
        let semaphore = Arc::clone(&semaphore);
        let config = config.clone();
        let metrics = metrics.clone();
        tasks.spawn(async move {
            monitor_host(
                host,
                config,
                backend,
                host_repository,
                icmp_repository,
                semaphore,
                metrics,
            )
            .await;
        });
    }

    runner::supervise(tasks).await;
}

async fn monitor_host(
    host: Host,
    config: IcmpMonitorConfig,
    backend: Arc<dyn PingBackend>,
    host_repository: Arc<dyn HostRepository>,
    icmp_repository: Arc<dyn IcmpRepository>,
    semaphore: Arc<Semaphore>,
    metrics: Arc<RuntimeMetrics>,
) {
    let state = loop {
        match host_repository.host(&host.id).await {
            Ok(Some(record)) => break record.state,
            Err(crate::storage::StorageError::Busy) => time::sleep(Duration::from_millis(25)).await,
            Ok(None) => {
                tracing::error!(host_id = %host.id, "configured host is missing");
                return;
            }
            Err(err) => {
                tracing::error!(host_id = %host.id, %err, "failed to load initial host state");
                return;
            }
        }
    };

    runner::run_job(
        IcmpJob {
            host,
            config,
            backend,
            repository: icmp_repository,
            state,
        },
        semaphore,
        metrics,
    )
    .await;
}

struct IcmpJob {
    host: Host,
    config: IcmpMonitorConfig,
    backend: Arc<dyn PingBackend>,
    repository: Arc<dyn IcmpRepository>,
    state: HostRuntimeState,
}

#[async_trait]
impl MonitorJob for IcmpJob {
    type Observation = (HostRuntimeState, Option<IcmpTransition>);
    fn kind(&self) -> &'static str {
        "icmp"
    }
    fn interval(&self) -> Duration {
        self.host.modules.icmp.interval
    }
    async fn observe(&mut self) -> (Self::Observation, bool) {
        let transition = check_once(
            &self.host,
            &self.config,
            self.backend.as_ref(),
            &mut self.state,
        )
        .await;
        (
            (self.state.clone(), transition),
            self.state.last_error.is_none(),
        )
    }
    async fn persist(
        &self,
        observation: &Self::Observation,
    ) -> Result<(), crate::storage::StorageError> {
        self.repository
            .update_check_result(&self.host.id, observation.0.clone(), observation.1.clone())
            .await
    }
}

pub async fn check_once(
    host: &Host,
    config: &IcmpMonitorConfig,
    backend: &dyn PingBackend,
    state: &mut HostRuntimeState,
) -> Option<IcmpTransition> {
    let checked_at = Utc::now();
    state.last_checked_at = Some(checked_at);
    let request = PingCheckRequest {
        host_id: host.id.clone(),
        address: host.address.clone(),
        timeout: host.modules.icmp.timeout,
    };

    match PingCheck(backend).run(&request).await {
        Ok(outcome) => {
            state.latency = Some(outcome.latency);
            state.consecutive_successes = state.consecutive_successes.saturating_add(1);
            state.consecutive_failures = 0;
            state.last_error = None;
            if state.status != HostStatus::Up
                && state.consecutive_successes >= config.success_threshold
            {
                return transition_to(
                    host,
                    state,
                    HostStatus::Up,
                    checked_at,
                    Some(duration_ms(outcome.latency)),
                    None,
                    backend.name(),
                    "success threshold reached",
                );
            }
            tracing::debug!(host_id = %host.id, status = ?state.status, "host check succeeded");
        }
        Err(err) => {
            state.latency = None;
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            state.consecutive_successes = 0;
            state.last_error = Some(err.message.clone());
            if state.status != HostStatus::Down
                && state.consecutive_failures >= config.failure_threshold
            {
                return transition_to(
                    host,
                    state,
                    HostStatus::Down,
                    checked_at,
                    None,
                    Some(err.message),
                    backend.name(),
                    "failure threshold reached",
                );
            }
            tracing::debug!(host_id = %host.id, status = ?state.status, "host check failed");
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn transition_to(
    host: &Host,
    state: &mut HostRuntimeState,
    new_status: HostStatus,
    changed_at: chrono::DateTime<Utc>,
    latency_ms: Option<f64>,
    error: Option<String>,
    backend: &str,
    reason: &str,
) -> Option<IcmpTransition> {
    let previous_status = state.status.clone();
    state.status = new_status.clone();
    state.last_change_at = Some(changed_at);
    tracing::info!(
        host_id = %host.id,
        previous_status = previous_status.as_str(),
        new_status = new_status.as_str(),
        "host status changed"
    );
    Some(IcmpTransition {
        id: None,
        host_id: host.id.clone(),
        previous_status,
        new_status,
        changed_at,
        latency_ms,
        error,
        backend: backend.into(),
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CheckFailure, PingOutcome};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct FakeBackend {
        results: Mutex<Vec<bool>>,
    }

    #[async_trait]
    impl PingBackend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn check(&self, _request: &PingCheckRequest) -> Result<PingOutcome, CheckFailure> {
            if self.results.lock().unwrap().remove(0) {
                Ok(PingOutcome {
                    address: None,
                    latency: Duration::from_millis(5),
                })
            } else {
                Err(CheckFailure {
                    message: "timeout".into(),
                })
            }
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
    async fn transitions_down_after_failure_threshold() {
        let backend = FakeBackend {
            results: Mutex::new(vec![false, false]),
        };
        let config = IcmpMonitorConfig {
            concurrency: 10,
            failure_threshold: 2,
            success_threshold: 1,
        };
        let mut state = HostRuntimeState::default();

        assert!(
            check_once(&host(), &config, &backend, &mut state)
                .await
                .is_none()
        );
        let event = check_once(&host(), &config, &backend, &mut state)
            .await
            .unwrap();

        assert_eq!(event.new_status, HostStatus::Down);
        assert_eq!(state.status, HostStatus::Down);
    }
    struct FailFirstWrite {
        storage: crate::storage::SqliteStorage,
        first: std::sync::atomic::AtomicBool,
        stored: tokio::sync::Notify,
    }
    #[async_trait]
    impl IcmpRepository for FailFirstWrite {
        async fn update_check_result(
            &self,
            id: &str,
            state: HostRuntimeState,
            transition: Option<IcmpTransition>,
        ) -> Result<(), crate::storage::StorageError> {
            if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::storage::StorageError::InvalidData(
                    "injected failure".into(),
                ));
            }
            self.storage
                .update_check_result(id, state, transition)
                .await?;
            self.stored.notify_one();
            Ok(())
        }
        async fn history(
            &self,
            id: &str,
            limit: usize,
        ) -> Result<Vec<IcmpTransition>, crate::storage::StorageError> {
            self.storage.history(id, limit).await
        }
    }

    #[tokio::test]
    async fn retries_the_initial_transition_before_taking_another_observation() {
        let mut fixture = host();
        fixture.modules.icmp.interval = Duration::from_millis(1);
        let storage = crate::storage::SqliteStorage::in_memory(vec![fixture.clone()]).unwrap();
        let repository = Arc::new(FailFirstWrite {
            storage: storage.clone(),
            first: std::sync::atomic::AtomicBool::new(true),
            stored: tokio::sync::Notify::new(),
        });
        let backend = Arc::new(FakeBackend {
            results: Mutex::new(vec![true]),
        });
        let task = tokio::spawn(monitor_host(
            fixture,
            IcmpMonitorConfig {
                concurrency: 1,
                failure_threshold: 1,
                success_threshold: 1,
            },
            backend,
            Arc::new(storage.clone()),
            repository.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(RuntimeMetrics::default()),
        ));
        time::timeout(Duration::from_secs(2), repository.stored.notified())
            .await
            .unwrap();
        task.abort();
        let _ = task.await;
        let events = storage.history("r1", 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].new_status, HostStatus::Up);
    }

    #[tokio::test]
    async fn a_panicked_host_task_ends_the_module_for_supervision() {
        let mut fixture = host();
        fixture.modules.icmp.interval = Duration::from_millis(1);
        let storage =
            Arc::new(crate::storage::SqliteStorage::in_memory(vec![fixture.clone()]).unwrap());
        let backend = Arc::new(FakeBackend {
            results: Mutex::new(vec![]),
        });
        time::timeout(
            Duration::from_secs(1),
            run_icmp_monitor(
                vec![fixture],
                IcmpMonitorConfig {
                    concurrency: 1,
                    failure_threshold: 1,
                    success_threshold: 1,
                },
                backend,
                storage.clone(),
                storage,
                Arc::new(RuntimeMetrics::default()),
            ),
        )
        .await
        .unwrap();
    }
}
