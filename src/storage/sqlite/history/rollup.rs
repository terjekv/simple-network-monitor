use super::*;

/// Materialize duration spans with a durable cursor. All resolutions are updated atomically.
pub(in crate::storage::sqlite) fn rollup(
    conn: &Connection,
    now: i64,
    config: &HistoryConfig,
    limit: usize,
) -> Result<(u64, bool), StorageError> {
    let mut work = 0;
    while work < limit as u64 {
        let span: Option<(i64, i64, i64, i64, String)> = conn.query_row("SELECT id,epoch_id,cursor_ms,end_ms,observation FROM history_spans ORDER BY id LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?;
        let Some((id, epoch, start, end, body)) = span else {
            break;
        };
        let start = start.max(floor(cutoff(now, config.daily_retention), 86400));
        if start >= end {
            conn.execute("DELETE FROM history_spans WHERE id=?1", [id])?;
            work += 1;
            continue;
        }
        let smallest = if start >= floor(cutoff(now, config.five_minute_retention), 300) {
            300
        } else if start >= floor(cutoff(now, config.hourly_retention), 3600) {
            3600
        } else {
            86400
        };
        let mut until = end.min(floor(start, smallest) + smallest * 1000);
        for res in TIERS {
            let boundary = floor(cutoff(now, retention(config, res)), res);
            if boundary > start {
                until = until.min(boundary);
            }
        }
        let observation: HistoryObservation = serde_json::from_str(&body)?;
        let stats = observation.duration_stats((until - start) as u64);
        for resolution in TIERS {
            if start >= floor(cutoff(now, retention(config, resolution)), resolution) {
                add_stats(conn, epoch, resolution, floor(start, resolution), &stats)?;
            }
        }
        if until == end {
            conn.execute("DELETE FROM history_spans WHERE id=?1", [id])?;
        } else {
            conn.execute(
                "UPDATE history_spans SET cursor_ms=?2 WHERE id=?1",
                params![id, until],
            )?;
        }
        work += 1;
    }
    let more = conn.query_row("SELECT EXISTS(SELECT 1 FROM history_spans)", [], |r| {
        r.get(0)
    })?;
    Ok((work, more))
}
pub(in crate::storage::sqlite) fn prune(
    conn: &Connection,
    now: i64,
    config: &HistoryConfig,
    usage_retention: Duration,
    limit: usize,
) -> Result<(u64, bool), StorageError> {
    let mut removed = 0;
    for (sql, at) in [
        (
            concat!(
                "DELETE FROM history_raw WHERE id IN (SELECT r.id FROM history_raw r JOIN ",
                "history_epochs e ON e.id=r.epoch_id WHERE e.module!='usage' AND r.at_ms<?1 LIMIT ?2)"
            ),
            cutoff(now, config.raw_retention),
        ),
        (
            concat!(
                "DELETE FROM history_raw WHERE id IN (SELECT r.id FROM history_raw r JOIN ",
                "history_epochs e ON e.id=r.epoch_id WHERE e.module='usage' AND r.at_ms<?1 LIMIT ?2)"
            ),
            cutoff(now, usage_retention),
        ),
        (
            "DELETE FROM history_events WHERE id IN (SELECT id FROM history_events WHERE at_ms<?1 LIMIT ?2)",
            cutoff(now, config.event_retention),
        ),
    ] {
        removed += conn.execute(sql, params![at, limit as i64])?;
    }
    for resolution in TIERS {
        removed += conn.execute(concat!("DELETE FROM history_buckets WHERE (epoch_id,resolution,start_ms) IN (SELECT ", "epoch_id,resolution,start_ms FROM history_buckets WHERE resolution=?1 AND start_ms<?2 ", "LIMIT ?3)"), params![resolution, floor(cutoff(now, retention(config, resolution)), resolution), limit as i64])?;
    }
    removed += conn.execute(
        concat!(
            "DELETE FROM history_epochs WHERE id IN (SELECT e.id FROM history_epochs e WHERE ",
            "ended_ms<?1 AND NOT EXISTS(SELECT 1 FROM history_raw WHERE epoch_id=e.id) AND NOT ",
            "EXISTS(SELECT 1 FROM history_buckets WHERE epoch_id=e.id) AND NOT EXISTS(SELECT 1 ",
            "FROM history_spans WHERE epoch_id=e.id) AND NOT EXISTS(SELECT 1 FROM history_events ",
            "WHERE epoch_id=e.id) LIMIT ?2)"
        ),
        params![
            cutoff(now, config.daily_retention.max(config.event_retention)),
            limit as i64
        ],
    )?;
    Ok((removed as u64, removed >= limit))
}
