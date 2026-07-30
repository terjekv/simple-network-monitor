use crate::{
    domain::{ApiToken, Host},
    modules::{
        icmp::{IcmpHostConfig, IcmpModuleConfig},
        usage::{UsageHostConfig, UsageModuleConfig},
    },
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

const MIN_DURATION: Duration = Duration::from_millis(1);

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub bind: SocketAddr,
    pub database_path: PathBuf,
    pub api_workers: Option<usize>,
    pub modules: ModuleConfigs,
    pub api_token: Option<ApiToken>,
    pub allow_unauthenticated_non_loopback: bool,
    pub hosts: Vec<Host>,
}

#[derive(Clone, Debug, Default)]
pub struct ModuleConfigs {
    pub icmp: IcmpModuleConfig,
    pub usage: UsageModuleConfig,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Auto,
    Raw,
    System,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default = "default_bind")]
    bind: SocketAddr,
    #[serde(default = "default_database_path")]
    database_path: PathBuf,
    #[serde(default)]
    api_workers: Option<usize>,
    #[serde(default)]
    modules: HashMap<String, toml::Value>,
    #[serde(default)]
    api_token: Option<ApiToken>,
    #[serde(default)]
    allow_unauthenticated_non_loopback: bool,
    #[serde(default)]
    groups: HashMap<String, Vec<String>>,
    #[serde(default)]
    group_settings: HashMap<String, RawGroupSettings>,
    hosts: Vec<RawHost>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHost {
    id: String,
    address: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    metadata: Map<String, Value>,
    #[serde(default)]
    modules: HashMap<String, toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGroupSettings {
    #[serde(default)]
    modules: HashMap<String, toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageGroupConfig {
    ssh_verify_host_key: Option<bool>,
}

struct ParsedHostModules {
    icmp: IcmpHostConfig,
    usage: UsageHostConfig,
}

struct ParsedGroupModules {
    usage: UsageGroupConfig,
}

impl AppConfig {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml_str(&contents).map_err(|err| match err {
            ConfigError::Parse { source, .. } => ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })
    }

    pub fn from_toml_str(contents: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(contents).map_err(|source| ConfigError::Parse {
            path: PathBuf::from("<inline>"),
            source,
        })?;
        raw.validate()
    }
}

impl RawConfig {
    fn validate(mut self) -> Result<AppConfig, ConfigError> {
        self.validate_core_settings()?;
        self.validate_auth_settings()?;

        let modules = parse_global_modules(std::mem::take(&mut self.modules))?;
        validate_module_config(&modules)?;

        let ids = self.validate_host_ids()?;
        let groups_by_host = self.validate_root_groups(&ids)?;
        self.validate_group_settings_names(&groups_by_host)?;
        let group_settings = parse_group_settings(std::mem::take(&mut self.group_settings))?;

        self.into_app_config(modules, groups_by_host, group_settings)
    }

    fn validate_core_settings(&self) -> Result<(), ConfigError> {
        if self.api_workers == Some(0) {
            return Err(ConfigError::Invalid(
                "api_workers must be at least 1".into(),
            ));
        }
        if self.hosts.is_empty() {
            return Err(ConfigError::Invalid("at least one host is required".into()));
        }
        Ok(())
    }

    fn validate_auth_settings(&self) -> Result<(), ConfigError> {
        if self.api_token.as_ref().is_some_and(ApiToken::is_blank) {
            return Err(ConfigError::Invalid("api_token must not be empty".into()));
        }
        if !self.bind.ip().is_loopback()
            && self.api_token.is_none()
            && !self.allow_unauthenticated_non_loopback
        {
            return Err(ConfigError::Invalid(
                "non-loopback bind requires api_token or allow_unauthenticated_non_loopback = true"
                    .into(),
            ));
        }
        Ok(())
    }

    fn validate_host_ids(&self) -> Result<HashSet<String>, ConfigError> {
        let mut ids = HashSet::new();
        for raw in &self.hosts {
            validate_id("host id", &raw.id)?;
            if !ids.insert(raw.id.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate host id {:?}",
                    raw.id
                )));
            }
        }
        Ok(ids)
    }

    fn validate_root_groups(
        &self,
        ids: &HashSet<String>,
    ) -> Result<HashMap<String, Vec<String>>, ConfigError> {
        let mut groups_by_host: HashMap<String, Vec<String>> = HashMap::new();
        for (group, members) in &self.groups {
            validate_id("group", group)?;
            if members.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "group {:?} must contain at least one host",
                    group
                )));
            }
            for member in members {
                validate_id("host id", member)?;
                if !ids.contains(member) {
                    return Err(ConfigError::Invalid(format!(
                        "group {:?} references unknown host {:?}",
                        group, member
                    )));
                }
                groups_by_host
                    .entry(member.clone())
                    .or_default()
                    .push(group.clone());
            }
        }
        Ok(groups_by_host)
    }

    fn validate_group_settings_names(
        &self,
        groups_by_host: &HashMap<String, Vec<String>>,
    ) -> Result<(), ConfigError> {
        let mut known_groups = HashSet::new();
        for raw in &self.hosts {
            known_groups.extend(raw.groups.iter().cloned());
        }
        for groups in groups_by_host.values() {
            known_groups.extend(groups.iter().cloned());
        }

        for group in self.group_settings.keys() {
            validate_id("group", group)?;
            if !known_groups.contains(group) {
                return Err(ConfigError::Invalid(format!(
                    "group_settings {:?} references unknown group",
                    group
                )));
            }
        }
        Ok(())
    }

    fn into_app_config(
        self,
        modules: ModuleConfigs,
        mut groups_by_host: HashMap<String, Vec<String>>,
        group_settings: HashMap<String, ParsedGroupModules>,
    ) -> Result<AppConfig, ConfigError> {
        let mut hosts = Vec::with_capacity(self.hosts.len());
        for raw in self.hosts {
            if raw.address.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "host {:?} has an empty address",
                    raw.id
                )));
            }
            let parsed_modules = parse_host_modules(&raw.id, raw.modules)?;

            let mut groups = raw.groups;
            if let Some(external_groups) = groups_by_host.remove(&raw.id) {
                groups.extend(external_groups);
            }
            groups.sort();
            groups.dedup();
            if groups.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "host {:?} must be in at least one group",
                    raw.id
                )));
            }
            for group in &groups {
                validate_id("group", group)?;
            }

            let group_usage_ssh_verify_host_key =
                group_settings_usage_ssh_verify_host_key(&group_settings, &groups);
            let icmp = modules.icmp.resolve_host(parsed_modules.icmp);
            let usage = modules
                .usage
                .resolve_host(parsed_modules.usage, group_usage_ssh_verify_host_key);
            if !modules.icmp.enabled && icmp.enabled {
                return Err(ConfigError::Invalid(format!(
                    "host {:?}: modules.icmp.enabled requires modules.icmp.enabled = true",
                    raw.id
                )));
            }
            if !modules.usage.enabled && usage.enabled {
                return Err(ConfigError::Invalid(format!(
                    "host {:?}: modules.usage.enabled requires modules.usage.enabled = true",
                    raw.id
                )));
            }
            validate_host_module_durations(&raw.id, &icmp, &usage)?;

            hosts.push(Host {
                name: raw.name.unwrap_or_else(|| raw.id.clone()),
                id: raw.id,
                address: raw.address,
                groups,
                metadata: raw.metadata,
                modules: crate::domain::host::HostModuleConfig { icmp, usage },
            });
        }

        Ok(AppConfig {
            bind: self.bind,
            database_path: self.database_path,
            api_workers: self.api_workers,
            modules,
            api_token: self.api_token,
            allow_unauthenticated_non_loopback: self.allow_unauthenticated_non_loopback,
            hosts,
        })
    }
}

fn parse_global_modules(raw: HashMap<String, toml::Value>) -> Result<ModuleConfigs, ConfigError> {
    let mut modules = ModuleConfigs::default();
    for (id, value) in raw {
        match id.as_str() {
            "icmp" => modules.icmp = parse_module_scope("modules.icmp", value)?,
            "usage" => modules.usage = parse_module_scope("modules.usage", value)?,
            _ => {
                return Err(ConfigError::Invalid(format!(
                    "unknown module {id:?}; valid modules: icmp, usage"
                )));
            }
        }
    }
    Ok(modules)
}

fn parse_host_modules(
    host_id: &str,
    raw: HashMap<String, toml::Value>,
) -> Result<ParsedHostModules, ConfigError> {
    let mut modules = ParsedHostModules {
        icmp: IcmpHostConfig::default(),
        usage: UsageHostConfig::default(),
    };
    for (id, value) in raw {
        match id.as_str() {
            "icmp" => {
                modules.icmp = parse_module_scope(&format!("hosts.{host_id}.modules.icmp"), value)?
            }
            "usage" => {
                modules.usage =
                    parse_module_scope(&format!("hosts.{host_id}.modules.usage"), value)?
            }
            _ => {
                return Err(ConfigError::Invalid(format!(
                    "host {host_id:?}: unknown module {id:?}; valid modules: icmp, usage"
                )));
            }
        }
    }
    Ok(modules)
}

fn parse_group_settings(
    raw: HashMap<String, RawGroupSettings>,
) -> Result<HashMap<String, ParsedGroupModules>, ConfigError> {
    let mut parsed = HashMap::new();
    for (group, settings) in raw {
        let mut modules = ParsedGroupModules {
            usage: UsageGroupConfig::default(),
        };
        for (id, value) in settings.modules {
            match id.as_str() {
                "usage" => {
                    modules.usage =
                        parse_module_scope(&format!("group_settings.{group}.modules.usage"), value)?
                }
                _ => {
                    return Err(ConfigError::Invalid(format!(
                        "group_settings {group:?}: unknown module {id:?}; valid modules: usage"
                    )));
                }
            }
        }
        parsed.insert(group, modules);
    }
    Ok(parsed)
}

fn parse_module_scope<T>(path: &str, value: toml::Value) -> Result<T, ConfigError>
where
    T: for<'de> Deserialize<'de>,
{
    value
        .try_into()
        .map_err(|err| ConfigError::Invalid(format!("{path}: {err}")))
}

fn validate_module_config(modules: &ModuleConfigs) -> Result<(), ConfigError> {
    if modules.icmp.concurrency == 0 {
        return Err(ConfigError::Invalid(
            "modules.icmp.concurrency must be at least 1".into(),
        ));
    }
    if modules.icmp.failure_threshold == 0 || modules.icmp.success_threshold == 0 {
        return Err(ConfigError::Invalid(
            "modules.icmp.success_threshold and modules.icmp.failure_threshold must be at least 1"
                .into(),
        ));
    }
    validate_duration("modules.icmp.interval", modules.icmp.interval)?;
    validate_duration("modules.icmp.timeout", modules.icmp.timeout)?;

    if modules.usage.concurrency == 0 {
        return Err(ConfigError::Invalid(
            "modules.usage.concurrency must be at least 1".into(),
        ));
    }
    validate_duration("modules.usage.interval", modules.usage.interval)?;
    validate_duration("modules.usage.timeout", modules.usage.timeout)?;
    validate_duration(
        "modules.usage.sample_retention",
        modules.usage.sample_retention,
    )?;
    Ok(())
}

fn validate_host_module_durations(
    host_id: &str,
    icmp: &crate::modules::icmp::ResolvedIcmpHostConfig,
    usage: &crate::modules::usage::ResolvedUsageHostConfig,
) -> Result<(), ConfigError> {
    validate_duration(
        &format!("host {host_id:?}: modules.icmp.interval"),
        icmp.interval,
    )?;
    validate_duration(
        &format!("host {host_id:?}: modules.icmp.timeout"),
        icmp.timeout,
    )?;
    validate_duration(
        &format!("host {host_id:?}: modules.usage.interval"),
        usage.interval,
    )?;
    validate_duration(
        &format!("host {host_id:?}: modules.usage.timeout"),
        usage.timeout,
    )?;
    Ok(())
}

fn validate_duration(name: &str, duration: Duration) -> Result<(), ConfigError> {
    if duration < MIN_DURATION {
        return Err(ConfigError::Invalid(format!("{name} must be at least 1ms")));
    }
    Ok(())
}

fn validate_id(kind: &str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ConfigError::Invalid(format!(
            "{kind} {value:?} must contain only ASCII letters, digits, dash, underscore, or dot"
        )));
    }
    Ok(())
}

fn group_settings_usage_ssh_verify_host_key(
    group_settings: &HashMap<String, ParsedGroupModules>,
    groups: &[String],
) -> Option<bool> {
    let mut configured = None;
    for group in groups {
        let Some(value) = group_settings
            .get(group)
            .and_then(|settings| settings.usage.ssh_verify_host_key)
        else {
            continue;
        };

        configured = Some(configured.unwrap_or(true) && value);
    }

    configured
}

fn default_bind() -> SocketAddr {
    SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 3000)
}

fn default_database_path() -> PathBuf {
    PathBuf::from("./network-monitor.sqlite3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_defaults_and_preserves_metadata() {
        let cfg = AppConfig::from_toml_str(
            r#"
hosts = [
  { id = "r1", address = "192.0.2.1", groups = ["core"], metadata = { room = "A1" } },
]
"#,
        )
        .unwrap();

        assert_eq!(cfg.modules.icmp.backend, BackendKind::Auto);
        assert_eq!(cfg.api_workers, None);
        assert_eq!(cfg.modules.icmp.concurrency, 128);
        assert!(cfg.modules.icmp.enabled);
        assert!(!cfg.modules.usage.enabled);
        assert_eq!(cfg.hosts[0].name, "r1");
        assert!(cfg.hosts[0].modules.icmp.enabled);
        assert!(!cfg.hosts[0].modules.usage.enabled);
        assert_eq!(cfg.hosts[0].metadata["room"], "A1");
    }

    #[test]
    fn loads_structured_module_config() {
        let cfg = AppConfig::from_toml_str(
            r#"
[modules.icmp]
backend = "system"
interval = "10s"
timeout = "2s"
concurrency = 8

[modules.usage]
enabled = true
interval = "10m"
timeout = "7s"
concurrency = 7

[[hosts]]
id = "r1"
address = "192.0.2.1"
groups = ["core"]

[hosts.modules.usage]
enabled = true
os = "linux"
interval = "1m"
"#,
        )
        .unwrap();

        assert_eq!(cfg.modules.icmp.backend, BackendKind::System);
        assert_eq!(cfg.modules.icmp.concurrency, 8);
        assert_eq!(cfg.modules.usage.concurrency, 7);
        assert_eq!(cfg.hosts[0].modules.usage.os, crate::domain::UsageOs::Linux);
        assert_eq!(cfg.hosts[0].modules.usage.interval, Duration::from_secs(60));
        assert_eq!(cfg.hosts[0].modules.usage.timeout, Duration::from_secs(7));
    }

    #[test]
    fn example_config_loads() {
        let cfg = AppConfig::from_toml_str(include_str!("../monitor.example.toml")).unwrap();

        assert_eq!(cfg.hosts.len(), 2);
        assert!(!cfg.modules.usage.enabled);
        assert!(cfg.hosts.iter().all(|host| !host.modules.usage.enabled));
    }

    #[test]
    fn applies_group_usage_settings() {
        let cfg = AppConfig::from_toml_str(
            r#"
[modules.usage]
enabled = true
ssh_verify_host_key = false

[[hosts]]
id = "r1"
address = "192.0.2.1"
groups = ["linux"]

[[hosts]]
id = "r2"
address = "192.0.2.2"
groups = ["mac"]

[[hosts]]
id = "r3"
address = "192.0.2.3"
groups = ["mac"]
modules = { usage = { ssh_verify_host_key = true } }

[group_settings.mac.modules.usage]
ssh_verify_host_key = false

[group_settings.linux.modules.usage]
ssh_verify_host_key = true
"#,
        )
        .unwrap();

        assert!(cfg.hosts[0].modules.usage.ssh_verify_host_key);
        assert!(!cfg.hosts[1].modules.usage.ssh_verify_host_key);
        assert!(cfg.hosts[2].modules.usage.ssh_verify_host_key);
    }

    #[test]
    fn rejects_host_usage_enabled_when_global_usage_disabled() {
        let err = AppConfig::from_toml_str(
            r#"
[[hosts]]
id = "r1"
address = "192.0.2.1"
groups = ["core"]

[hosts.modules.usage]
enabled = true
"#,
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("modules.usage.enabled = true"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_old_root_keys() {
        let err = AppConfig::from_toml_str(
            r#"
interval = "30s"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["core"] }]
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("interval") || err.to_string().contains("unknown"));
    }

    #[test]
    fn rejects_unknown_module_key() {
        let err = AppConfig::from_toml_str(
            r#"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["core"] }]

[modules.usage]
enabld = true
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("enabld"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_host_module_key() {
        let err = AppConfig::from_toml_str(
            r#"
[[hosts]]
id = "r1"
address = "192.0.2.1"
groups = ["core"]
[hosts.modules.usage]
enabld = true
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("enabld"), "got: {err}");
    }

    #[test]
    fn accepts_multiline_hosts_and_root_group_memberships() {
        let cfg = AppConfig::from_toml_str(
            r#"
[modules.usage]
enabled = true

[[hosts]]
id = "router-core-1"
address = "192.0.2.1"
name = "Core Router 1"
metadata = { room = "net-a", rack = "rack-1" }
modules = { usage = { enabled = false, os = "auto", interval = "5m" } }

[[hosts]]
id = "switch-edge-1"
address = "192.0.2.2"
name = "Edge Switch 1"
metadata = { room = "lab-2" }

[groups]
core = ["router-core-1"]
routers = ["router-core-1"]
edge = ["switch-edge-1"]
switches = ["switch-edge-1"]
"#,
        )
        .unwrap();

        assert_eq!(cfg.hosts[0].groups, vec!["core", "routers"]);
        assert_eq!(
            cfg.hosts[0].modules.usage.interval,
            Duration::from_secs(300)
        );
        assert_eq!(cfg.hosts[1].groups, vec!["edge", "switches"]);
    }

    #[test]
    fn rejects_unknown_group_settings_group() {
        let err = AppConfig::from_toml_str(
            r#"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["core"] }]

[group_settings.missing.modules.usage]
ssh_verify_host_key = false
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("unknown group"), "got: {err}");
    }

    #[test]
    fn rejects_zero_module_interval() {
        let err = AppConfig::from_toml_str(
            r#"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["core"] }]

[modules.icmp]
interval = "0s"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("interval"), "got: {err}");
    }

    #[test]
    fn rejects_blank_api_token() {
        let err = AppConfig::from_toml_str(
            r#"
api_token = "  "
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["core"] }]
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("api_token"), "got: {err}");
    }

    #[test]
    fn rejects_duplicate_hosts() {
        let err = AppConfig::from_toml_str(
            r#"
hosts = [
  { id = "r1", address = "192.0.2.1", groups = ["core"] },
  { id = "r1", address = "192.0.2.2", groups = ["edge"] },
]
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate host id"));
    }
}
