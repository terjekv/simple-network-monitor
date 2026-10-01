use crate::domain::CheckFailure;
use std::{
    net::{IpAddr, ToSocketAddrs},
    sync::{Arc, LazyLock},
};
use tokio::sync::Semaphore;

// OS resolver calls can outlive a cancelled future. Keep admission inside the
// blocking task so repeated deadlines cannot accumulate resolver work.
pub(super) static DNS_ADMISSION: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(32)));

pub(super) async fn resolve(
    address: &str,
    port: u16,
) -> Result<Vec<std::net::SocketAddr>, CheckFailure> {
    if let Ok(ip) = address.parse::<IpAddr>() {
        return Ok(vec![std::net::SocketAddr::new(ip, port)]);
    }
    let permit = DNS_ADMISSION
        .clone()
        .try_acquire_owned()
        .map_err(|_| CheckFailure {
            message: "DNS resolver is busy".into(),
        })?;
    let hostname = address.to_owned();
    let addrs = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        (hostname.as_str(), port).to_socket_addrs()
    })
    .await
    .map_err(|err| CheckFailure {
        message: format!("DNS task failed: {err}"),
    })?
    .map_err(|err| CheckFailure {
        message: format!("failed to resolve address: {err}"),
    })?;
    Ok(addrs.take(64).collect())
}
