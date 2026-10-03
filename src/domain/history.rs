//! Mergeable history statistics. Durations are milliseconds; latency is milliseconds.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Inclusive upper bounds for the mergeable latency distribution; the final bin is overflow.
pub const LATENCY_BOUNDS: [f64; 16] = [
    0.5, 1., 2., 4., 8., 16., 32., 64., 128., 256., 512., 1024., 2048., 4096., 8192., 16384.,
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct HistoryStats {
    pub samples: u64,
    pub successful_samples: u64,
    pub latency_count: u64,
    pub latency_sum_ms: f64,
    pub latency_min_ms: Option<f64>,
    pub latency_max_ms: Option<f64>,
    pub latency_histogram: [u64; 17],
    pub up_ms: u64,
    pub down_ms: u64,
    pub usage_observed_ms: u64,
    pub console_user_ms: f64,
    pub remote_user_ms: f64,
}
impl HistoryStats {
    pub fn merge(&mut self, other: &Self) {
        self.samples += other.samples;
        self.successful_samples += other.successful_samples;
        self.latency_count += other.latency_count;
        self.latency_sum_ms += other.latency_sum_ms;
        if let Some(v) = other.latency_min_ms {
            self.latency_min_ms = Some(self.latency_min_ms.map_or(v, |n| n.min(v)));
        }
        if let Some(v) = other.latency_max_ms {
            self.latency_max_ms = Some(self.latency_max_ms.map_or(v, |n| n.max(v)));
        }
        for (a, b) in self
            .latency_histogram
            .iter_mut()
            .zip(other.latency_histogram)
        {
            *a += b;
        }
        self.up_ms += other.up_ms;
        self.down_ms += other.down_ms;
        self.usage_observed_ms += other.usage_observed_ms;
        self.console_user_ms += other.console_user_ms;
        self.remote_user_ms += other.remote_user_ms;
    }
    pub fn latency_p95(&self) -> Option<f64> {
        if self.latency_count == 0 {
            return None;
        }
        let target = (self.latency_count as f64 * 0.95).ceil() as u64;
        let mut count = 0;
        for (i, n) in self.latency_histogram.iter().enumerate() {
            count += n;
            if count >= target {
                return LATENCY_BOUNDS.get(i).copied().or(self.latency_max_ms);
            }
        }
        None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct HistoryObservation {
    pub state: String,
    pub success: bool,
    pub latency_ms: Option<f64>,
    pub console_users: Option<u32>,
    pub remote_users: Option<u32>,
    pub error: Option<String>,
}
impl HistoryObservation {
    pub fn sample_stats(&self) -> HistoryStats {
        let mut s = HistoryStats {
            samples: 1,
            successful_samples: u64::from(self.success),
            ..Default::default()
        };
        if let Some(v) = self
            .latency_ms
            .filter(|v| self.success && v.is_finite() && *v >= 0.)
        {
            s.latency_count = 1;
            s.latency_sum_ms = v;
            s.latency_min_ms = Some(v);
            s.latency_max_ms = Some(v);
            let bin = LATENCY_BOUNDS
                .iter()
                .position(|limit| v <= *limit)
                .unwrap_or(16);
            s.latency_histogram[bin] = 1;
        }
        s
    }
    pub fn duration_stats(&self, duration_ms: u64) -> HistoryStats {
        let mut s = HistoryStats::default();
        if self.state == "up" {
            s.up_ms = duration_ms;
        }
        if self.state == "down" {
            s.down_ms = duration_ms;
        }
        if self.success
            && let (Some(console), Some(remote)) = (self.console_users, self.remote_users)
        {
            s.usage_observed_ms = duration_ms;
            s.console_user_ms = f64::from(console) * duration_ms as f64;
            s.remote_user_ms = f64::from(remote) * duration_ms as f64;
        }
        s
    }
    pub fn changed_from(&self, previous: &Self) -> bool {
        self.state != previous.state
            || self.console_users != previous.console_users
            || self.remote_users != previous.remote_users
            || self.error != previous.error
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HistoryBucket {
    pub start_ms: i64,
    pub end_ms: i64,
    pub eligible_ms: u64,
    pub stats: HistoryStats,
    pub latency_p95_ms: Option<f64>,
}
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HistorySeries {
    pub id: String,
    pub label: String,
    pub buckets: Vec<HistoryBucket>,
}
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HistoryResponse {
    pub from_ms: i64,
    pub to_ms: i64,
    pub resolution_seconds: i64,
    pub source_resolution_seconds: i64,
    pub available_from_ms: Option<i64>,
    pub membership: String,
    pub series: Vec<HistorySeries>,
}
#[derive(Clone, Debug)]
pub struct HistoryRequest {
    pub from_ms: i64,
    pub to_ms: i64,
    pub module: String,
    pub check: Option<String>,
    pub hosts: Vec<String>,
    pub groups: Vec<String>,
    pub breakdown: String,
    pub max_points: usize,
}
impl HistoryRequest {
    pub fn validate(&self, now_ms: i64) -> Result<(), &'static str> {
        if self.from_ms >= now_ms
            || self.from_ms < 0
            || self.to_ms <= self.from_ms
            || self.to_ms > now_ms + 60_000
            || self.to_ms - self.from_ms > 3660 * 86_400_000
        {
            return Err(
                "history requires a valid UTC range no longer than ten years, ending at or before now",
            );
        }
        if !(10..=1000).contains(&self.max_points) {
            return Err("max_points must be between 10 and 1000");
        }
        if !["icmp", "tcp", "usage"].contains(&self.module.as_str())
            || !["all", "group", "host"].contains(&self.breakdown.as_str())
        {
            return Err("invalid history module or breakdown");
        }
        if self.hosts.len() > 16
            || self.groups.len() > 16
            || self
                .hosts
                .iter()
                .chain(&self.groups)
                .any(|v| v.is_empty() || v.len() > 128)
        {
            return Err("select at most 16 hosts and 16 groups");
        }
        if self
            .check
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 128)
        {
            return Err("invalid check id");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HistoryEvent {
    pub id: i64,
    pub host_id: String,
    pub name: String,
    pub groups: Vec<String>,
    pub module: String,
    pub check_id: String,
    pub at_ms: i64,
    pub previous_state: Option<String>,
    pub observation: HistoryObservation,
}
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HistoryEventsResponse {
    pub events: Vec<HistoryEvent>,
    pub next_before: Option<i64>,
}
