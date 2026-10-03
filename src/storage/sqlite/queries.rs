use super::*;

pub(super) fn host_record(conn: &Connection, host: &Host) -> Result<HostRecord, StorageError> {
    let state = load_latest_state(conn, &host.id)?.unwrap_or_default();
    let usage = load_latest_usage(conn, &host.id)?;
    let mut record = state.to_record(host, usage);
    record.tcp = super::tcp::load_host(conn, host)?;
    Ok(record)
}

pub(super) fn sorted_records(
    conn: &Connection,
    hosts: &BTreeMap<String, Host>,
    filter: &HostFilter,
) -> Result<Vec<HostRecord>, StorageError> {
    let mut states = load_all_latest_states(conn)?;
    let usage = load_all_latest_usage(conn)?;
    let mut tcp = super::tcp::load_all(conn, hosts)?;
    let mut records = Vec::new();
    for host in hosts.values() {
        let state = states.remove(&host.id).unwrap_or_default();
        let mut record = state.to_record(host, usage.get(&host.id).cloned());
        record.tcp = tcp.remove(&host.id).unwrap_or_default();
        if matches_host_filter(conn, &record, filter)? {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.host.id.cmp(&b.host.id));
    Ok(records)
}

pub(super) fn matches_host_filter(
    conn: &Connection,
    record: &HostRecord,
    filter: &HostFilter,
) -> Result<bool, StorageError> {
    if let Some(status) = &filter.icmp_status
        && &record.state.status != status
    {
        return Ok(false);
    }
    if let Some(group) = &filter.group
        && !record
            .host
            .groups
            .iter()
            .any(|candidate| candidate == group)
    {
        return Ok(false);
    }
    if !filter.metadata.is_empty()
        && !filter.metadata.iter().all(|(key, expected)| {
            record
                .host
                .metadata
                .get(key)
                .is_some_and(|actual| metadata_value_matches(actual, expected))
        })
    {
        return Ok(false);
    }
    if !filter.usage.is_empty() {
        let Some(snapshot) = &record.usage else {
            return Ok(false);
        };
        if !matches_usage_filter(conn, &record.host, snapshot, &filter.usage)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn metadata_value_matches(actual: &Value, expected: &str) -> bool {
    match actual {
        Value::String(value) => value == expected,
        Value::Bool(value) => value.to_string() == expected,
        Value::Number(value) => value.to_string() == expected,
        _ => false,
    }
}

pub(super) fn usage_summary<'a>(
    snapshots: impl IntoIterator<Item = &'a UsageSnapshot>,
) -> UsageSummary {
    let mut summary = UsageSummary {
        hosts_reporting: 0,
        hosts_with_errors: 0,
        console_users: 0,
        remote_users: 0,
    };
    for snapshot in snapshots {
        summary.hosts_reporting += 1;
        match snapshot.status {
            UsageCollectionStatus::Ok => {
                summary.console_users += u64::from(snapshot.console_users.unwrap_or(0));
                summary.remote_users += u64::from(snapshot.remote_users.unwrap_or(0));
            }
            UsageCollectionStatus::Failed => {
                summary.hosts_with_errors += 1;
            }
        }
    }
    summary
}

pub(super) fn matches_usage_filter(
    conn: &Connection,
    host: &Host,
    snapshot: &UsageSnapshot,
    filter: &UsageFilter,
) -> Result<bool, StorageError> {
    if let Some(status) = &filter.status
        && &snapshot.status != status
    {
        return Ok(false);
    }
    let host_id = host.id.as_str();
    let history = SqliteInactivityHistory { conn, host_id };
    if filter.inactive_console_for.is_some() || filter.no_users_for.is_some() {
        if !host.modules.usage.enabled
            || !crate::domain::host::is_fresh(
                Some(snapshot.collected_at),
                host.modules.usage.interval,
                host.modules.usage.timeout,
                filter.now,
            )
        {
            return Ok(false);
        }
        let window = filter
            .inactive_console_for
            .into_iter()
            .chain(filter.no_users_for)
            .max()
            .unwrap_or_default();
        let cutoff = checked_cutoff(filter.now, window)?;
        let covered: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_coverage WHERE host_id = ?1 AND started_at <= ?2 AND NOT EXISTS(SELECT 1 FROM retention_floor WHERE cutoff > ?2))",
            params![host_id, cutoff],
            |row| row.get(0),
        )?;
        if !covered {
            return Ok(false);
        }
    }
    if let Some(duration) = filter.inactive_console_for {
        let inactive = crate::domain::evaluate_inactivity(
            snapshot,
            duration,
            filter.now,
            crate::domain::ActivityPredicate::ConsoleOnly,
            &history,
        )
        .map_err(map_inactivity_error)?;
        if !inactive {
            return Ok(false);
        }
    }
    if let Some(duration) = filter.no_users_for {
        let inactive = crate::domain::evaluate_inactivity(
            snapshot,
            duration,
            filter.now,
            crate::domain::ActivityPredicate::AnyUser,
            &history,
        )
        .map_err(map_inactivity_error)?;
        if !inactive {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn map_inactivity_error(
    err: crate::domain::InactivityError<StorageError>,
) -> StorageError {
    match err {
        crate::domain::InactivityError::InvalidDuration(msg) => {
            StorageError::InvalidData(format!("invalid inactivity duration: {msg}"))
        }
        crate::domain::InactivityError::Storage(e) => e,
    }
}

/// SQL implementation of the [`InactivityHistory`](crate::domain::InactivityHistory)
/// trait — every method is a single SELECT against `usage_history`.
pub(super) struct SqliteInactivityHistory<'a> {
    conn: &'a Connection,
    host_id: &'a str,
}

impl crate::domain::InactivityHistory for SqliteInactivityHistory<'_> {
    type Error = StorageError;

    fn state_at_or_before(
        &self,
        cutoff: DateTime<Utc>,
        predicate: crate::domain::ActivityPredicate,
    ) -> Result<Option<crate::domain::StateAtCutoff>, StorageError> {
        let pred = activity_sql(predicate);
        let sql = format!(
            r#"
SELECT status, CASE WHEN {pred} THEN 1 ELSE 0 END AS activity
FROM usage_history
WHERE host_id = ?1 AND collected_at <= ?2
ORDER BY collected_at DESC, id DESC
LIMIT 1
"#
        );
        let row: Option<(String, i64)> = self
            .conn
            .query_row(&sql, params![self.host_id, cutoff.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?;
        Ok(row.map(|(status, activity)| match status.as_str() {
            "ok" => crate::domain::StateAtCutoff::Ok {
                activity_matched: activity != 0,
            },
            _ => crate::domain::StateAtCutoff::Unknown,
        }))
    }

    fn window_breaks_inactivity(
        &self,
        cutoff: DateTime<Utc>,
        predicate: crate::domain::ActivityPredicate,
    ) -> Result<bool, StorageError> {
        let pred = activity_sql(predicate);
        let sql = format!(
            r#"
SELECT EXISTS (
    SELECT 1
    FROM usage_history
    WHERE host_id = ?1
      AND collected_at > ?2
      AND (status != 'ok' OR ({pred}))
)
"#
        );
        let exists: i64 =
            self.conn
                .query_row(&sql, params![self.host_id, cutoff.to_rfc3339()], |row| {
                    row.get(0)
                })?;
        Ok(exists != 0)
    }
}

pub(super) fn activity_sql(predicate: crate::domain::ActivityPredicate) -> &'static str {
    match predicate {
        crate::domain::ActivityPredicate::ConsoleOnly => "console_users > 0",
        crate::domain::ActivityPredicate::AnyUser => "console_users > 0 OR remote_users > 0",
    }
}

pub(super) fn upsert_latest_usage(
    conn: &Connection,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    conn.execute(
        r#"
INSERT INTO latest_usage (
    host_id, collected_at, console_users, remote_users, status, error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(host_id) DO UPDATE SET
    collected_at = excluded.collected_at,
    console_users = excluded.console_users,
    remote_users = excluded.remote_users,
    status = excluded.status,
    error = excluded.error
"#,
        params![
            host_id,
            snapshot.collected_at.to_rfc3339(),
            snapshot.console_users,
            snapshot.remote_users,
            snapshot.status.as_str(),
            snapshot.error,
        ],
    )?;
    Ok(())
}

pub(super) fn insert_usage_sample(
    conn: &Connection,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    insert_usage_row(conn, UsageTable::Samples, host_id, snapshot)
}

pub(super) fn insert_usage_history(
    conn: &Connection,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    insert_usage_row(conn, UsageTable::History, host_id, snapshot)
}

#[derive(Clone, Copy)]
pub(super) enum UsageTable {
    Samples,
    History,
}

impl UsageTable {
    fn name(self) -> &'static str {
        match self {
            Self::Samples => "usage_samples",
            Self::History => "usage_history",
        }
    }
}

pub(super) fn insert_usage_row(
    conn: &Connection,
    table: UsageTable,
    host_id: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), StorageError> {
    let table = table.name();
    let sql = format!(
        r#"
INSERT INTO {table} (
    host_id, collected_at, console_users, remote_users, status, error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
"#
    );
    conn.execute(
        &sql,
        params![
            host_id,
            snapshot.collected_at.to_rfc3339(),
            snapshot.console_users,
            snapshot.remote_users,
            snapshot.status.as_str(),
            snapshot.error,
        ],
    )?;
    Ok(())
}

pub(super) fn load_usage_history(
    conn: &Connection,
    host_id: &str,
    limit: usize,
) -> Result<Vec<UsageHistory>, StorageError> {
    load_usage_rows(conn, UsageTable::History, host_id, limit)
}

pub(super) fn load_usage_samples(
    conn: &Connection,
    host_id: &str,
    limit: usize,
) -> Result<Vec<UsageSample>, StorageError> {
    load_usage_rows(conn, UsageTable::Samples, host_id, limit)
}

pub(super) fn load_usage_rows<T>(
    conn: &Connection,
    table: UsageTable,
    host_id: &str,
    limit: usize,
) -> Result<Vec<T>, StorageError>
where
    T: TryFrom<UsageRow, Error = StorageError>,
{
    let table = table.name();
    let sql = format!(
        r#"
SELECT id, host_id, collected_at, console_users, remote_users, status, error
FROM {table}
WHERE host_id = ?1
ORDER BY collected_at DESC, id DESC
LIMIT ?2
"#
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![host_id, limit as i64], |row| {
        Ok(UsageRow {
            id: row.get(0)?,
            host_id: row.get(1)?,
            collected_at: row.get(2)?,
            console_users: row.get(3)?,
            remote_users: row.get(4)?,
            status: row.get(5)?,
            error: row.get(6)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?.try_into()?);
    }
    Ok(events)
}

pub(super) fn upsert_latest_state(
    conn: &Connection,
    host_id: &str,
    state: &HostRuntimeState,
) -> Result<(), StorageError> {
    conn.execute(
        r#"
INSERT INTO latest_status (
    host_id, status, last_checked_at, last_change_at, latency_ms,
    consecutive_successes, consecutive_failures, last_error
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
ON CONFLICT(host_id) DO UPDATE SET
    status = excluded.status,
    last_checked_at = excluded.last_checked_at,
    last_change_at = excluded.last_change_at,
    latency_ms = excluded.latency_ms,
    consecutive_successes = excluded.consecutive_successes,
    consecutive_failures = excluded.consecutive_failures,
    last_error = excluded.last_error
"#,
        params![
            host_id,
            state.status.as_str(),
            state.last_checked_at.map(|ts| ts.to_rfc3339()),
            state.last_change_at.map(|ts| ts.to_rfc3339()),
            state.latency.map(crate::domain::duration_ms),
            state.consecutive_successes,
            state.consecutive_failures,
            state.last_error,
        ],
    )?;
    Ok(())
}

pub(super) fn load_latest_state(
    conn: &Connection,
    host_id: &str,
) -> Result<Option<HostRuntimeState>, StorageError> {
    let row = conn
        .query_row(
            r#"
SELECT status, last_checked_at, last_change_at, latency_ms,
       consecutive_successes, consecutive_failures, last_error
FROM latest_status
WHERE host_id = ?1
"#,
            params![host_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(status, last_checked_at, last_change_at, latency_ms, succ, fail, last_err)| {
            runtime_state_from_row(
                status,
                last_checked_at,
                last_change_at,
                latency_ms,
                succ,
                fail,
                last_err,
            )
        },
    )
    .transpose()
}

/// Bulk-load every persisted latest_status row in one SELECT — one round-trip
/// instead of N. Hosts without a row are absent from the map; the caller
/// supplies the default.
pub(super) fn load_all_latest_states(
    conn: &Connection,
) -> Result<HashMap<String, HostRuntimeState>, StorageError> {
    let mut stmt = conn.prepare(
        r#"
SELECT host_id, status, last_checked_at, last_change_at, latency_ms,
       consecutive_successes, consecutive_failures, last_error
FROM latest_status
"#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, u32>(5)?,
            row.get::<_, u32>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (host_id, status, last_checked_at, last_change_at, latency_ms, succ, fail, last_err) =
            row?;
        let state = runtime_state_from_row(
            status,
            last_checked_at,
            last_change_at,
            latency_ms,
            succ,
            fail,
            last_err,
        )?;
        map.insert(host_id, state);
    }
    Ok(map)
}

pub(super) fn load_latest_usage(
    conn: &Connection,
    host_id: &str,
) -> Result<Option<UsageSnapshot>, StorageError> {
    let row = conn
        .query_row(
            r#"
SELECT host_id, collected_at, console_users, remote_users, status, error
FROM latest_usage
WHERE host_id = ?1
"#,
            params![host_id],
            |row| {
                Ok(UsageRow {
                    id: None,
                    host_id: row.get(0)?,
                    collected_at: row.get(1)?,
                    console_users: row.get(2)?,
                    remote_users: row.get(3)?,
                    status: row.get(4)?,
                    error: row.get(5)?,
                })
            },
        )
        .optional()?;
    row.map(TryInto::try_into).transpose()
}

/// Bulk-load every persisted latest_usage row in one SELECT.
pub(super) fn load_all_latest_usage(
    conn: &Connection,
) -> Result<HashMap<String, UsageSnapshot>, StorageError> {
    let mut stmt = conn.prepare(
        r#"
SELECT host_id, collected_at, console_users, remote_users, status, error
FROM latest_usage
"#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UsageRow {
            id: None,
            host_id: row.get(0)?,
            collected_at: row.get(1)?,
            console_users: row.get(2)?,
            remote_users: row.get(3)?,
            status: row.get(4)?,
            error: row.get(5)?,
        })
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let usage_row = row?;
        let host_id = usage_row.host_id.clone();
        let snapshot: UsageSnapshot = usage_row.try_into()?;
        map.insert(host_id, snapshot);
    }
    Ok(map)
}

pub(super) fn insert_transition(
    conn: &Connection,
    event: &IcmpTransition,
) -> Result<(), StorageError> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM transitions WHERE host_id = ?1 AND changed_at = ?2 AND previous_status = ?3 AND new_status = ?4)", params![event.host_id, event.changed_at.to_rfc3339(), event.previous_status.as_str(), event.new_status.as_str()], |row| row.get(0))?;
    if exists {
        return Ok(());
    }
    conn.execute(
        r#"
INSERT INTO transitions (
    host_id, previous_status, new_status, changed_at, latency_ms, error, backend, reason
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
        params![
            event.host_id,
            event.previous_status.as_str(),
            event.new_status.as_str(),
            event.changed_at.to_rfc3339(),
            event.latency_ms,
            event.error,
            event.backend,
            event.reason,
        ],
    )?;
    Ok(())
}

pub(super) fn load_history(
    conn: &Connection,
    host_id: &str,
    limit: usize,
) -> Result<Vec<IcmpTransition>, StorageError> {
    let mut stmt = conn.prepare(
        r#"
SELECT id, host_id, previous_status, new_status, changed_at, latency_ms, error, backend, reason
FROM transitions
WHERE host_id = ?1
ORDER BY changed_at DESC, id DESC
LIMIT ?2
"#,
    )?;
    let rows = stmt.query_map(params![host_id, limit as i64], |row| {
        Ok(TransitionRow {
            id: row.get(0)?,
            host_id: row.get(1)?,
            previous_status: row.get(2)?,
            new_status: row.get(3)?,
            changed_at: row.get(4)?,
            latency_ms: row.get(5)?,
            error: row.get(6)?,
            backend: row.get(7)?,
            reason: row.get(8)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?.try_into()?);
    }
    Ok(events)
}
