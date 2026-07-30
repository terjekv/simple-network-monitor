mod monitor;

use crate::{
    AppConfig,
    api::routes,
    app::FilterKeyMetadata,
    backends::usage::{SystemSshUsageCollector, UsageCollector},
    domain::UsageOs,
    modules::{
        ConfigOptionDoc, ModuleMetadata, ModuleRuntimeContext, MonitorModule, SpawnedMonitor,
    },
};
use actix_web::web;
use monitor::{UsageMonitorConfig, run_usage_monitor};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageModuleConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_interval", with = "humantime_serde")]
    pub interval: Duration,
    #[serde(default = "default_timeout", with = "humantime_serde")]
    pub timeout: Duration,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_ssh_verify_host_key")]
    pub ssh_verify_host_key: bool,
    #[serde(default = "default_sample_retention", with = "humantime_serde")]
    pub sample_retention: Duration,
    #[serde(default = "default_linux_min_uid")]
    pub linux_min_uid: u32,
    #[serde(default = "default_macos_min_uid")]
    pub macos_min_uid: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageHostConfig {
    pub enabled: Option<bool>,
    pub os: Option<UsageOs>,
    pub ssh_verify_host_key: Option<bool>,
    #[serde(default, with = "humantime_serde")]
    pub interval: Option<Duration>,
    #[serde(default, with = "humantime_serde")]
    pub timeout: Option<Duration>,
    pub linux_min_uid: Option<u32>,
    pub macos_min_uid: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedUsageHostConfig {
    pub enabled: bool,
    pub os: UsageOs,
    pub ssh_verify_host_key: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub linux_min_uid: u32,
    pub macos_min_uid: u32,
}

impl UsageModuleConfig {
    pub fn resolve_host(
        &self,
        raw: UsageHostConfig,
        group_ssh_verify_host_key: Option<bool>,
    ) -> ResolvedUsageHostConfig {
        ResolvedUsageHostConfig {
            enabled: raw.enabled.unwrap_or(self.enabled),
            os: raw.os.unwrap_or(UsageOs::Auto),
            ssh_verify_host_key: raw
                .ssh_verify_host_key
                .or(group_ssh_verify_host_key)
                .unwrap_or(self.ssh_verify_host_key),
            interval: raw.interval.unwrap_or(self.interval),
            timeout: raw.timeout.unwrap_or(self.timeout),
            linux_min_uid: raw.linux_min_uid.unwrap_or(self.linux_min_uid),
            macos_min_uid: raw.macos_min_uid.unwrap_or(self.macos_min_uid),
        }
    }
}

impl Default for UsageModuleConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            interval: default_interval(),
            timeout: default_timeout(),
            concurrency: default_concurrency(),
            ssh_verify_host_key: default_ssh_verify_host_key(),
            sample_retention: default_sample_retention(),
            linux_min_uid: default_linux_min_uid(),
            macos_min_uid: default_macos_min_uid(),
        }
    }
}

impl Default for ResolvedUsageHostConfig {
    fn default() -> Self {
        UsageModuleConfig::default().resolve_host(UsageHostConfig::default(), None)
    }
}

pub struct UsageModule;

impl MonitorModule for UsageModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: "usage",
            name: "Console usage",
            description: "Collects console and remote user counts over SSH.",
            enabled_by_default: false,
        }
    }

    fn globally_enabled(&self, config: &AppConfig) -> bool {
        config.modules.usage.enabled
    }

    fn has_enabled_hosts(&self, config: &AppConfig) -> bool {
        config.hosts.iter().any(|host| host.modules.usage.enabled)
    }

    fn filter_specs(&self) -> Vec<(&'static str, FilterKeyMetadata)> {
        vec![
            (
                "status",
                FilterKeyMetadata {
                    value_type: "enum",
                    values: vec!["ok", "error"],
                    description: "latest usage collection status",
                },
            ),
            (
                "inactive_console_for",
                FilterKeyMetadata {
                    value_type: "duration",
                    values: vec!["24h", "7days"],
                    description: "no console users during the duration window",
                },
            ),
            (
                "no_users_for",
                FilterKeyMetadata {
                    value_type: "duration",
                    values: vec!["24h", "7days"],
                    description: "no console or remote users during the duration window",
                },
            ),
        ]
    }

    fn config_options(&self) -> Vec<ConfigOptionDoc> {
        vec![
            ConfigOptionDoc {
                key: "enabled",
                value_type: "boolean",
                default_value: "false",
                description: "whether usage collection is enabled globally",
            },
            ConfigOptionDoc {
                key: "interval",
                value_type: "duration",
                default_value: "5m",
                description: "default host collection interval",
            },
            ConfigOptionDoc {
                key: "timeout",
                value_type: "duration",
                default_value: "5s",
                description: "SSH command timeout",
            },
            ConfigOptionDoc {
                key: "concurrency",
                value_type: "integer",
                default_value: "32",
                description: "maximum concurrent usage collections",
            },
            ConfigOptionDoc {
                key: "ssh_verify_host_key",
                value_type: "boolean",
                default_value: "true",
                description: "whether SSH host keys must verify",
            },
            ConfigOptionDoc {
                key: "sample_retention",
                value_type: "duration",
                default_value: "30d",
                description: "how long per-poll usage samples are retained",
            },
            ConfigOptionDoc {
                key: "linux_min_uid",
                value_type: "integer",
                default_value: "1000",
                description: "minimum Linux UID counted as an interactive user",
            },
            ConfigOptionDoc {
                key: "macos_min_uid",
                value_type: "integer",
                default_value: "500",
                description: "minimum macOS UID counted as an interactive user",
            },
        ]
    }

    fn register_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(routes::usage_summary)
            .service(routes::host_usage_history)
            .service(routes::host_usage_samples);
    }

    fn spawn_monitor(&self, context: ModuleRuntimeContext<'_>) -> Option<SpawnedMonitor> {
        if !self.globally_enabled(context.config) || !self.has_enabled_hosts(context.config) {
            return None;
        }

        let monitor_config = UsageMonitorConfig {
            concurrency: context.config.modules.usage.concurrency,
        };
        let collector: Arc<dyn UsageCollector> = Arc::new(SystemSshUsageCollector);
        let handle = tokio::spawn(run_usage_monitor(
            context.config.hosts.clone(),
            monitor_config,
            collector,
            context.usage_repository,
        ));
        Some(SpawnedMonitor {
            kind: "usage",
            handle,
        })
    }
}

pub fn default_enabled() -> bool {
    false
}

pub fn default_interval() -> Duration {
    Duration::from_secs(300)
}

pub fn default_timeout() -> Duration {
    Duration::from_secs(5)
}

pub fn default_ssh_verify_host_key() -> bool {
    true
}

pub fn default_concurrency() -> usize {
    32
}

pub fn default_sample_retention() -> Duration {
    Duration::from_secs(30 * 24 * 60 * 60)
}

pub fn default_linux_min_uid() -> u32 {
    1000
}

pub fn default_macos_min_uid() -> u32 {
    500
}
