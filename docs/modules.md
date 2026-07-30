# Writing Monitor Modules

Monitor modules are the extension point for adding a new kind of observation
without spreading config, filters, docs, routes, and runtime startup logic across
the whole codebase.

## Boundaries

A module owns:

- A `MonitorModule` impl in `src/modules/<id>.rs`.
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
- `spawn_monitor`

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

1. Add `src/modules/<id>.rs` with config structs, resolved host config,
   metadata, enabled-state checks, filter specs, config option docs, route
   registration, and monitor spawning.
2. Add a static module entry in `src/modules/mod.rs`.
3. Add raw config parsing and inheritance in `src/config.rs`.
4. Add resolved host config under `HostModuleConfig` in `src/domain/host.rs`.
5. Add monitor runtime code under `src/modules/<id>/monitor.rs` and call it
   from the module's `spawn_monitor` implementation.
6. Wire startup/reload spawning in `src/main.rs`.
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
