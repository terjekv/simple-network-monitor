# Maintainer Notes

This project is intentionally small: the daemon loads strict TOML config,
builds monitor modules, records state in SQLite, and serves the current view
over Actix JSON routes.

## Runtime Shape

- `config` owns all TOML parsing and semantic validation. Keep validation
  explicit: parse raw types first, validate, then build `AppConfig`.
- `domain` contains transport- and storage-independent types.
- `backends` implements probing details behind narrow traits and typed
  `domain::check::Check` adapters. TCP and raw ICMP share bounded DNS admission.
- `modules` owns module metadata, config docs, filter docs, enabled-state
  checks, route registration, dependency preparation, and monitor spawning for
  ICMP, usage, and named TCP checks.
- `app` owns filter parsing, the filter registry, and process-lifetime execution
  metrics. `modules::runner` owns common scheduling and durable-write retries.
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
also get a small read-only `r2d2` pool with a custom SQLite connection manager with `query_only = true`, so read
routes can avoid waiting on writer transactions. In-memory storage has no
reader pool because each SQLite in-memory connection has independent state.

Storage owns SQL. Modules should not import SQLite types or emit DDL; they
produce/consume typed domain data and storage decides how to persist it. Tables
currently have separate purposes:

- `latest_status`: latest ICMP state for each host.
- `transitions`: ICMP history.
- `latest_usage`: latest usage snapshot for each host.
- `latest_tcp`: latest observation keyed by host and check ID, with address and
  port identity. Schema v3 adds this table without modifying v2 observations.
- `usage_history`: usage change events.
- `usage_samples`: every retained usage poll sample.
- `usage_coverage`: start and latest timestamp of continuous collection.
- `host_identity`: address and usage enabled-state across process restarts.
- `retention_floor`: oldest trustworthy history boundary after pruning.

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
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo run --locked -- --config monitor.example.toml --verify-config-only
```

Keep production files under 1,000 lines. Split large modules by responsibility
or move large test modules into sibling `tests.rs` files. Keep functions short
enough to review in one screen; if validation or SQL logic grows, extract a
named helper that captures one rule or one query path.

Catalogs are immutable `Arc<BTreeMap<...>>` snapshots swapped under an `RwLock`
on reload. Single-host lookups clone only that host. A semaphore admits readers
before `spawn_blocking`; another serializes writes. Reader queries use a SQLite
transaction for a consistent state/usage view. Monitor storage handles carry a
generation checked under the writer mutex, so queued writes from a replaced
inventory fail without mutating new state. A host-task exit ends its module and
is propagated to service supervision. Failed writes retain their observation
for retry; ICMP transition identity makes repeating a write idempotent.

Periodic maintenance prunes bounded batches independent of active polling.
Inactivity evaluation is bounded by retained history so a removed event cannot turn a
long inactivity window into a false affirmative result. Migration is transactional.


The authenticated `/metrics` endpoint reads the same repository observations as
host responses, then emits fixed Prometheus metric families. It performs no
network checks. Freshness gauges and observation timestamps distinguish stopped
collection from failed targets. Module execution counters and histograms are
shared across API workers and reloads, and count completed observations before
persistence retries. Labels contain stable host/check IDs and bounded enums;
free-form diagnostic text and inventory metadata are excluded.

TCP identities include host address, check ID, and port. Inventory synchronization
clears results for changed/disabled/removed checks in the same transaction as the
host inventory. Monitor repositories enforce the existing generation guard on TCP
writes. A restart with unchanged identity retains current observations. Readiness
counts each enabled named TCP check separately.
