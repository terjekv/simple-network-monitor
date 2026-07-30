use crate::{
    app::{FilterKeyMetadata, FilterNamespaceCatalog},
    domain::{
        HostRecord, HostStatus, IcmpTransition, UsageCollectionStatus, UsageEvent, UsageReport,
        UsageSnapshot, UsageSummary, duration_ms,
    },
    modules::{ConfigOptionDoc, ModuleMetadata},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HostResponse {
    pub id: String,
    pub address: String,
    pub name: String,
    pub groups: Vec<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub metadata: Map<String, Value>,
    #[schema(value_type = String, example = "up")]
    pub status: HostStatus,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_change_at: Option<DateTime<Utc>>,
    pub latency_ms: Option<f64>,
    pub consecutive_successes: u32,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub usage: Option<UsageSnapshotResponse>,
}

impl From<HostRecord> for HostResponse {
    fn from(record: HostRecord) -> Self {
        Self {
            id: record.host.id,
            address: record.host.address,
            name: record.host.name,
            groups: record.host.groups,
            metadata: record.host.metadata,
            status: record.state.status,
            last_checked_at: record.state.last_checked_at,
            last_change_at: record.state.last_change_at,
            latency_ms: record.state.latency.map(duration_ms),
            consecutive_successes: record.state.consecutive_successes,
            consecutive_failures: record.state.consecutive_failures,
            last_error: record.state.last_error,
            usage: record.usage.map(Into::into),
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct IcmpTransitionResponse {
    pub id: Option<i64>,
    pub host_id: String,
    #[schema(value_type = String, example = "unknown")]
    pub previous_status: HostStatus,
    #[schema(value_type = String, example = "down")]
    pub new_status: HostStatus,
    pub changed_at: DateTime<Utc>,
    pub latency_ms: Option<f64>,
    pub error: Option<String>,
    pub backend: String,
    pub reason: String,
}

impl From<IcmpTransition> for IcmpTransitionResponse {
    fn from(event: IcmpTransition) -> Self {
        Self {
            id: event.id,
            host_id: event.host_id,
            previous_status: event.previous_status,
            new_status: event.new_status,
            changed_at: event.changed_at,
            latency_ms: event.latency_ms,
            error: event.error,
            backend: event.backend,
            reason: event.reason,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct UsageSnapshotResponse {
    pub collected_at: DateTime<Utc>,
    pub console_users: Option<u32>,
    pub remote_users: Option<u32>,
    #[schema(value_type = String, example = "ok")]
    pub status: UsageCollectionStatus,
    pub error: Option<String>,
}

impl From<UsageSnapshot> for UsageSnapshotResponse {
    fn from(snapshot: UsageSnapshot) -> Self {
        Self {
            collected_at: snapshot.collected_at,
            console_users: snapshot.console_users,
            remote_users: snapshot.remote_users,
            status: snapshot.status,
            error: snapshot.error,
        }
    }
}

/// JSON shape returned for both `/v1/hosts/{id}/usage/history` and
/// `/v1/hosts/{id}/usage/samples`. The two endpoints differ in WHICH rows they
/// return (changes-only vs every-poll), not in the shape per row.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct UsageEventResponse {
    pub id: Option<i64>,
    pub host_id: String,
    pub collected_at: DateTime<Utc>,
    pub console_users: Option<u32>,
    pub remote_users: Option<u32>,
    #[schema(value_type = String, example = "ok")]
    pub status: UsageCollectionStatus,
    pub error: Option<String>,
}

impl From<UsageEvent> for UsageEventResponse {
    fn from(event: UsageEvent) -> Self {
        Self {
            id: event.id,
            host_id: event.host_id,
            collected_at: event.collected_at,
            console_users: event.console_users,
            remote_users: event.remote_users,
            status: event.status,
            error: event.error,
        }
    }
}

/// Alias preserved so the OpenAPI schema and existing route signatures keep
/// their distinct names while sharing one struct.
pub type UsageHistoryResponse = UsageEventResponse;

/// See [`UsageHistoryResponse`].
pub type UsageSampleResponse = UsageEventResponse;

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct UsageSummaryResponse {
    pub hosts_reporting: usize,
    pub hosts_with_errors: usize,
    pub console_users: u32,
    pub remote_users: u32,
}

impl From<UsageSummary> for UsageSummaryResponse {
    fn from(summary: UsageSummary) -> Self {
        Self {
            hosts_reporting: summary.hosts_reporting,
            hosts_with_errors: summary.hosts_with_errors,
            console_users: summary.console_users,
            remote_users: summary.remote_users,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HostUsageResponse {
    pub id: String,
    pub address: String,
    pub name: String,
    pub groups: Vec<String>,
    pub usage: UsageSnapshotResponse,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct UsageReportResponse {
    pub summary: UsageSummaryResponse,
    pub hosts: Vec<HostUsageResponse>,
}

impl From<UsageReport> for UsageReportResponse {
    fn from(report: UsageReport) -> Self {
        Self {
            summary: report.summary.into(),
            hosts: report
                .hosts
                .into_iter()
                .map(|host| HostUsageResponse {
                    id: host.id,
                    address: host.address,
                    name: host.name,
                    groups: host.groups,
                    usage: host.usage.into(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NamespaceCatalogResponse {
    pub bare: BTreeMap<&'static str, FilterKeyResponse>,
    pub namespaces: BTreeMap<&'static str, BTreeMap<&'static str, FilterKeyResponse>>,
    pub examples: Vec<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ModulesResponse {
    pub modules: Vec<ModuleResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ModuleResponse {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub enabled: bool,
    pub enabled_by_default: bool,
    pub filters: BTreeMap<&'static str, FilterKeyResponse>,
    pub config_options: Vec<ConfigOptionResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConfigOptionResponse {
    pub key: &'static str,
    pub value_type: &'static str,
    pub default_value: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FilterKeyResponse {
    pub value_type: &'static str,
    pub values: Vec<&'static str>,
    pub description: &'static str,
}

impl From<FilterNamespaceCatalog> for NamespaceCatalogResponse {
    fn from(catalog: FilterNamespaceCatalog) -> Self {
        Self {
            bare: catalog
                .bare
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect(),
            namespaces: catalog
                .namespaces
                .into_iter()
                .map(|(namespace, keys)| {
                    (
                        namespace,
                        keys.into_iter()
                            .map(|(key, value)| (key, value.into()))
                            .collect(),
                    )
                })
                .collect(),
            examples: catalog.examples,
        }
    }
}

impl From<FilterKeyMetadata> for FilterKeyResponse {
    fn from(metadata: FilterKeyMetadata) -> Self {
        Self {
            value_type: metadata.value_type,
            values: metadata.values,
            description: metadata.description,
        }
    }
}

impl From<ConfigOptionDoc> for ConfigOptionResponse {
    fn from(doc: ConfigOptionDoc) -> Self {
        Self {
            key: doc.key,
            value_type: doc.value_type,
            default_value: doc.default_value,
            description: doc.description,
        }
    }
}

impl ModuleResponse {
    pub fn new(
        metadata: ModuleMetadata,
        enabled: bool,
        filters: Vec<(&'static str, FilterKeyMetadata)>,
        config_options: Vec<ConfigOptionDoc>,
    ) -> Self {
        Self {
            id: metadata.id,
            name: metadata.name,
            description: metadata.description,
            enabled,
            enabled_by_default: metadata.enabled_by_default,
            filters: filters
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect(),
            config_options: config_options.into_iter().map(Into::into).collect(),
        }
    }
}
