use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, num::NonZeroU16, time::Duration};

/// A stable check ID within one host's TCP module.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CheckId(String);

impl CheckId {
    pub fn new(value: String) -> Result<Self, &'static str> {
        if value.is_empty()
            || value.len() > 128
            || matches!(value.as_str(), "." | "..")
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        {
            return Err(
                "check ID must contain 1..128 ASCII letters, digits, dash, underscore, or dot",
            );
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CheckId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CheckId> for String {
    fn from(value: CheckId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TcpCheckConfig {
    pub id: CheckId,
    pub port: NonZeroU16,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedTcpHostConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub checks: Vec<TcpCheckConfig>,
}

impl Default for ResolvedTcpHostConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(3),
            checks: Vec::new(),
        }
    }
}

impl ResolvedTcpHostConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        super::validation::validate_duration(self.interval)?;
        super::validation::validate_duration(self.timeout)?;
        if self.checks.len() > 64 {
            return Err("at most 64 TCP checks are allowed per host");
        }
        let mut ids = HashSet::new();
        for check in &self.checks {
            if !ids.insert(&check.id) {
                return Err("duplicate TCP check ID");
            }
        }
        Ok(())
    }
}

/// A completed TCP attempt; success and diagnostic state cannot disagree.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TcpSnapshot {
    observed_at: DateTime<Utc>,
    result: Result<Duration, String>,
}

impl TcpSnapshot {
    pub fn new(observed_at: DateTime<Utc>, result: Result<Duration, String>) -> Self {
        Self {
            observed_at,
            result,
        }
    }
    pub fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }
    pub fn success(&self) -> bool {
        self.result.is_ok()
    }
    pub fn duration(&self) -> Option<Duration> {
        self.result.as_ref().ok().copied()
    }
    pub fn error(&self) -> Option<&str> {
        self.result.as_ref().err().map(String::as_str)
    }
}
