use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct RetryJob {
    observations: Arc<AtomicUsize>,
    writes: AtomicUsize,
}

#[async_trait]
impl MonitorJob for RetryJob {
    type Observation = usize;
    fn kind(&self) -> &'static str {
        "test"
    }
    fn interval(&self) -> Duration {
        Duration::from_millis(1)
    }
    async fn observe(&mut self) -> (usize, bool) {
        (self.observations.fetch_add(1, Ordering::SeqCst), true)
    }
    async fn persist(&self, observation: &usize) -> Result<(), StorageError> {
        assert_eq!(*observation, 0);
        if self.writes.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(StorageError::Busy)
        } else {
            Err(StorageError::StaleGeneration)
        }
    }
}

#[tokio::test]
async fn retries_keep_one_observation_and_one_execution_count() {
    let observations = Arc::new(AtomicUsize::new(0));
    let metrics = Arc::new(RuntimeMetrics::default());
    tokio::time::timeout(
        Duration::from_secs(3),
        run_job(
            RetryJob {
                observations: observations.clone(),
                writes: AtomicUsize::new(0),
            },
            Arc::new(Semaphore::new(1)),
            metrics.clone(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(observations.load(Ordering::SeqCst), 1);
    let stats = metrics.snapshot();
    assert_eq!(stats["test"].successes, 1);
    assert_eq!(stats["test"].storage_retries, 1);
}

struct PendingJob {
    started: Arc<tokio::sync::Notify>,
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

struct ObservationGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ObservationGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl MonitorJob for PendingJob {
    type Observation = ();
    fn kind(&self) -> &'static str {
        "test"
    }
    fn interval(&self) -> Duration {
        Duration::from_millis(1)
    }
    async fn observe(&mut self) -> ((), bool) {
        let _guard = ObservationGuard(self.dropped.clone());
        self.started.notify_one();
        std::future::pending().await
    }
    async fn persist(&self, _: &()) -> Result<(), StorageError> {
        panic!("cancelled observation must not be persisted")
    }
}

#[tokio::test]
async fn cancellation_drops_inflight_observation_and_releases_admission() {
    let started = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let semaphore = Arc::new(Semaphore::new(1));
    let metrics = Arc::new(RuntimeMetrics::default());
    let task = tokio::spawn(run_job(
        PendingJob {
            started: started.clone(),
            dropped: dropped.clone(),
        },
        semaphore.clone(),
        metrics.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(semaphore.available_permits(), 1);
    assert!(metrics.snapshot().is_empty());
}
