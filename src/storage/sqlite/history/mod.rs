//! Durable observations, mergeable summaries, and versioned series identities.
mod ingest;
mod query;
mod rollup;
use super::*;
use crate::domain::{history::*, maintenance::HistoryConfig};
pub(super) use ingest::{record, sync_epochs};
pub(super) use rollup::{prune, rollup};
const TIERS: [i64; 3] = [300, 3600, 86400];

pub(super) fn migrate(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS history_epochs (
        id INTEGER PRIMARY KEY, host_id TEXT NOT NULL, name TEXT NOT NULL, groups_json TEXT NOT NULL,
        module TEXT NOT NULL, check_id TEXT NOT NULL, signature TEXT NOT NULL,
        started_ms INTEGER NOT NULL, ended_ms INTEGER, freshness_ms INTEGER NOT NULL,
        last_ms INTEGER, last_json TEXT
    );
    CREATE UNIQUE INDEX IF NOT EXISTS history_active ON history_epochs(host_id,module,check_id) WHERE ended_ms IS NULL;
    CREATE INDEX IF NOT EXISTS history_epoch_time ON history_epochs(module,started_ms,ended_ms);
    CREATE INDEX IF NOT EXISTS history_epoch_host_time ON history_epochs(module,host_id,started_ms,ended_ms);
    CREATE TABLE IF NOT EXISTS history_raw (
        id INTEGER PRIMARY KEY, epoch_id INTEGER NOT NULL REFERENCES history_epochs(id), at_ms INTEGER NOT NULL, observation TEXT NOT NULL,
        UNIQUE(epoch_id,at_ms)
    );
    CREATE INDEX IF NOT EXISTS history_raw_time ON history_raw(at_ms);
    CREATE TABLE IF NOT EXISTS history_buckets (
        epoch_id INTEGER NOT NULL REFERENCES history_epochs(id), resolution INTEGER NOT NULL, start_ms INTEGER NOT NULL, stats TEXT NOT NULL,
        PRIMARY KEY(epoch_id,resolution,start_ms)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS history_bucket_time ON history_buckets(resolution,start_ms);
    CREATE TABLE IF NOT EXISTS history_spans (
        id INTEGER PRIMARY KEY, epoch_id INTEGER NOT NULL REFERENCES history_epochs(id), cursor_ms INTEGER NOT NULL,end_ms INTEGER NOT NULL, observation TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS history_span_time ON history_spans(cursor_ms);
    CREATE TABLE IF NOT EXISTS history_events (
        id INTEGER PRIMARY KEY,epoch_id INTEGER NOT NULL REFERENCES history_epochs(id),at_ms INTEGER NOT NULL,previous_state TEXT,observation TEXT NOT NULL,
        UNIQUE(epoch_id,at_ms)
    );
    CREATE INDEX IF NOT EXISTS history_event_time ON history_events(at_ms);")?;
    Ok(())
}
fn add_stats(
    conn: &Connection,
    epoch: i64,
    resolution: i64,
    start: i64,
    stats: &HistoryStats,
) -> Result<(), StorageError> {
    let previous: Option<String> = conn
        .query_row(
            "SELECT stats FROM history_buckets WHERE epoch_id=?1 AND resolution=?2 AND start_ms=?3",
            params![epoch, resolution, start],
            |r| r.get(0),
        )
        .optional()?;
    let mut next: HistoryStats = previous
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default();
    next.merge(stats);
    conn.execute("INSERT INTO history_buckets VALUES (?1,?2,?3,?4) ON CONFLICT(epoch_id,resolution,start_ms) DO UPDATE SET stats=excluded.stats",params![epoch,resolution,start,serde_json::to_string(&next)?])?;
    Ok(())
}
fn floor(at: i64, seconds: i64) -> i64 {
    at.div_euclid(seconds * 1000) * (seconds * 1000)
}
fn cutoff(now: i64, duration: Duration) -> i64 {
    now.saturating_sub(duration.as_millis() as i64)
}
fn retention(config: &HistoryConfig, resolution: i64) -> Duration {
    match resolution {
        300 => config.five_minute_retention,
        3600 => config.hourly_retention,
        _ => config.daily_retention,
    }
}

#[cfg(test)]
mod tests;
