mod monitor;

use crate::{
    AppConfig,
    api::routes,
    app::FilterKeyMetadata,
    config::BackendKind,
    modules::{
        ConfigOptionDoc, ModuleMetadata, ModuleRuntimeContext, MonitorModule, SpawnedMonitor,
    },
};
use actix_web::web;
use monitor::{IcmpMonitorConfig, run_icmp_monitor};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IcmpModuleConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub backend: BackendKind,
    #[serde(default = "default_interval", with = "humantime_serde")]
    pub interval: Duration,
    #[serde(default = "default_timeout", with = "humantime_serde")]
    pub timeout: Duration,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    #[serde(default = "default_success_threshold")]
    pub success_threshold: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IcmpHostConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default, with = "humantime_serde")]
    pub interval: Option<Duration>,
    #[serde(default, with = "humantime_serde")]
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedIcmpHostConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

impl IcmpModuleConfig {
    pub fn resolve_host(&self, raw: IcmpHostConfig) -> ResolvedIcmpHostConfig {
        ResolvedIcmpHostConfig {
            enabled: raw.enabled.unwrap_or(self.enabled),
            interval: raw.interval.unwrap_or(self.interval),
            timeout: raw.timeout.unwrap_or(self.timeout),
        }
    }
}

impl Default for IcmpModuleConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            backend: BackendKind::Auto,
            interval: default_interval(),
            timeout: default_timeout(),
            concurrency: default_concurrency(),
            failure_threshold: default_failure_threshold(),
            success_threshold: default_success_threshold(),
        }
    }
}

impl Default for ResolvedIcmpHostConfig {
    fn default() -> Self {
        IcmpModuleConfig::default().resolve_host(IcmpHostConfig::default())
    }
}

pub struct IcmpModule;

impl MonitorModule for IcmpModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: "icmp",
            name: "ICMP reachability",
            description: "Polls hosts with ping and records status transitions.",
            enabled_by_default: true,
        }
    }

    fn globally_enabled(&self, config: &AppConfig) -> bool {
        config.modules.icmp.enabled
    }

    fn has_enabled_hosts(&self, config: &AppConfig) -> bool {
        config.hosts.iter().any(|host| host.modules.icmp.enabled)
    }

    fn filter_specs(&self) -> Vec<(&'static str, FilterKeyMetadata)> {
        vec![(
            "status",
            FilterKeyMetadata {
                value_type: "enum",
                values: vec!["unknown", "up", "down"],
                description: "host ICMP status",
            },
        )]
    }

    fn config_options(&self) -> Vec<ConfigOptionDoc> {
        vec![
            ConfigOptionDoc {
                key: "enabled",
                value_type: "boolean",
                default_value: "true",
                description: "whether ICMP monitoring is enabled globally",
            },
            ConfigOptionDoc {
                key: "backend",
                value_type: "enum",
                default_value: "auto",
                description: "ping backend: auto, raw, or system",
            },
            ConfigOptionDoc {
                key: "interval",
                value_type: "duration",
                default_value: "30s",
                description: "default host polling interval",
            },
            ConfigOptionDoc {
                key: "timeout",
                value_type: "duration",
                default_value: "1s",
                description: "default ping timeout",
            },
            ConfigOptionDoc {
                key: "concurrency",
                value_type: "integer",
                default_value: "128",
                description: "maximum concurrent ping checks",
            },
            ConfigOptionDoc {
                key: "failure_threshold",
                value_type: "integer",
                default_value: "2",
                description: "failed checks required before a host is down",
            },
            ConfigOptionDoc {
                key: "success_threshold",
                value_type: "integer",
                default_value: "1",
                description: "successful checks required before a host is up",
            },
        ]
    }

    fn register_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(routes::host_history);
    }

    fn spawn_monitor(&self, context: ModuleRuntimeContext<'_>) -> Option<SpawnedMonitor> {
        if !self.globally_enabled(context.config) || !self.has_enabled_hosts(context.config) {
            return None;
        }
        let Some(ping_backend) = context.ping_backend else {
            tracing::error!("ICMP monitor requested without a ping backend");
            return None;
        };

        let hosts = context
            .config
            .hosts
            .iter()
            .filter(|host| host.modules.icmp.enabled)
            .cloned()
            .collect();
        let monitor_config = IcmpMonitorConfig {
            concurrency: context.config.modules.icmp.concurrency,
            failure_threshold: context.config.modules.icmp.failure_threshold,
            success_threshold: context.config.modules.icmp.success_threshold,
        };
        let handle = tokio::spawn(run_icmp_monitor(
            hosts,
            monitor_config,
            ping_backend,
            context.host_repository,
            context.icmp_repository,
        ));
        Some(SpawnedMonitor {
            kind: "icmp",
            handle,
        })
    }
}

pub fn default_enabled() -> bool {
    true
}

pub fn default_interval() -> Duration {
    Duration::from_secs(30)
}

pub fn default_timeout() -> Duration {
    Duration::from_secs(1)
}

pub fn default_concurrency() -> usize {
    128
}

pub fn default_failure_threshold() -> u32 {
    2
}

pub fn default_success_threshold() -> u32 {
    1
}
