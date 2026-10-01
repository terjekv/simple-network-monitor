use actix_web::{App, HttpServer, web};
use clap::Parser;
use simple_network_monitor::{
    AppConfig,
    api::{self, ApiState},
    backends::ping::{PingBackend, build_backend},
    config::ModuleConfigs,
    domain::ApiToken,
    modules::{self, ModuleRuntimeContext},
    storage::{HostRepository, IcmpRepository, SqliteStorage, UsageRepository},
};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tokio::{sync::mpsc, task::AbortHandle};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug, Parser)]
#[command(version, about = "Simple ICMP network monitor with JSON status API")]
struct Cli {
    #[arg(short, long, env = "SNM_CONFIG", default_value = "monitor.toml")]
    config: PathBuf,
    /// Validate the config file and exit without starting monitors or the API.
    #[arg(long = "verify-config-only", alias = "verify-config")]
    verify_config_only: bool,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    init_logging();
    let cli = Cli::parse();
    let config = AppConfig::from_path(&cli.config).map_err(io_other)?;

    if cli.verify_config_only {
        println!(
            "config ok: {} hosts, bind {}, database {}",
            config.hosts.len(),
            config.bind,
            config.database_path.display()
        );
        return Ok(());
    }

    let storage = Arc::new(
        SqliteStorage::open(&config.database_path, config.hosts.clone()).map_err(io_other)?,
    );
    storage
        .set_usage_sample_retention(Some(config.modules.usage.sample_retention))
        .map_err(io_other)?;
    storage
        .set_history_retention(config.history_retention)
        .map_err(io_other)?;
    let host_repository: Arc<dyn HostRepository> = storage.clone();
    let icmp_repository: Arc<dyn IcmpRepository> = storage.clone();
    let usage_repository: Arc<dyn UsageRepository> = storage.clone();
    let api_token = Arc::new(RwLock::new(config.api_token.clone()));
    let module_config = Arc::new(RwLock::new(config.modules.clone()));
    let api_state = ApiState {
        hosts: Arc::clone(&host_repository),
        icmp: Arc::clone(&icmp_repository),
        usage: Arc::clone(&usage_repository),
        api_token: Arc::clone(&api_token),
        module_config: Arc::clone(&module_config),
    };

    tracing::info!(
        bind = %config.bind,
        hosts = config.hosts.len(),
        backend = ?config.modules.icmp.backend,
        database = %config.database_path.display(),
        api_workers = ?config.api_workers,
        usage_enabled = config.modules.usage.enabled,
        "starting simple network monitor"
    );

    let monitor_manager = tokio::spawn(run_monitor_manager(
        cli.config,
        config.clone(),
        MonitorManagerContext {
            storage: Arc::clone(&storage),
            api_token,
            module_config,
        },
    ));

    let server = {
        let mut server = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(api_state.clone()))
                .configure(api::configure)
        });
        if let Some(workers) = config.api_workers {
            server = server.workers(workers);
        }
        server
    }
    .bind(config.bind)?
    .run();

    tokio::select! {
        result = server => result,
        result = monitor_manager => {
            tracing::error!(?result, "monitor manager exited; shutting down");
            Err(std::io::Error::other("monitor manager exited"))
        }
    }
}

struct MonitorManagerContext {
    storage: Arc<SqliteStorage>,
    api_token: Arc<RwLock<Option<ApiToken>>>,
    module_config: Arc<RwLock<ModuleConfigs>>,
}

async fn run_monitor_manager(
    config_path: PathBuf,
    config: AppConfig,
    context: MonitorManagerContext,
) -> std::io::Result<()> {
    let prepared = prepare_monitor_config(config).await?;
    manage_monitors(config_path, prepared, context, hup_signal()?).await
}

async fn manage_monitors(
    config_path: PathBuf,
    prepared: PreparedMonitorConfig,
    context: MonitorManagerContext,
    mut reload: impl ReloadSignal,
) -> std::io::Result<()> {
    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel();
    let mut generation = 0_u64;
    let mut config = prepared.config;
    let mut handles = spawn_monitor_generation(
        generation,
        &config,
        prepared.backend,
        &context.storage,
        exit_tx.clone(),
    )?;
    let mut maintenance = tokio::time::interval(std::time::Duration::from_secs(1));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = maintenance.tick() => {
                match context.storage.prune_history(chrono::Utc::now()).await {
                    Ok(rows) => tracing::debug!(rows, "completed history maintenance batch"),
                    Err(err) => tracing::error!(%err, "history maintenance failed"),
                }
            }
            Some(()) = reload.recv() => {
                match prepare_monitor_config_from_path(&config_path).await {
                    Ok(prepared) => {
                        let next_config = prepared.config;
                        if next_config.bind != config.bind {
                            tracing::warn!(
                                current = %config.bind,
                                configured = %next_config.bind,
                                "ignoring changed bind address until restart"
                            );
                        }
                        if next_config.database_path != config.database_path {
                            tracing::warn!(
                                current = %config.database_path.display(),
                                configured = %next_config.database_path.display(),
                                "ignoring changed database path until restart"
                            );
                        }
                        if next_config.api_workers != config.api_workers {
                            tracing::warn!(
                                current = ?config.api_workers,
                                configured = ?next_config.api_workers,
                                "ignoring changed API worker count until restart"
                            );
                        }

                        let next_config = match next_config.for_reload(&config) {
                            Ok(next) => next,
                            Err(err) => { tracing::error!(%err, "unsafe config reload rejected"); continue; }
                        };
                        if let Err(err) = apply_reloaded_config(
                            &context.storage,
                            &context.api_token,
                            &context.module_config,
                            &next_config,
                        ) {
                            tracing::error!(%err, "config reload failed; keeping current config and monitors");
                            continue;
                        }

                        handles.abort();
                        generation += 1;
                        handles = spawn_monitor_generation(
                            generation,
                            &next_config,
                            prepared.backend,
                            &context.storage,
                            exit_tx.clone(),
                        )?;

                        tracing::info!(
                            hosts = next_config.hosts.len(),
                            backend = ?next_config.modules.icmp.backend,
                            api_workers = ?next_config.api_workers,
                            usage_enabled = next_config.modules.usage.enabled,
                            "reloaded config after SIGHUP"
                        );
                        config = next_config;
                    }
                    Err(err) => {
                        tracing::error!(%err, "config reload failed; keeping current config");
                    }
                }
            }
            Some(exit) = exit_rx.recv() => {
                if exit.generation == generation {
                    tracing::error!(kind = exit.kind, ?exit.result, "monitor exited");
                    return Err(std::io::Error::other(format!("{} monitor exited", exit.kind)));
                }
                tracing::debug!(kind = exit.kind, generation = exit.generation, "old monitor task exited after reload");
            }
        }
    }
}

struct MonitorHandles {
    handles: Vec<AbortHandle>,
}

impl MonitorHandles {
    fn abort(&self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

struct MonitorExit {
    generation: u64,
    kind: &'static str,
    result: Result<(), tokio::task::JoinError>,
}

struct PreparedMonitorConfig {
    config: AppConfig,
    backend: Option<Arc<dyn PingBackend>>,
}

async fn prepare_monitor_config_from_path(
    path: &PathBuf,
) -> std::io::Result<PreparedMonitorConfig> {
    let config = AppConfig::from_path(path).map_err(io_other)?;
    prepare_monitor_config(config).await
}

async fn prepare_monitor_config(config: AppConfig) -> std::io::Result<PreparedMonitorConfig> {
    let backend = if icmp_monitor_needed(&config) {
        Some(Arc::from(
            build_backend(config.modules.icmp.backend)
                .await
                .map_err(io_other)?,
        ))
    } else {
        None
    };
    Ok(PreparedMonitorConfig { config, backend })
}

fn icmp_monitor_needed(config: &AppConfig) -> bool {
    config.modules.icmp.enabled && config.hosts.iter().any(|host| host.modules.icmp.enabled)
}

#[cfg(test)]
async fn reload_shared_config_from_path(
    path: &PathBuf,
    storage: &SqliteStorage,
    api_token: &RwLock<Option<ApiToken>>,
    module_config: &RwLock<ModuleConfigs>,
) -> std::io::Result<PreparedMonitorConfig> {
    let prepared = prepare_monitor_config_from_path(path).await?;
    apply_reloaded_config(storage, api_token, module_config, &prepared.config)?;
    Ok(prepared)
}

fn apply_reloaded_config(
    storage: &SqliteStorage,
    api_token: &RwLock<Option<ApiToken>>,
    module_config: &RwLock<ModuleConfigs>,
    config: &AppConfig,
) -> std::io::Result<()> {
    let mut token = api_token
        .write()
        .map_err(|_| std::io::Error::other("api token lock poisoned"))?;
    let mut modules = module_config
        .write()
        .map_err(|_| std::io::Error::other("module config lock poisoned"))?;
    storage
        .update_hosts(config.hosts.clone())
        .map_err(io_other)?;
    storage
        .set_usage_sample_retention(Some(config.modules.usage.sample_retention))
        .map_err(io_other)?;
    storage
        .set_history_retention(config.history_retention)
        .map_err(io_other)?;
    *token = config.api_token.clone();
    *modules = config.modules.clone();
    Ok(())
}

fn spawn_monitor_generation(
    generation: u64,
    config: &AppConfig,
    backend: Option<Arc<dyn PingBackend>>,
    storage: &SqliteStorage,
    exit_tx: mpsc::UnboundedSender<MonitorExit>,
) -> std::io::Result<MonitorHandles> {
    let repository = Arc::new(storage.monitor_generation().map_err(io_other)?);
    let mut handles = Vec::new();
    for module in modules::registry() {
        let Some(spawned) = module.spawn_monitor(ModuleRuntimeContext {
            config,
            ping_backend: backend.clone(),
            host_repository: repository.clone(),
            icmp_repository: repository.clone(),
            usage_repository: repository.clone(),
        }) else {
            continue;
        };
        let abort = spawned.handle.abort_handle();
        watch_monitor_exit(spawned.kind, generation, spawned.handle, exit_tx.clone());
        handles.push(abort);
    }

    Ok(MonitorHandles { handles })
}

fn watch_monitor_exit(
    kind: &'static str,
    generation: u64,
    handle: tokio::task::JoinHandle<()>,
    exit_tx: mpsc::UnboundedSender<MonitorExit>,
) {
    tokio::spawn(async move {
        let result = handle.await;
        let _ = exit_tx.send(MonitorExit {
            generation,
            kind,
            result,
        });
    });
}

#[cfg(unix)]
fn hup_signal() -> std::io::Result<tokio::signal::unix::Signal> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
}

trait ReloadSignal: Send {
    fn recv(&mut self) -> impl Future<Output = Option<()>> + Send;
}

#[cfg(unix)]
impl ReloadSignal for tokio::signal::unix::Signal {
    async fn recv(&mut self) -> Option<()> {
        self.recv().await
    }
}

#[cfg(not(unix))]
fn hup_signal() -> std::io::Result<NeverSignal> {
    Ok(NeverSignal)
}

#[cfg(not(unix))]
struct NeverSignal;

#[cfg(not(unix))]
impl ReloadSignal for NeverSignal {
    async fn recv(&mut self) -> Option<()> {
        std::future::pending().await
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

fn io_other(err: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::other(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use simple_network_monitor::{
        backends::ping::PingCheckRequest,
        domain::{CheckFailure, HostFilter, HostStatus, PingOutcome, UsageFilter},
    };
    use std::{collections::HashMap, io::Write, time::Duration};
    use tempfile::NamedTempFile;
    use tokio::sync::oneshot;

    struct TestReloadSignal {
        requests: mpsc::UnboundedReceiver<()>,
        processed: mpsc::UnboundedSender<()>,
        received: bool,
    }

    impl ReloadSignal for TestReloadSignal {
        async fn recv(&mut self) -> Option<()> {
            // Re-entering the signal wait proves the previous reload was handled.
            if self.received {
                self.processed.send(()).unwrap();
                self.received = false;
            }
            let signal = self.requests.recv().await;
            self.received = signal.is_some();
            signal
        }
    }

    struct ControlledPing {
        requests: mpsc::UnboundedSender<oneshot::Sender<PingOutcome>>,
    }

    #[async_trait::async_trait]
    impl PingBackend for ControlledPing {
        fn name(&self) -> &'static str {
            "controlled"
        }

        async fn check(&self, _: &PingCheckRequest) -> Result<PingOutcome, CheckFailure> {
            let (response_tx, response_rx) = oneshot::channel();
            self.requests.send(response_tx).unwrap();
            Ok(response_rx.await.unwrap())
        }
    }

    fn config(contents: &str) -> AppConfig {
        AppConfig::from_toml_str(contents).unwrap()
    }

    fn write_config(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    fn empty_host_filter() -> HostFilter {
        HostFilter {
            icmp_status: None,
            group: None,
            metadata: HashMap::new(),
            usage: UsageFilter::empty(Utc::now()),
        }
    }

    async fn host_ids(storage: &SqliteStorage) -> Vec<String> {
        storage
            .hosts(empty_host_filter())
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.host.id)
            .collect()
    }

    fn token_value(token: &RwLock<Option<ApiToken>>) -> Option<String> {
        token
            .read()
            .unwrap()
            .as_ref()
            .map(|token| String::from_utf8_lossy(token.expose_bytes()).into_owned())
    }

    #[tokio::test]
    async fn reload_shared_config_replaces_hosts_and_token() {
        let initial = config(
            r#"
api_token = "old"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["old"] }]

[modules.icmp]
backend = "system"
"#,
        );
        let next = write_config(
            r#"
api_token = "new"
hosts = [
  { id = "r2", address = "192.0.2.2", groups = ["new"] },
  { id = "r3", address = "192.0.2.3", groups = ["new"] },
]

[modules.icmp]
backend = "system"
"#,
        );
        let storage = SqliteStorage::in_memory(initial.hosts.clone()).unwrap();
        let api_token = RwLock::new(initial.api_token.clone());
        let module_config = RwLock::new(initial.modules.clone());

        reload_shared_config_from_path(
            &next.path().to_path_buf(),
            &storage,
            &api_token,
            &module_config,
        )
        .await
        .unwrap();

        assert_eq!(host_ids(&storage).await, vec!["r2", "r3"]);
        assert_eq!(token_value(&api_token).as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn failed_reload_leaves_shared_state_unchanged() {
        let initial = config(
            r#"
api_token = "old"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["old"] }]

[modules.icmp]
backend = "system"
"#,
        );
        let invalid = write_config(
            r#"
api_token = "new"
hosts = []

[modules.icmp]
backend = "system"
"#,
        );
        let storage = SqliteStorage::in_memory(initial.hosts.clone()).unwrap();
        let api_token = RwLock::new(initial.api_token.clone());
        let module_config = RwLock::new(initial.modules.clone());

        let err = match reload_shared_config_from_path(
            &invalid.path().to_path_buf(),
            &storage,
            &api_token,
            &module_config,
        )
        .await
        {
            Ok(_) => panic!("invalid reload unexpectedly succeeded"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("at least one host is required"));
        assert_eq!(host_ids(&storage).await, vec!["r1"]);
        assert_eq!(token_value(&api_token).as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn busy_reload_keeps_monitors_running_and_allows_retry() {
        let initial = config(
            r#"
api_token = "fake-old-token"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["example"] }]

[modules.icmp]
backend = "system"
interval = "1ms"
"#,
        );
        let next = write_config(
            r#"
api_token = "fake-new-token"
hosts = [{ id = "r2", address = "192.0.2.2", groups = ["example"] }]

[modules.icmp]
enabled = false
"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state.db");
        let storage = Arc::new(SqliteStorage::open(&database, initial.hosts.clone()).unwrap());
        let api_token = Arc::new(RwLock::new(initial.api_token.clone()));
        let module_config = Arc::new(RwLock::new(initial.modules.clone()));
        let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
        let (reload_tx, reload_rx) = mpsc::unbounded_channel();
        let (processed_tx, mut processed_rx) = mpsc::unbounded_channel();
        let mut manager = tokio::spawn(manage_monitors(
            next.path().to_path_buf(),
            PreparedMonitorConfig {
                config: initial,
                backend: Some(Arc::new(ControlledPing { requests: probe_tx })),
            },
            MonitorManagerContext {
                storage: storage.clone(),
                api_token: api_token.clone(),
                module_config: module_config.clone(),
            },
            TestReloadSignal {
                requests: reload_rx,
                processed: processed_tx,
                received: false,
            },
        ));
        let probe = tokio::time::timeout(Duration::from_secs(10), probe_rx.recv())
            .await
            .unwrap()
            .unwrap();

        let writer = rusqlite::Connection::open(&database).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        reload_tx.send(()).unwrap();
        tokio::select! {
            result = &mut manager => panic!("manager exited during rejected reload: {result:?}"),
            processed = tokio::time::timeout(Duration::from_secs(15), processed_rx.recv()) => {
                assert_eq!(processed.unwrap(), Some(()));
            }
        }

        assert_eq!(host_ids(&storage).await, vec!["r1"]);
        assert_eq!(token_value(&api_token).as_deref(), Some("fake-old-token"));
        assert!(module_config.read().unwrap().icmp.enabled);
        assert!(!probe.is_closed(), "the current monitor was aborted");

        writer.execute_batch("ROLLBACK").unwrap();
        probe
            .send(PingOutcome {
                address: Some("192.0.2.1".parse().unwrap()),
                latency: Duration::from_millis(1),
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if storage.host("r1").await.unwrap().unwrap().state.status == HostStatus::Up {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the current monitor must still be able to persist observations");

        reload_tx.send(()).unwrap();
        tokio::select! {
            result = &mut manager => panic!("manager exited during reload retry: {result:?}"),
            processed = tokio::time::timeout(Duration::from_secs(15), processed_rx.recv()) => {
                assert_eq!(processed.unwrap(), Some(()));
            }
        }
        assert_eq!(host_ids(&storage).await, vec!["r2"]);
        assert_eq!(token_value(&api_token).as_deref(), Some("fake-new-token"));
        assert!(!module_config.read().unwrap().icmp.enabled);

        manager.abort();
        assert!(manager.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn icmp_disabled_does_not_build_ping_backend() {
        let prepared = prepare_monitor_config(config(
            r#"
hosts = [{ id = "r1", address = "192.0.2.1", groups = ["core"] }]

[modules.icmp]
enabled = false
backend = "raw"

[modules.usage]
enabled = true
"#,
        ))
        .await
        .unwrap();

        assert!(prepared.backend.is_none());
    }
}
