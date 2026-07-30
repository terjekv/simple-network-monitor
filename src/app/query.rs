use crate::{
    domain::{
        HostFilter, HostRecord, IcmpTransition, UsageFilter, UsageHistory, UsageReport, UsageSample,
    },
    storage::{HostRepository, IcmpRepository, StorageError, UsageRepository},
};
use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("host {0:?} not found")]
    NotFound(String),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

#[async_trait]
pub trait HostQueryService: Send + Sync + 'static {
    async fn hosts(&self, filter: HostFilter) -> Result<Vec<HostRecord>, AppError>;
    async fn host(&self, id: &str) -> Result<Option<HostRecord>, AppError>;
    async fn icmp_history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<IcmpTransition>, AppError>;
    async fn usage_history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageHistory>, AppError>;
    async fn usage_samples(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageSample>, AppError>;
    async fn usage_report(&self, filter: UsageFilter) -> Result<UsageReport, AppError>;
}

#[derive(Clone)]
pub struct DefaultHostQueryService {
    hosts: Arc<dyn HostRepository>,
    icmp: Arc<dyn IcmpRepository>,
    usage: Arc<dyn UsageRepository>,
}

impl DefaultHostQueryService {
    pub fn new(
        hosts: Arc<dyn HostRepository>,
        icmp: Arc<dyn IcmpRepository>,
        usage: Arc<dyn UsageRepository>,
    ) -> Self {
        Self { hosts, icmp, usage }
    }
}

#[async_trait]
impl HostQueryService for DefaultHostQueryService {
    async fn hosts(&self, filter: HostFilter) -> Result<Vec<HostRecord>, AppError> {
        Ok(self.hosts.hosts(filter).await?)
    }

    async fn host(&self, id: &str) -> Result<Option<HostRecord>, AppError> {
        Ok(self.hosts.host(id).await?)
    }

    async fn icmp_history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<IcmpTransition>, AppError> {
        Ok(self.icmp.history(host_id, limit).await?)
    }

    async fn usage_history(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageHistory>, AppError> {
        Ok(self.usage.usage_history(host_id, limit).await?)
    }

    async fn usage_samples(
        &self,
        host_id: &str,
        limit: usize,
    ) -> Result<Vec<UsageSample>, AppError> {
        Ok(self.usage.usage_samples(host_id, limit).await?)
    }

    async fn usage_report(&self, filter: UsageFilter) -> Result<UsageReport, AppError> {
        Ok(self.usage.usage_report(filter).await?)
    }
}
