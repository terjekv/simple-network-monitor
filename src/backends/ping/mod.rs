use crate::{
    config::BackendKind,
    domain::{CheckFailure, PingOutcome},
};
use async_trait::async_trait;
use std::{
    collections::HashSet,
    net::IpAddr,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{net::lookup_host, process::Command, time};

#[derive(Clone, Debug)]
pub struct PingCheckRequest {
    pub host_id: String,
    pub address: String,
    pub timeout: Duration,
}

#[derive(Debug, Error)]
pub enum PingError {
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait PingBackend: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn check(&self, request: &PingCheckRequest) -> Result<PingOutcome, CheckFailure>;
}

pub async fn build_backend(kind: BackendKind) -> Result<Box<dyn PingBackend>, PingError> {
    match kind {
        BackendKind::System => Ok(Box::new(SystemPingBackend::new(current_platform()))),
        BackendKind::Raw => RawIcmpBackend::try_new()
            .await
            .map(|backend| Box::new(backend) as _),
        BackendKind::Auto => match RawIcmpBackend::try_new().await {
            Ok(backend) => Ok(Box::new(backend)),
            Err(err) => {
                tracing::warn!(%err, "raw ICMP backend unavailable, falling back to system ping");
                Ok(Box::new(SystemPingBackend::new(current_platform())))
            }
        },
    }
}

pub struct RawIcmpBackend {
    client_v4: Arc<surge_ping::Client>,
    /// Optional — opened best-effort. Hosts on systems without an IPv6 stack
    /// still get a working v4 raw backend rather than a hard startup failure.
    client_v6: Option<Arc<surge_ping::Client>>,
    salt: u16,
    counter: AtomicU16,
    inflight: Arc<Mutex<HashSet<u16>>>,
}

impl RawIcmpBackend {
    pub async fn try_new() -> Result<Self, PingError> {
        let client_v4 = surge_ping::Client::new(&surge_ping::Config::default())
            .map_err(|err| PingError::BackendUnavailable(err.to_string()))?;
        let client_v6 = match surge_ping::Client::new(
            &surge_ping::Config::builder()
                .kind(surge_ping::ICMP::V6)
                .build(),
        ) {
            Ok(c) => Some(Arc::new(c)),
            Err(err) => {
                tracing::warn!(%err, "IPv6 raw ICMP socket unavailable; raw backend will only ping IPv4 hosts");
                None
            }
        };
        let salt: u16 = rand::random();
        Ok(Self {
            client_v4: Arc::new(client_v4),
            client_v6,
            salt,
            counter: AtomicU16::new(1),
            inflight: Arc::new(Mutex::new(HashSet::new())),
        })
    }
}

#[async_trait]
impl PingBackend for RawIcmpBackend {
    fn name(&self) -> &'static str {
        "raw"
    }

    async fn check(&self, request: &PingCheckRequest) -> Result<PingOutcome, CheckFailure> {
        let address = resolve_ip(&request.address).await?;
        let client = match address {
            IpAddr::V4(_) => &self.client_v4,
            IpAddr::V6(_) => self.client_v6.as_ref().ok_or_else(|| CheckFailure {
                message: format!(
                    "ipv6 address {address} cannot be pinged: no IPv6 raw socket available"
                ),
            })?,
        };
        let seed = self
            .counter
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(self.salt);
        let identifier = next_unique_identifier(&self.inflight, seed)?;
        struct Release<'a>(&'a Mutex<HashSet<u16>>, u16);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                if let Ok(mut set) = self.0.lock() {
                    set.remove(&self.1);
                }
            }
        }
        let _release = Release(&self.inflight, identifier);

        let mut pinger = client
            .pinger(address, surge_ping::PingIdentifier(identifier))
            .await;
        pinger.timeout(request.timeout);
        let payload = [0_u8; 56];
        let (_packet, latency) = pinger
            .ping(surge_ping::PingSequence(0), &payload)
            .await
            .map_err(|err| CheckFailure {
                message: err.to_string(),
            })?;
        Ok(PingOutcome {
            address: Some(address),
            latency,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PingPlatform {
    Linux,
    MacOs,
}

pub struct SystemPingBackend {
    platform: PingPlatform,
}

impl SystemPingBackend {
    pub fn new(platform: PingPlatform) -> Self {
        Self { platform }
    }
}

#[async_trait]
impl PingBackend for SystemPingBackend {
    fn name(&self) -> &'static str {
        "system"
    }

    async fn check(&self, request: &PingCheckRequest) -> Result<PingOutcome, CheckFailure> {
        let args = ping_args(self.platform, &request.address, request.timeout);
        let started = Instant::now();
        let child = Command::new("ping")
            .args(args)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| CheckFailure {
                message: format!("failed to start ping: {err}"),
            })?;

        let output = time::timeout(request.timeout + request.timeout, child.wait_with_output())
            .await
            .map_err(|_| CheckFailure {
                message: "ping command timed out".into(),
            })?
            .map_err(|err| CheckFailure {
                message: format!("failed to wait for ping: {err}"),
            })?;

        if output.status.success() {
            Ok(PingOutcome {
                address: request.address.parse::<IpAddr>().ok(),
                latency: started.elapsed(),
            })
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(CheckFailure {
                message: if stderr.is_empty() {
                    format!("ping exited with {}", output.status)
                } else {
                    stderr
                },
            })
        }
    }
}

pub fn ping_args(
    platform: PingPlatform,
    address: &str,
    timeout: std::time::Duration,
) -> Vec<String> {
    let secs = timeout.as_secs_f32().max(0.001);
    match platform {
        PingPlatform::Linux => vec![
            "-n".into(),
            "-c".into(),
            "1".into(),
            "-W".into(),
            format!("{secs:.3}"),
            address.into(),
        ],
        PingPlatform::MacOs => vec![
            "-n".into(),
            "-c".into(),
            "1".into(),
            "-W".into(),
            timeout.as_millis().max(1).to_string(),
            address.into(),
        ],
    }
}

fn current_platform() -> PingPlatform {
    if cfg!(target_os = "macos") {
        PingPlatform::MacOs
    } else {
        PingPlatform::Linux
    }
}

async fn resolve_ip(address: &str) -> Result<IpAddr, CheckFailure> {
    if let Ok(ip) = address.parse::<IpAddr>() {
        return Ok(ip);
    }
    let addrs = lookup_host((address, 0))
        .await
        .map_err(|err| CheckFailure {
            message: format!("failed to resolve address: {err}"),
        })?;
    // Prefer IPv4 when both families are returned — matches the default behaviour
    // of most ping(8) implementations.
    let mut v4 = None;
    let mut v6 = None;
    for addr in addrs {
        match addr.ip() {
            IpAddr::V4(_) if v4.is_none() => v4 = Some(addr.ip()),
            IpAddr::V6(_) if v6.is_none() => v6 = Some(addr.ip()),
            _ => {}
        }
    }
    v4.or(v6).ok_or_else(|| CheckFailure {
        message: format!("{address} resolved to no IP addresses"),
    })
}

pub(crate) fn next_unique_identifier(
    inflight: &Mutex<HashSet<u16>>,
    seed: u16,
) -> Result<u16, CheckFailure> {
    const SPACE: u32 = u16::MAX as u32 + 1;
    let mut set = inflight.lock().expect("inflight mutex poisoned");
    let mut probe = seed;
    for _ in 0..SPACE {
        if !set.contains(&probe) {
            set.insert(probe);
            return Ok(probe);
        }
        probe = probe.wrapping_add(1);
    }
    Err(CheckFailure {
        message: "all 65536 icmp identifiers are in flight".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(PingPlatform::Linux, 1_500, vec!["-n", "-c", "1", "-W", "1.500", "192.0.2.1"])]
    #[case(PingPlatform::MacOs, 1_500, vec!["-n", "-c", "1", "-W", "1500", "192.0.2.1"])]
    fn ping_args_use_platform_timeout_units(
        #[case] platform: PingPlatform,
        #[case] timeout_ms: u64,
        #[case] expected: Vec<&str>,
    ) {
        assert_eq!(
            ping_args(platform, "192.0.2.1", Duration::from_millis(timeout_ms)),
            expected
        );
    }

    #[test]
    fn linux_timeout_uses_fractional_seconds() {
        let args = ping_args(PingPlatform::Linux, "192.0.2.1", Duration::from_millis(250));
        let w = args
            .iter()
            .position(|s| s == "-W")
            .expect("ping_args is missing -W");
        let value = &args[w + 1];
        let parsed: f32 = value.parse().expect("non-numeric -W value");
        assert!((parsed - 0.25).abs() < 1e-3, "expected ~0.25, got {value}");
    }

    #[test]
    fn linux_timeout_never_below_one_millisecond() {
        let args = ping_args(PingPlatform::Linux, "192.0.2.1", Duration::from_millis(0));
        let w = args.iter().position(|s| s == "-W").unwrap();
        let parsed: f32 = args[w + 1].parse().unwrap();
        assert!(parsed >= 0.001, "expected ≥1ms floor, got {parsed}");
    }

    #[tokio::test]
    async fn resolve_ip_accepts_v6_literal() {
        // Dual-stack backend should resolve both literal v4 and v6 addresses.
        let ip = resolve_ip("::1").await.unwrap();
        assert!(ip.is_ipv6());
    }

    #[tokio::test]
    async fn resolve_ip_returns_v4_literal() {
        let ip = resolve_ip("192.0.2.1").await.unwrap();
        assert!(ip.is_ipv4());
    }

    #[tokio::test]
    async fn raw_backend_does_not_reuse_in_flight_identifier() {
        let inflight: Mutex<HashSet<u16>> = Mutex::new(HashSet::new());

        let mut taken = Vec::new();
        for _ in 0..1024 {
            let id = next_unique_identifier(&inflight, 0xABCDu16).unwrap();
            assert!(
                inflight.lock().unwrap().contains(&id),
                "helper must reserve {id} in the set"
            );
            assert!(
                !taken.contains(&id),
                "helper handed out duplicate in-flight id {id}"
            );
            taken.push(id);
        }
        assert_eq!(inflight.lock().unwrap().len(), 1024);

        for id in taken.iter().step_by(2) {
            inflight.lock().unwrap().remove(id);
        }
        let mut second_round = Vec::new();
        for _ in 0..256 {
            let id = next_unique_identifier(&inflight, 0xABCDu16).unwrap();
            assert!(!second_round.contains(&id));
            second_round.push(id);
        }
        let set = inflight.lock().unwrap();
        assert_eq!(set.len(), set.iter().collect::<HashSet<_>>().len());
    }
}
