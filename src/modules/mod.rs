pub mod icmp;
pub mod usage;

use crate::{
    AppConfig,
    app::FilterKeyMetadata,
    backends::ping::PingBackend,
    storage::{HostRepository, IcmpRepository, UsageRepository},
};
use actix_web::web;
use serde::Serialize;
use std::sync::Arc;
use tokio::task::JoinHandle;

#[derive(Clone, Debug, Serialize)]
pub struct ModuleMetadata {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub enabled_by_default: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigOptionDoc {
    pub key: &'static str,
    pub value_type: &'static str,
    pub default_value: &'static str,
    pub description: &'static str,
}

pub trait MonitorModule: Sync {
    fn metadata(&self) -> ModuleMetadata;
    fn globally_enabled(&self, config: &AppConfig) -> bool;
    fn has_enabled_hosts(&self, config: &AppConfig) -> bool;
    fn filter_specs(&self) -> Vec<(&'static str, FilterKeyMetadata)>;
    fn config_options(&self) -> Vec<ConfigOptionDoc>;
    fn register_routes(&self, cfg: &mut web::ServiceConfig);
    fn spawn_monitor(&self, context: ModuleRuntimeContext<'_>) -> Option<SpawnedMonitor>;
}

pub struct ModuleRuntimeContext<'a> {
    pub config: &'a AppConfig,
    pub ping_backend: Option<Arc<dyn PingBackend>>,
    pub host_repository: Arc<dyn HostRepository>,
    pub icmp_repository: Arc<dyn IcmpRepository>,
    pub usage_repository: Arc<dyn UsageRepository>,
}

pub struct SpawnedMonitor {
    pub kind: &'static str,
    pub handle: JoinHandle<()>,
}

static ICMP_MODULE: icmp::IcmpModule = icmp::IcmpModule;
static USAGE_MODULE: usage::UsageModule = usage::UsageModule;
static MODULES: [&'static dyn MonitorModule; 2] = [&ICMP_MODULE, &USAGE_MODULE];

pub fn registry() -> &'static [&'static dyn MonitorModule] {
    &MODULES
}
