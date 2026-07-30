use crate::{
    domain::{
        HostRuntimeState, HostStatus, IcmpTransition, UsageCollectionStatus, UsageEvent,
        UsageSnapshot,
    },
    storage::StorageError,
};
use chrono::{DateTime, Utc};

pub(super) struct UsageRow {
    pub id: Option<i64>,
    pub host_id: String,
    pub collected_at: String,
    pub console_users: Option<u32>,
    pub remote_users: Option<u32>,
    pub status: String,
    pub error: Option<String>,
}

impl TryFrom<UsageRow> for UsageSnapshot {
    type Error = StorageError;

    fn try_from(row: UsageRow) -> Result<Self, Self::Error> {
        Ok(Self {
            collected_at: parse_ts(row.collected_at)?,
            console_users: row.console_users,
            remote_users: row.remote_users,
            status: UsageCollectionStatus::try_from(row.status.as_str())
                .map_err(StorageError::InvalidData)?,
            error: row.error,
        })
    }
}

impl TryFrom<UsageRow> for UsageEvent {
    type Error = StorageError;

    fn try_from(row: UsageRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            host_id: row.host_id,
            collected_at: parse_ts(row.collected_at)?,
            console_users: row.console_users,
            remote_users: row.remote_users,
            status: UsageCollectionStatus::try_from(row.status.as_str())
                .map_err(StorageError::InvalidData)?,
            error: row.error,
        })
    }
}

pub(super) struct TransitionRow {
    pub id: Option<i64>,
    pub host_id: String,
    pub previous_status: String,
    pub new_status: String,
    pub changed_at: String,
    pub latency_ms: Option<f64>,
    pub error: Option<String>,
    pub backend: String,
    pub reason: String,
}

impl TryFrom<TransitionRow> for IcmpTransition {
    type Error = StorageError;

    fn try_from(row: TransitionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            host_id: row.host_id,
            previous_status: HostStatus::try_from(row.previous_status.as_str())
                .map_err(StorageError::InvalidData)?,
            new_status: HostStatus::try_from(row.new_status.as_str())
                .map_err(StorageError::InvalidData)?,
            changed_at: parse_ts(row.changed_at)?,
            latency_ms: row.latency_ms,
            error: row.error,
            backend: row.backend,
            reason: row.reason,
        })
    }
}

pub(super) fn runtime_state_from_row(
    status: String,
    last_checked_at: Option<String>,
    last_change_at: Option<String>,
    latency_ms: Option<f64>,
    consecutive_successes: u32,
    consecutive_failures: u32,
    last_error: Option<String>,
) -> Result<HostRuntimeState, StorageError> {
    Ok(HostRuntimeState {
        status: HostStatus::try_from(status.as_str()).map_err(StorageError::InvalidData)?,
        last_checked_at: parse_optional_ts(last_checked_at)?,
        last_change_at: parse_optional_ts(last_change_at)?,
        latency: latency_ms.map(|ms| std::time::Duration::from_secs_f64(ms / 1000.0)),
        consecutive_successes,
        consecutive_failures,
        last_error,
    })
}

fn parse_optional_ts(value: Option<String>) -> Result<Option<DateTime<Utc>>, StorageError> {
    value.map(parse_ts).transpose()
}

fn parse_ts(value: String) -> Result<DateTime<Utc>, StorageError> {
    DateTime::parse_from_rfc3339(&value)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|err| StorageError::InvalidData(format!("invalid timestamp {value:?}: {err}")))
}
