use std::{collections::BTreeMap, sync::Mutex, time::Duration};

pub const DURATION_BUCKETS: [f64; 7] = [0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

#[derive(Clone, Default)]
pub struct CheckStats {
    pub successes: u64,
    pub failures: u64,
    pub storage_retries: u64,
    pub duration_sum: f64,
    pub buckets: [u64; DURATION_BUCKETS.len()],
}

/// Process-lifetime counters survive configuration reloads, but not restarts.
#[derive(Default)]
pub struct RuntimeMetrics {
    checks: Mutex<BTreeMap<&'static str, CheckStats>>,
}

impl RuntimeMetrics {
    pub fn observe(&self, kind: &'static str, success: bool, duration: Duration) {
        let mut checks = self.checks.lock().unwrap_or_else(|err| err.into_inner());
        let stats = checks.entry(kind).or_default();
        if success {
            stats.successes += 1;
        } else {
            stats.failures += 1;
        }
        let seconds = duration.as_secs_f64();
        stats.duration_sum += seconds;
        for (index, boundary) in DURATION_BUCKETS.iter().enumerate() {
            if seconds <= *boundary {
                stats.buckets[index] += 1;
            }
        }
    }

    pub fn storage_retry(&self, kind: &'static str) {
        self.checks
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .entry(kind)
            .or_default()
            .storage_retries += 1;
    }

    pub fn snapshot(&self) -> BTreeMap<&'static str, CheckStats> {
        self.checks
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }
}
