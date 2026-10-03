use crate::domain::{check::Check, validation::validate_address};
use async_trait::async_trait;
use std::{
    net::SocketAddr,
    num::NonZeroU16,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct TcpRequest {
    pub address: String,
    pub port: NonZeroU16,
    pub timeout: Duration,
}

/// Narrow transport boundary for deterministic tests and alternate connectors.
#[async_trait]
pub trait TcpTransport: Send + Sync {
    async fn resolve(&self, address: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
    async fn connect(&self, address: SocketAddr) -> Result<(), String>;
}

pub struct SystemTcpTransport;

#[async_trait]
impl TcpTransport for SystemTcpTransport {
    async fn resolve(&self, address: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        super::dns::resolve(address, port)
            .await
            .map_err(|err| err.message)
    }
    async fn connect(&self, address: SocketAddr) -> Result<(), String> {
        tokio::net::TcpStream::connect(address)
            .await
            .map(drop)
            .map_err(|err| format!("TCP connect failed: {err}"))
    }
}

pub struct TcpConnectCheck<T = SystemTcpTransport>(pub T);

#[async_trait]
impl<T: TcpTransport> Check for TcpConnectCheck<T> {
    type Request = TcpRequest;
    type Observation = Duration;
    type Error = String;

    async fn run(&self, request: &TcpRequest) -> Result<Duration, String> {
        validate_address(&request.address).map_err(str::to_owned)?;
        crate::domain::validation::validate_duration(request.timeout).map_err(str::to_owned)?;
        let started = Instant::now();
        tokio::time::timeout(request.timeout, async {
            let addresses = self.0.resolve(&request.address, request.port.get()).await?;
            let mut error = "TCP target resolved to no addresses".to_string();
            for address in addresses {
                match self.0.connect(address).await {
                    Ok(()) => return Ok(started.elapsed()),
                    Err(err) => error = err,
                }
            }
            Err(error)
        })
        .await
        .map_err(|_| "TCP check timed out (including DNS resolution)".to_string())?
    }
}

#[cfg(test)]
mod tests;
