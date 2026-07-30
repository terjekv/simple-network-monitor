use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageOs {
    Auto,
    Linux,
    Macos,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageCollectionStatus {
    Ok,
    #[serde(rename = "error")]
    Failed,
}

impl UsageCollectionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "error",
        }
    }
}

impl TryFrom<&str> for UsageCollectionStatus {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "ok" => Ok(Self::Ok),
            "error" => Ok(Self::Failed),
            other => Err(format!("invalid usage collection status {other:?}")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct UsageSnapshot {
    pub collected_at: DateTime<Utc>,
    pub console_users: Option<u32>,
    pub remote_users: Option<u32>,
    pub status: UsageCollectionStatus,
    pub error: Option<String>,
}

impl UsageSnapshot {
    pub fn success(collected_at: DateTime<Utc>, console_users: u32, remote_users: u32) -> Self {
        Self {
            collected_at,
            console_users: Some(console_users),
            remote_users: Some(remote_users),
            status: UsageCollectionStatus::Ok,
            error: None,
        }
    }

    pub fn error(collected_at: DateTime<Utc>, error: String) -> Self {
        Self {
            collected_at,
            console_users: None,
            remote_users: None,
            status: UsageCollectionStatus::Failed,
            error: Some(error),
        }
    }

    pub fn differs_for_history(&self, previous: &Self) -> bool {
        self.status != previous.status
            || self.console_users != previous.console_users
            || self.remote_users != previous.remote_users
            || self.error != previous.error
    }
}

/// A single recorded usage observation. The same shape is used for both
/// per-poll samples (every observation) and the on-change history (one per
/// transition). Two SQL tables back the two streams, but the row shape is
/// identical so callers handle one type.
#[derive(Clone, Debug)]
pub struct UsageEvent {
    pub id: Option<i64>,
    pub host_id: String,
    pub collected_at: DateTime<Utc>,
    pub console_users: Option<u32>,
    pub remote_users: Option<u32>,
    pub status: UsageCollectionStatus,
    pub error: Option<String>,
}

/// Per-poll observation. Same row shape as [`UsageHistory`]; kept as an alias
/// so the public storage/query API still distinguishes the two streams by
/// type name where it matters.
pub type UsageSample = UsageEvent;

/// On-change observation (one row per transition). See [`UsageSample`].
pub type UsageHistory = UsageEvent;

#[derive(Clone, Debug)]
pub struct UsageSummary {
    pub hosts_reporting: usize,
    pub hosts_with_errors: usize,
    pub console_users: u32,
    pub remote_users: u32,
}

#[derive(Clone, Debug)]
pub struct HostUsage {
    pub id: String,
    pub address: String,
    pub name: String,
    pub groups: Vec<String>,
    pub usage: UsageSnapshot,
}

#[derive(Clone, Debug)]
pub struct UsageReport {
    pub summary: UsageSummary,
    pub hosts: Vec<HostUsage>,
}
