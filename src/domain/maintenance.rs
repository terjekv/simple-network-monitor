//! Maintenance configuration and observable job state.
use serde::{Deserialize, Serialize};
use std::time::Duration;
use utoipa::ToSchema;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    #[serde(with = "humantime_serde")]
    pub raw_retention: Duration,
    #[serde(with = "humantime_serde")]
    pub five_minute_retention: Duration,
    #[serde(with = "humantime_serde")]
    pub hourly_retention: Duration,
    #[serde(with = "humantime_serde")]
    pub daily_retention: Duration,
    #[serde(with = "humantime_serde")]
    pub event_retention: Duration,
}
impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            raw_retention: Duration::from_secs(3 * 86400),
            five_minute_retention: Duration::from_secs(30 * 86400),
            hourly_retention: Duration::from_secs(365 * 86400),
            daily_retention: Duration::from_secs(1095 * 86400),
            event_retention: Duration::from_secs(1095 * 86400),
        }
    }
}
impl HistoryConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        let tiers = [
            self.raw_retention,
            self.five_minute_retention,
            self.hourly_retention,
            self.daily_retention,
        ];
        for duration in tiers.into_iter().chain([self.event_retention]) {
            super::validation::validate_duration(duration)?;
            if duration < Duration::from_secs(86400) {
                return Err("history retention must be at least one day");
            }
        }
        if tiers.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err("history retention must increase as resolution becomes coarser");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaintenanceConfig {
    #[serde(with = "humantime_serde")]
    pub rollup_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub retention_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub reclaim_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub optimize_interval: Duration,
    pub batch_size: usize,
    pub reclaim_min_bytes: u64,
    pub reclaim_free_percent: u8,
    pub reclaim_pages: u32,
}
impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            rollup_interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(300),
            reclaim_interval: Duration::from_secs(3600),
            optimize_interval: Duration::from_secs(86400),
            batch_size: 500,
            reclaim_min_bytes: 256 * 1024 * 1024,
            reclaim_free_percent: 20,
            reclaim_pages: 1000,
        }
    }
}
impl MaintenanceConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        for v in [
            self.rollup_interval,
            self.retention_interval,
            self.reclaim_interval,
            self.optimize_interval,
        ] {
            super::validation::validate_duration(v)?;
            if v < Duration::from_secs(1) {
                return Err("maintenance intervals must be at least one second");
            }
        }
        if self.reclaim_min_bytes > i64::MAX as u64 {
            return Err("reclaim_min_bytes exceeds the supported range");
        }
        if !(1..=5000).contains(&self.batch_size)
            || !(1..=10000).contains(&self.reclaim_pages)
            || !(1..=90).contains(&self.reclaim_free_percent)
        {
            return Err("invalid maintenance batch size, page limit or free-space percentage");
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
pub enum JobKind {
    Rollup,
    Retention,
    Reclaim,
    Optimize,
    Checkpoint,
    JobHistory,
}
impl JobKind {
    pub const ALL: [Self; 6] = [
        Self::Rollup,
        Self::Retention,
        Self::Reclaim,
        Self::Optimize,
        Self::Checkpoint,
        Self::JobHistory,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::Rollup => "history_rollup",
            Self::Retention => "retention",
            Self::Reclaim => "space_reclamation",
            Self::Optimize => "query_optimization",
            Self::Checkpoint => "wal_checkpoint",
            Self::JobHistory => "job_history_cleanup",
        }
    }
    pub fn interval(self, config: &MaintenanceConfig) -> Duration {
        match self {
            Self::Rollup => config.rollup_interval,
            Self::Retention => config.retention_interval,
            Self::Reclaim => config.reclaim_interval,
            Self::Optimize => config.optimize_interval,
            Self::Checkpoint => Duration::from_secs(300),
            Self::JobHistory => Duration::from_secs(86400),
        }
    }
}
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct MaintenanceJobStatus {
    pub id: String,
    pub status: String,
    pub last_started_ms: Option<i64>,
    pub last_success_ms: Option<i64>,
    pub next_run_ms: i64,
    pub duration_ms: Option<i64>,
    pub work_done: u64,
    pub failures: u32,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DatabaseStatus {
    pub allocated_bytes: u64,
    pub reusable_bytes: u64,
    pub wal_bytes: u64,
    pub incremental_vacuum: bool,
    pub pending_spans: u64,
    pub oldest_pending_ms: Option<i64>,
}
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct MaintenanceStatus {
    pub database: DatabaseStatus,
    pub jobs: Vec<MaintenanceJobStatus>,
}
