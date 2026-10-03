pub mod icmp;
pub mod runner;
pub mod tcp;
pub mod usage;

use crate::{
    AppConfig,
    app::FilterKeyMetadata,
    app::telemetry::RuntimeMetrics,
    storage::{HostRepository, IcmpRepository, TcpRepository, UsageRepository},
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

#[async_trait::async_trait]
pub trait MonitorModule: Sync {
    fn metadata(&self) -> ModuleMetadata;
    fn globally_enabled(&self, config: &AppConfig) -> bool;
    fn has_enabled_hosts(&self, config: &AppConfig) -> bool;
    fn filter_specs(&self) -> Vec<(&'static str, FilterKeyMetadata)>;
    fn config_options(&self) -> Vec<ConfigOptionDoc>;
    fn register_routes(&self, cfg: &mut web::ServiceConfig);
    async fn prepare(
        &self,
        config: &AppConfig,
    ) -> std::io::Result<Option<Box<dyn PreparedMonitor>>>;
}

/// Module preparation finishes before a configuration is committed.
pub trait PreparedMonitor: Send {
    fn spawn(self: Box<Self>, context: ModuleRuntimeContext) -> SpawnedMonitor;
}

pub struct ModuleRuntimeContext {
    pub metrics: Arc<RuntimeMetrics>,
    pub tcp_repository: Arc<dyn TcpRepository>,
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
static TCP_MODULE: tcp::TcpModule = tcp::TcpModule;
static MODULES: [&'static dyn MonitorModule; 3] = [&ICMP_MODULE, &USAGE_MODULE, &TCP_MODULE];

pub fn registry() -> &'static [&'static dyn MonitorModule] {
    &MODULES
}
