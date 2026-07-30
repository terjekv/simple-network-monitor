use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, time::Duration};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostStatus {
    Unknown,
    Up,
    Down,
}

impl HostStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

impl TryFrom<&str> for HostStatus {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "unknown" => Ok(Self::Unknown),
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            other => Err(format!("invalid host status {other:?}")),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PingOutcome {
    pub address: Option<IpAddr>,
    pub latency: Duration,
}

#[derive(Clone, Debug)]
pub struct CheckFailure {
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct IcmpTransition {
    pub id: Option<i64>,
    pub host_id: String,
    pub previous_status: HostStatus,
    pub new_status: HostStatus,
    pub changed_at: DateTime<Utc>,
    pub latency_ms: Option<f64>,
    pub error: Option<String>,
    pub backend: String,
    pub reason: String,
}
