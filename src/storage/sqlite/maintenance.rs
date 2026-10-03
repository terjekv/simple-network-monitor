use super::*;

pub(super) fn checked_cutoff(
    now: DateTime<Utc>,
    duration: Duration,
) -> Result<String, StorageError> {
    let delta = chrono::Duration::from_std(duration)
        .map_err(|_| StorageError::InvalidData("invalid retention duration".into()))?;
    now.checked_sub_signed(delta)
        .map(|value| value.to_rfc3339())
        .ok_or_else(|| StorageError::InvalidData("duration exceeds supported date range".into()))
}

pub(super) fn update_coverage(
    conn: &Connection,
    host: &Host,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    let previous: Option<(String, String)> = conn
        .query_row(
            "SELECT started_at, observed_at FROM usage_coverage WHERE host_id = ?1",
            [&host.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let observed = snapshot.collected_at.to_rfc3339();
    let started = previous
        .and_then(|(start, last)| {
            DateTime::parse_from_rfc3339(&last)
                .ok()
                .filter(|last| {
                    crate::domain::host::is_fresh(
                        Some(last.with_timezone(&Utc)),
                        host.modules.usage.interval,
                        host.modules.usage.timeout,
                        snapshot.collected_at,
                    )
                })
                .map(|_| start)
        })
        .unwrap_or_else(|| observed.clone());
    conn.execute("INSERT INTO usage_coverage(host_id, started_at, observed_at) VALUES (?1, ?2, ?3) ON CONFLICT(host_id) DO UPDATE SET started_at=excluded.started_at, observed_at=excluded.observed_at", params![host.id, started, observed])?;
    Ok(())
}

/// Reset current observations when a host ID changes meaning, including across restarts.
pub(super) fn sync_identities(conn: &Connection, hosts: &[Host]) -> Result<(), StorageError> {
    let txn = conn.unchecked_transaction()?;
    sync_identities_in_transaction(&txn, hosts)?;
    txn.commit()?;
    Ok(())
}

pub(super) fn sync_identities_in_transaction(
    txn: &Connection,
    hosts: &[Host],
) -> Result<(), StorageError> {
    txn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS incoming_hosts (host_id TEXT PRIMARY KEY); DELETE FROM incoming_hosts;")?;
    for host in hosts {
        host.validate()
            .map_err(|error| StorageError::InvalidData(error.into()))?;
        txn.execute("INSERT INTO incoming_hosts VALUES (?1)", [&host.id])?;
        let prior: Option<(String, bool)> = txn
            .query_row(
                "SELECT address, usage_enabled FROM host_identity WHERE host_id = ?1",
                [&host.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if prior
            .as_ref()
            .is_none_or(|(address, _)| *address != host.address)
        {
            txn.execute("DELETE FROM latest_status WHERE host_id = ?1", [&host.id])?;
            txn.execute("DELETE FROM latest_usage WHERE host_id = ?1", [&host.id])?;
            txn.execute("DELETE FROM usage_coverage WHERE host_id = ?1", [&host.id])?;
        } else if !host.modules.usage.enabled || prior.is_some_and(|(_, enabled)| !enabled) {
            txn.execute("DELETE FROM usage_coverage WHERE host_id = ?1", [&host.id])?;
        }
        txn.execute("INSERT INTO host_identity VALUES (?1, ?2, ?3) ON CONFLICT(host_id) DO UPDATE SET address=excluded.address, usage_enabled=excluded.usage_enabled", params![host.id, host.address, host.modules.usage.enabled])?;
    }
    for table in [
        "latest_status",
        "latest_usage",
        "usage_coverage",
        "host_identity",
    ] {
        txn.execute(
            &format!(
                "DELETE FROM {table} WHERE host_id NOT IN (SELECT host_id FROM incoming_hosts)"
            ),
            [],
        )?;
    }
    super::tcp::sync_checks(txn, hosts)?;
    super::history::sync_epochs(txn, hosts, Utc::now().timestamp_millis())?;
    Ok(())
}
