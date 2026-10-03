use crate::{app::telemetry::RuntimeMetrics, storage::StorageError};
use async_trait::async_trait;
use rand::RngExt;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::Semaphore, task::JoinSet, time};

/// Typed observation processing stays with the module; the runner owns timing
/// and durable-write retries. `observe` runs exactly once per stored result.
#[async_trait]
pub trait MonitorJob: Send + 'static {
    type Observation: Send + Sync;
    fn kind(&self) -> &'static str;
    fn interval(&self) -> Duration;
    async fn observe(&mut self) -> (Self::Observation, bool);
    async fn persist(&self, observation: &Self::Observation) -> Result<(), StorageError>;
}

pub async fn run_job<J: MonitorJob + Sync>(
    mut job: J,
    semaphore: Arc<Semaphore>,
    metrics: Arc<RuntimeMetrics>,
) {
    let max_millis = (job.interval().as_millis() / 10).min(2_000) as u64;
    if max_millis > 0 {
        let delay = rand::rng().random_range(0..=max_millis);
        time::sleep(Duration::from_millis(delay)).await;
    }
    let mut interval = time::interval(job.interval());
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Ok(_permit) = semaphore.acquire().await else {
            return;
        };
        let started = Instant::now();
        let (observation, success) = job.observe().await;
        metrics.observe(job.kind(), success, started.elapsed());
        // Keep admission until persistence succeeds to bound pending writes.
        loop {
            match job.persist(&observation).await {
                Ok(()) => break,
                Err(StorageError::StaleGeneration) => return,
                Err(err) => {
                    metrics.storage_retry(job.kind());
                    tracing::error!(kind = job.kind(), %err, "failed to store observation; retrying");
                    time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
}

/// A host-task exit ends its module so service supervision can restart it.
pub async fn supervise(mut tasks: JoinSet<()>) {
    if let Some(result) = tasks.join_next().await {
        tracing::error!(?result, "check task exited; stopping module");
        tasks.shutdown().await;
    }
}

#[cfg(test)]
mod tests;
