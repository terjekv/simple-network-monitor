# Writing Monitor Modules

Monitor modules are the extension point for adding a new kind of observation
without spreading config, filters, docs, routes, and runtime startup logic across
the whole codebase.

## Boundaries

A module owns:

- A `MonitorModule` impl in `src/modules/<id>/mod.rs`.
- Module metadata returned by `GET /v1/modules`.
- Strict TOML structs for `[modules.<id>]`, `[hosts.modules.<id>]`, and any
  supported `[group_settings.<group>.modules.<id>]` settings.
- Filter documentation for `GET /v1/namespaces`, `GET /v1/modules`, and
  `/openapi.json`.
- API route registration for module-specific routes.
- Runtime monitor loop behavior under `src/modules/<id>/monitor.rs`.

The `MonitorModule` impl is the main boundary. It must provide:

- `metadata`
- `globally_enabled`
- `has_enabled_hosts`
- `filter_specs`
- `config_options`
- `register_routes`
- async `prepare` returning an optional `PreparedMonitor`

A module does not own:

- SQLite DDL or migrations. Storage owns SQL.
- Cross-module host identity fields. `Host` core fields are `id`, `address`,
  `name`, `groups`, and `metadata`.
- Authentication or duplicate query-key handling. API routes own those.

Storage should expose typed repository methods for module data that needs more
than common latest-state fields. For example, usage inactivity filters need
historical console and remote user counts, so those semantics stay in the
storage adapter rather than being flattened into generic text state.

## Adding A Module

1. Add `src/modules/<id>/mod.rs` with config structs, resolved host config,
   metadata, enabled-state checks, filter specs, config option docs, route
   registration, and dependency preparation.
2. Add a static module entry in `src/modules/mod.rs`.
3. Add raw config parsing and inheritance in `src/config.rs`.
4. Add resolved host config under `HostModuleConfig` in `src/domain/host.rs`.
5. Add monitor runtime code under `src/modules/<id>/monitor.rs` and call it
   from its prepared runtime's `spawn` implementation, using the shared runner.
6. Add any typed repositories to `ModuleRuntimeContext` and its construction.
   The manager iterates the module registry for preparation and spawning; backend
   construction belongs in the module.
7. Add parser behavior in `src/app/filters.rs` for any new query filters.
8. Add storage repository methods and SQLite tables only if the module needs
   durable data beyond existing latest-state/history semantics.
9. Add API tests for `/v1/modules`, `/v1/namespaces`, `/openapi.json`, route
   behavior, invalid filters, and duplicate query-key handling.
10. Update `monitor.example.toml`, relevant generated config scripts, README,
    and architecture notes.

## Compatibility

Prefer not to change SQLite schema metadata unless a physical storage change is
required. If a module can be added using existing tables or in-memory state, keep
`PRAGMA user_version` unchanged so existing databases remain usable by older
builds.

## Check and runtime contracts

`domain::check::Check` is an async trait with typed `Request`, `Observation`, and
`Error` associated types. It performs one observation and does not access HTTP or
storage. The existing `PingBackend` and `UsageCollector` APIs are adapted through
`backends::check`. `TcpConnectCheck` demonstrates a direct implementation and an
injectable transport for tests. Adding a backend still requires registration in
its module/configuration; the daemon does not load Rust libraries at runtime.

`MonitorModule::prepare` returns a `PreparedMonitor` or a preparation error. It
must finish dependency construction without starting check tasks or changing
shared state. The manager prepares every module before committing inventory,
authentication, and configuration changes. A preparation failure leaves the old
configuration and tasks active. `PreparedMonitor::spawn` consumes those prepared
dependencies and starts tasks against the newly selected repository generation.
`IcmpModule::prepare_with_backend` and `UsageModule::prepare_with_collector` expose
injection seams for embedding and deterministic tests.

A `runner::MonitorJob` owns module-specific request creation, observation
processing, and typed persistence. Use `runner::run_job` for each configured check
and `runner::supervise` for the module's task set. The runner applies interval
jitter, module admission limits, and missed-tick delays. It records one execution
metric after each completed observation, then retries that same observation until
persistence succeeds. Admission remains held during retries to bound pending
writes. A stale-generation error ends the job; unexpected task exits propagate
to service supervision. Aborting the module drops its task set and cancels jobs.

Checks must enforce a deadline covering resolution and all I/O, and their futures
must release owned resources on cancellation. OS DNS work may outlive a cancelled
future, so network checks should use the bounded resolver in `backends::dns`.
Keep module-specific semantics explicit: ICMP applies transition thresholds;
usage preserves continuous-coverage history; TCP stores independently named
latest results. A failed observation is still current for readiness purposes.

When exposing a new module, add its freshness/readiness projection and metric
families alongside its typed result. Use bounded labels and stable check IDs;
metrics are a view of stored observations. Never execute a check during a scrape
or count a persistence retry as another execution. TCP's metric check IDs are
prefixed with `tcp.` to avoid collisions with built-in `icmp` and `usage` IDs.
