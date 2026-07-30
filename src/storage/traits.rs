use crate::domain::{
    HostFilter, HostRecord, HostRuntimeState, IcmpTransition, UsageFilter, UsageHistory,
    UsageReport, UsageSample, UsageSnapshot,
};
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("host {0:?} not found")]
    NotFound(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid data in storage: {0}")]
    InvalidData(String),
    #[error("storage task failed: {0}")]
    TaskJoin(String),
    #[error("storage lock poisoned")]
    LockPoisoned,
}

#[async_trait]
pub trait HostRepository: Send + Sync + 'static {
    async fn hosts(&self, filter: HostFilter) -> Result<Vec<HostRecord>, StorageError>;
    async fn host(&self, id: &str) -> Result<Option<HostRecord>, StorageError>;
}

#[async_trait]
pub trait IcmpRepository: Send + Sync + 'static {
    async fn update_check_result(
        &self,
        host_id: &str,
        state: HostRuntimeState,
        transition: Option<IcmpTransition>,
    ) -> Result<(), StorageError>;

    async fn history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<IcmpTransition>, StorageError>;
}

#[async_trait]
pub trait UsageRepository: Send + Sync + 'static {
    async fn update_usage(
        &self,
        host_id: &str,
        snapshot: UsageSnapshot,
    ) -> Result<bool, StorageError>;

    async fn usage_history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageHistory>, StorageError>;

    async fn usage_samples(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageSample>, StorageError>;

    async fn usage_report(&self, filter: UsageFilter) -> Result<UsageReport, StorageError>;
}
