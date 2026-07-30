use crate::{
    domain::{HostStatus, usage::UsageSnapshot},
    modules::{icmp::ResolvedIcmpHostConfig, usage::ResolvedUsageHostConfig},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub address: String,
    pub name: String,
    pub groups: Vec<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    pub modules: HostModuleConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostModuleConfig {
    pub icmp: ResolvedIcmpHostConfig,
    pub usage: ResolvedUsageHostConfig,
}

#[derive(Clone, Debug)]
pub struct HostRuntimeState {
    pub status: HostStatus,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_change_at: Option<DateTime<Utc>>,
    pub latency: Option<Duration>,
    pub consecutive_successes: u32,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

impl Default for HostRuntimeState {
    fn default() -> Self {
        Self {
            status: HostStatus::Unknown,
            last_checked_at: None,
            last_change_at: None,
            latency: None,
            consecutive_successes: 0,
            consecutive_failures: 0,
            last_error: None,
        }
    }
}

impl HostRuntimeState {
    pub fn to_record(&self, host: &Host, usage: Option<UsageSnapshot>) -> HostRecord {
        HostRecord {
            host: host.clone(),
            state: self.clone(),
            usage,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HostRecord {
    pub host: Host,
    pub state: HostRuntimeState,
    pub usage: Option<UsageSnapshot>,
}

pub fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
