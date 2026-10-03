use super::{
    ping::{PingBackend, PingCheckRequest},
    usage::{UsageCollectionRequest, UsageCollector, UsageCounts, UsageFailure},
};
use crate::domain::check::Check;
use crate::domain::{CheckFailure, PingOutcome};
use async_trait::async_trait;

/// Adapters preserve the existing narrow backend APIs.
pub struct PingCheck<'a>(pub &'a dyn PingBackend);
pub struct UsageCheck<'a>(pub &'a dyn UsageCollector);

#[async_trait]
impl Check for PingCheck<'_> {
    type Request = PingCheckRequest;
    type Observation = PingOutcome;
    type Error = CheckFailure;
    async fn run(&self, request: &Self::Request) -> Result<Self::Observation, Self::Error> {
        self.0.check(request).await
    }
}

#[async_trait]
impl Check for UsageCheck<'_> {
    type Request = UsageCollectionRequest;
    type Observation = UsageCounts;
    type Error = UsageFailure;
    async fn run(&self, request: &Self::Request) -> Result<Self::Observation, Self::Error> {
        self.0.collect(request).await
    }
}
