# Maintainer Notes

This project is intentionally small: the daemon loads strict TOML config,
builds monitor modules, records state in SQLite, and serves the current view
over Actix JSON routes.

## Runtime Shape

- `config` owns all TOML parsing and semantic validation. Keep validation
  explicit: parse raw types first, validate, then build `AppConfig`.
- `domain` contains transport- and storage-independent types.
- `backends` implements probing details behind narrow traits.
- `modules` owns module metadata, config docs, filter docs, enabled-state
  checks, route registration, and monitor spawning for ICMP and usage.
- `app` owns monitor loops and filter parsing.
- `storage` owns persistence behind repository traits.
- `api` owns HTTP routes, DTOs, auth, errors, and OpenAPI annotations.

`modules.<id>.enabled` is the hard global gate for each module. A host may only
set `hosts.modules.<id>.enabled = true` when the global module gate is on; when
the global gate is on, per-host config can opt individual hosts out.

See `docs/modules.md` for the checklist and boundaries for adding a module.

## Config

Prefer `[[hosts]]` tables in examples when module-specific host config is shown:

```toml
[[hosts]]
id = "router-core-1"
address = "192.0.2.1"
metadata = { room = "net-a" }

[groups]
core = ["router-core-1"]
```

Top-level `[groups]` entries reference host ids. Empty member lists and unknown
host ids are rejected so filters cannot silently target invisible groups.

When adding config fields, update:

- `RawConfig` and the relevant module raw config/default helper.
- `RawConfig::validate` helpers if the field has semantic constraints.
- `monitor.example.toml`.
- A config test, especially for typo rejection or cross-field rules.

## Storage

SQLite storage keeps a writer connection behind a mutex. File-backed databases
also get a small read-only `r2d2_sqlite` pool with `query_only = true`, so read
routes can avoid waiting on writer transactions. In-memory storage has no
reader pool because each SQLite in-memory connection has independent state.

Storage owns SQL. Modules should not import SQLite types or emit DDL; they
produce/consume typed domain data and storage decides how to persist it. Tables
currently have separate purposes:

- `latest_status`: latest ICMP state for each host.
- `transitions`: ICMP history.
- `latest_usage`: latest usage snapshot for each host.
- `usage_history`: usage change events.
- `usage_samples`: every retained usage poll sample.

The in-memory `usage` map is only a change-detection cache for
`usage_history`; `latest_usage` is the durable source read by reports.

Schema migrations use `PRAGMA user_version`. Do not add per-module migration
bookkeeping until a module actually needs an independent physical schema change.
New migrations should be idempotent, tested against empty databases, and tested
from the previous schema version when possible.

## API And Filters

Routes must reject duplicate query keys before parsing filters. Dotted filter
names are registry-backed, except `metadata.<field>` which accepts arbitrary
metadata keys. Metadata matching is exact scalar equality for strings,
booleans, and numbers; arrays and objects do not match query values.

When adding a module filter, update:

- the module's filter docs and `app::filters` parser behavior.
- Storage query behavior.
- API tests for `/v1/namespaces`, `/v1/modules`, and `/openapi.json`.
- API tests for valid, invalid, and duplicate-query cases when applicable.

## Test And Size Expectations

Run these before handing off changes:

```sh
cargo fmt
cargo test -p simple-network-monitor
cargo clippy -p simple-network-monitor --all-targets -- -D warnings
cargo doc -p simple-network-monitor --no-deps
```

Keep production files under 1,000 lines. Split large modules by responsibility
or move large test modules into sibling `tests.rs` files. Keep functions short
enough to review in one screen; if validation or SQL logic grows, extract a
named helper that captures one rule or one query path.
