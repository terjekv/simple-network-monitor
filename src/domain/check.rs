use async_trait::async_trait;

/// One observation, without scheduling, persistence, or HTTP concerns.
/// Implementations must include resolution and I/O in the request's deadline.
#[async_trait]
pub trait Check: Send + Sync {
    type Request: Send + Sync;
    type Observation: Send;
    type Error: Send;

    async fn run(&self, request: &Self::Request) -> Result<Self::Observation, Self::Error>;
}
