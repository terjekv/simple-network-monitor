mod monitor;

use crate::{
    AppConfig,
    app::FilterKeyMetadata,
    domain::tcp::{ResolvedTcpHostConfig, TcpCheckConfig},
    modules::{
        ConfigOptionDoc, ModuleMetadata, ModuleRuntimeContext, MonitorModule, PreparedMonitor,
        SpawnedMonitor,
    },
};
use actix_web::web;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TcpModuleConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub concurrency: usize,
}

impl Default for TcpModuleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(3),
            concurrency: 64,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TcpHostConfig {
    pub enabled: Option<bool>,
    #[serde(with = "humantime_serde")]
    pub interval: Option<Duration>,
    #[serde(with = "humantime_serde")]
    pub timeout: Option<Duration>,
    pub checks: Vec<TcpCheckConfig>,
}

impl TcpModuleConfig {
    pub fn resolve_host(&self, raw: TcpHostConfig) -> ResolvedTcpHostConfig {
        ResolvedTcpHostConfig {
            enabled: raw.enabled.unwrap_or(self.enabled),
            interval: raw.interval.unwrap_or(self.interval),
            timeout: raw.timeout.unwrap_or(self.timeout),
            checks: raw.checks,
        }
    }
}

pub struct TcpModule;

#[async_trait::async_trait]
impl MonitorModule for TcpModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: "tcp",
            name: "TCP connectivity",
            description: "Connects to named TCP ports and records the latest result.",
            enabled_by_default: false,
        }
    }
    fn globally_enabled(&self, config: &AppConfig) -> bool {
        config.modules.tcp.enabled
    }
    fn has_enabled_hosts(&self, config: &AppConfig) -> bool {
        config
            .hosts
            .iter()
            .any(|host| host.modules.tcp.enabled && !host.modules.tcp.checks.is_empty())
    }
    fn filter_specs(&self) -> Vec<(&'static str, FilterKeyMetadata)> {
        Vec::new()
    }
    fn config_options(&self) -> Vec<ConfigOptionDoc> {
        vec![
            ConfigOptionDoc {
                key: "enabled",
                value_type: "boolean",
                default_value: "false",
                description: "global gate for TCP checks",
            },
            ConfigOptionDoc {
                key: "interval",
                value_type: "duration",
                default_value: "30s",
                description: "default polling interval (host override supported)",
            },
            ConfigOptionDoc {
                key: "timeout",
                value_type: "duration",
                default_value: "3s",
                description: "deadline including DNS and connection attempts (host override supported)",
            },
            ConfigOptionDoc {
                key: "concurrency",
                value_type: "integer",
                default_value: "64",
                description: "maximum concurrent TCP checks",
            },
            ConfigOptionDoc {
                key: "checks",
                value_type: "array",
                default_value: "[]",
                description: "host-only list of unique check IDs and nonzero TCP ports (at most 64)",
            },
        ]
    }
    fn register_routes(&self, _cfg: &mut web::ServiceConfig) {}
    async fn prepare(
        &self,
        config: &AppConfig,
    ) -> std::io::Result<Option<Box<dyn PreparedMonitor>>> {
        if !self.globally_enabled(config) || !self.has_enabled_hosts(config) {
            return Ok(None);
        }
        Ok(Some(Box::new(PreparedTcp {
            hosts: config
                .hosts
                .iter()
                .filter(|host| host.modules.tcp.enabled)
                .cloned()
                .collect(),
            concurrency: config.modules.tcp.concurrency,
        })))
    }
}

struct PreparedTcp {
    hosts: Vec<crate::domain::Host>,
    concurrency: usize,
}

impl PreparedMonitor for PreparedTcp {
    fn spawn(self: Box<Self>, context: ModuleRuntimeContext) -> SpawnedMonitor {
        SpawnedMonitor {
            kind: "tcp",
            handle: tokio::spawn(monitor::run(
                self.hosts,
                self.concurrency,
                context.tcp_repository,
                context.metrics,
            )),
        }
    }
}
