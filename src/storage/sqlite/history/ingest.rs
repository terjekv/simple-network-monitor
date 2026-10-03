use super::*;

pub(in crate::storage::sqlite) fn sync_epochs(
    conn: &Connection,
    hosts: &[Host],
    now: i64,
) -> Result<(), StorageError> {
    let mut active = HashSet::new();
    for host in hosts {
        let mut checks = Vec::new();
        if host.modules.icmp.enabled {
            checks.push((
                "icmp",
                "".to_owned(),
                host.modules.icmp.interval,
                host.modules.icmp.timeout,
                0,
            ));
        }
        if host.modules.usage.enabled {
            checks.push((
                "usage",
                "".to_owned(),
                host.modules.usage.interval,
                host.modules.usage.timeout,
                0,
            ));
        }
        if host.modules.tcp.enabled {
            for check in &host.modules.tcp.checks {
                checks.push((
                    "tcp",
                    check.id.as_str().to_owned(),
                    host.modules.tcp.interval,
                    host.modules.tcp.timeout,
                    check.port.get(),
                ));
            }
        }
        for (module, check, interval, timeout, port) in checks {
            let mut groups = host.groups.clone();
            groups.sort();
            groups.dedup();
            let groups_json = serde_json::to_string(&groups)?;
            let signature = serde_json::to_string(&(
                host.address.clone(),
                host.name.clone(),
                groups.clone(),
                interval.as_millis(),
                timeout.as_millis(),
                port,
            ))?;
            let prior: Option<(i64, String)> = conn.query_row("SELECT id,signature FROM history_epochs WHERE host_id=?1 AND module=?2 AND check_id=?3 AND ended_ms IS NULL", params![host.id, module, check], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let id = if let Some((id, old)) = prior.as_ref().filter(|(_, old)| old == &signature) {
                let _ = old;
                *id
            } else {
                if let Some((id, _)) = prior {
                    close_epoch(conn, id, now)?;
                }
                let fresh = (interval
                    .saturating_mul(2)
                    .saturating_add(timeout)
                    .as_millis())
                .min(i64::MAX as u128) as i64;
                conn.execute("INSERT INTO history_epochs(host_id,name,groups_json,module,check_id,signature,started_ms,freshness_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![host.id, host.name, groups_json, module, check, signature, now, fresh])?;
                conn.last_insert_rowid()
            };
            active.insert(id);
        }
    }
    let ids = conn
        .prepare("SELECT id FROM history_epochs WHERE ended_ms IS NULL")?
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for id in ids {
        if !active.contains(&id) {
            close_epoch(conn, id, now)?;
        }
    }
    Ok(())
}
fn close_epoch(conn: &Connection, id: i64, now: i64) -> Result<(), StorageError> {
    flush_tail(conn, id, now)?;
    conn.execute("UPDATE history_epochs SET ended_ms=MAX(started_ms,?2),last_ms=NULL,last_json=NULL WHERE id=?1", params![id, now])?;
    Ok(())
}
pub(in crate::storage::sqlite) fn flush_tail(
    conn: &Connection,
    epoch: i64,
    end: i64,
) -> Result<(), StorageError> {
    let (last, body, fresh): (Option<i64>, Option<String>, i64) = conn.query_row(
        "SELECT last_ms,last_json,freshness_ms FROM history_epochs WHERE id=?1",
        [epoch],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if let (Some(start), Some(body)) = (last, body) {
        let end = end.min(start.saturating_add(fresh));
        if end > start {
            conn.execute("INSERT INTO history_spans(epoch_id,cursor_ms,end_ms,observation) VALUES (?1,?2,?3,?4)", params![epoch, start, end, body])?;
        }
    }
    Ok(())
}
pub(in crate::storage::sqlite) fn record(
    conn: &Connection,
    host: &str,
    module: &str,
    check: &str,
    at: i64,
    observation: HistoryObservation,
    now: i64,
) -> Result<(), StorageError> {
    // Future and out-of-order observations must never advance history or retention.
    if at > now {
        return Ok(());
    }
    let epoch: Option<(i64, i64, Option<i64>, Option<String>)> = conn.query_row("SELECT id,started_ms,last_ms,last_json FROM history_epochs WHERE host_id=?1 AND module=?2 AND check_id=?3 AND ended_ms IS NULL", params![host, module, check], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
    let Some((id, start, last, previous)) = epoch else {
        return Ok(());
    };
    if at < start || last.is_some_and(|last| at <= last) {
        return Ok(());
    }
    let body = serde_json::to_string(&observation)?;
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO history_raw(epoch_id,at_ms,observation) VALUES (?1,?2,?3)",
        params![id, at, body],
    )?;
    if inserted == 0 {
        return Ok(());
    }
    flush_tail(conn, id, at)?;
    let prior: Option<HistoryObservation> =
        previous.map(|s| serde_json::from_str(&s)).transpose()?;
    if prior.as_ref().is_none_or(|p| observation.changed_from(p)) {
        conn.execute("INSERT OR IGNORE INTO history_events(epoch_id,at_ms,previous_state,observation) VALUES (?1,?2,?3,?4)", params![id, at, prior.map(|p| p.state), body])?;
    }
    let stats = observation.sample_stats();
    for resolution in TIERS {
        add_stats(conn, id, resolution, floor(at, resolution), &stats)?;
    }
    conn.execute(
        "UPDATE history_epochs SET last_ms=?2,last_json=?3 WHERE id=?1",
        params![id, at, body],
    )?;
    Ok(())
}
