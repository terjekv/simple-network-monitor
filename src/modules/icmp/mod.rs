mod monitor;

use crate::backends::ping::{PingBackend, build_backend};
use crate::{
    AppConfig,
    api::routes,
    app::FilterKeyMetadata,
    config::BackendKind,
    modules::{
        ConfigOptionDoc, ModuleMetadata, ModuleRuntimeContext, MonitorModule, PreparedMonitor,
        SpawnedMonitor,
    },
};
use actix_web::web;
use monitor::{IcmpMonitorConfig, run_icmp_monitor};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

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

pub use crate::domain::settings::ResolvedIcmpHostConfig;

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

pub struct IcmpModule;

#[async_trait::async_trait]
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

    async fn prepare(
        &self,
        config: &AppConfig,
    ) -> std::io::Result<Option<Box<dyn PreparedMonitor>>> {
        if !self.globally_enabled(config) || !self.has_enabled_hosts(config) {
            return Ok(None);
        }
        let backend = Arc::from(
            build_backend(config.modules.icmp.backend)
                .await
                .map_err(std::io::Error::other)?,
        );
        Ok(Some(Self::prepare_with_backend(config, backend)))
    }
}

impl IcmpModule {
    /// Prepare with an injected backend; useful for embedders and deterministic tests.
    pub fn prepare_with_backend(
        config: &AppConfig,
        backend: Arc<dyn PingBackend>,
    ) -> Box<dyn PreparedMonitor> {
        Box::new(PreparedIcmp {
            hosts: config
                .hosts
                .iter()
                .filter(|host| host.modules.icmp.enabled)
                .cloned()
                .collect(),
            config: IcmpMonitorConfig {
                concurrency: config.modules.icmp.concurrency,
                failure_threshold: config.modules.icmp.failure_threshold,
                success_threshold: config.modules.icmp.success_threshold,
            },
            backend,
        })
    }
}

struct PreparedIcmp {
    hosts: Vec<crate::domain::Host>,
    config: IcmpMonitorConfig,
    backend: Arc<dyn PingBackend>,
}

impl PreparedMonitor for PreparedIcmp {
    fn spawn(self: Box<Self>, context: ModuleRuntimeContext) -> SpawnedMonitor {
        SpawnedMonitor {
            kind: "icmp",
            handle: tokio::spawn(run_icmp_monitor(
                self.hosts,
                self.config,
                self.backend,
                context.host_repository,
                context.icmp_repository,
                context.metrics,
            )),
        }
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
