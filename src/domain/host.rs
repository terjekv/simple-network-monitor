use crate::{
    domain::settings::{ResolvedIcmpHostConfig, ResolvedUsageHostConfig},
    domain::{HostStatus, usage::UsageSnapshot},
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

/// An observation is current for two intervals plus its collection deadline.
pub fn is_fresh(
    observed: Option<DateTime<Utc>>,
    interval: Duration,
    timeout: Duration,
    now: DateTime<Utc>,
) -> bool {
    let Some(observed) = observed else {
        return false;
    };
    observed <= now
        && (now - observed)
            .to_std()
            .is_ok_and(|age| age <= interval.saturating_mul(2).saturating_add(timeout))
}

impl Host {
    /// Validate values at persistence and configuration boundaries.
    pub fn validate(&self) -> Result<(), &'static str> {
        use super::validation::{validate_address, validate_duration};
        if self.id.is_empty()
            || matches!(self.id.as_str(), "." | "..")
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        {
            return Err("invalid host ID");
        }
        validate_address(&self.address)?;
        for duration in [
            self.modules.icmp.interval,
            self.modules.icmp.timeout,
            self.modules.usage.interval,
            self.modules.usage.timeout,
        ] {
            validate_duration(duration)?;
        }
        Ok(())
    }
}
