use super::UsageOs;
use serde::{Deserialize, Serialize};
use std::time::Duration;
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedIcmpHostConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedUsageHostConfig {
    pub enabled: bool,
    pub os: UsageOs,
    pub ssh_verify_host_key: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub linux_min_uid: u32,
    pub macos_min_uid: u32,
}

impl Default for ResolvedIcmpHostConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(1),
        }
    }
}
impl Default for ResolvedUsageHostConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            os: UsageOs::Auto,
            ssh_verify_host_key: true,
            interval: Duration::from_secs(300),
            timeout: Duration::from_secs(5),
            linux_min_uid: 1000,
            macos_min_uid: 500,
        }
    }
}
