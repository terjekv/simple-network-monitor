use crate::{
    app::telemetry::RuntimeMetrics,
    backends::tcp::{SystemTcpTransport, TcpConnectCheck, TcpRequest},
    domain::{
        Host,
        check::Check,
        tcp::{CheckId, TcpSnapshot},
    },
    modules::runner::{self, MonitorJob},
    storage::{StorageError, TcpRepository},
};
use async_trait::async_trait;
use chrono::Utc;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinSet};

pub(super) async fn run(
    hosts: Vec<Host>,
    concurrency: usize,
    repository: Arc<dyn TcpRepository>,
    metrics: Arc<RuntimeMetrics>,
) {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    for host in hosts {
        for check in &host.modules.tcp.checks {
            let job = TcpJob {
                host_id: host.id.clone(),
                check_id: check.id.clone(),
                interval: host.modules.tcp.interval,
                request: TcpRequest {
                    address: host.address.clone(),
                    port: check.port,
                    timeout: host.modules.tcp.timeout,
                },
                check: TcpConnectCheck(SystemTcpTransport),
                repository: repository.clone(),
            };
            tasks.spawn(runner::run_job(job, semaphore.clone(), metrics.clone()));
        }
    }
    runner::supervise(tasks).await;
}

struct TcpJob<C> {
    host_id: String,
    check_id: CheckId,
    interval: Duration,
    request: TcpRequest,
    check: C,
    repository: Arc<dyn TcpRepository>,
}

#[async_trait]
impl<C: Check<Request = TcpRequest, Observation = Duration, Error = String> + 'static> MonitorJob
    for TcpJob<C>
{
    type Observation = TcpSnapshot;
    fn kind(&self) -> &'static str {
        "tcp"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn observe(&mut self) -> (TcpSnapshot, bool) {
        let observed = Utc::now();
        let result = self.check.run(&self.request).await;
        let success = result.is_ok();
        (TcpSnapshot::new(observed, result), success)
    }
    async fn persist(&self, snapshot: &TcpSnapshot) -> Result<(), StorageError> {
        self.repository
            .update_tcp(&self.host_id, &self.check_id, snapshot.clone())
            .await
    }
}
