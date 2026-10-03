# Simple Network Monitor

Rust daemon that monitors ICMP reachability, SSH user counts, and named TCP ports.
Exposes current observations over JSON and Prometheus metrics, with ICMP and
usage history in SQLite.

[![CI](https://github.com/terjekv/simple-network-monitor/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/terjekv/simple-network-monitor/actions/workflows/ci.yml)

## Run

```sh
cp monitor.example.toml monitor.toml
cargo run -- --config monitor.toml
```

## Make shortcuts

Common development commands are available through `make`:

```sh
make build
make test
make clippy
make verify-config CONFIG=monitor.toml
make rpm
```

Run `make help` for the full target list.

## Continuous integration and security

Pull requests and `main` are checked with rustfmt, Clippy, the full test suite,
documentation warnings, example-config validation, strict RustSec auditing,
dependency review, and CodeQL analysis of both Rust and GitHub Actions.
Security checks also run weekly so newly published advisories are detected even
when the source does not change. Dependabot proposes updates for Cargo
dependencies and pinned GitHub Actions.

## systemd deployment

For a persistent Linux daemon, use systemd rather than adding daemon/forking
logic to the binary. The example unit is in
`packaging/systemd/simple-network-monitor.service`.

Build and install the binary, config, and unit:

```sh
cargo build --release
sudo install -m 0755 target/release/simple-network-monitor /usr/local/bin/simple-network-monitor
sudo install -d -m 0755 /etc/simple-network-monitor
sudo install -m 0644 monitor.example.toml /etc/simple-network-monitor/monitor.toml
sudo install -m 0644 packaging/systemd/simple-network-monitor.service /etc/systemd/system/simple-network-monitor.service
```

Create a dedicated service account:

```sh
sudo useradd --system --home-dir /var/lib/simple-network-monitor --shell /usr/sbin/nologin simple-network-monitor
```

Set the persistent SQLite path in `/etc/simple-network-monitor/monitor.toml`:

```toml
database_path = "/var/lib/simple-network-monitor/network-monitor.sqlite3"
```

Then enable and start the service:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now simple-network-monitor.service
sudo systemctl status simple-network-monitor.service
journalctl -u simple-network-monitor.service -f
```

The unit uses `StateDirectory=simple-network-monitor`, so systemd creates
`/var/lib/simple-network-monitor` and grants the service user write access. If
usage collection needs SSH keys or known hosts, place them under
`/var/lib/simple-network-monitor/.ssh/` for the `simple-network-monitor` user.
The unit grants `CAP_NET_RAW` so `modules.icmp.backend = "raw"` can work;
`modules.icmp.backend = "system"` does not need it. Config reload is available through systemd:

```sh
sudo systemctl reload simple-network-monitor.service
```

## RPM build

The RPM packaging lives in `packaging/rpm/simple-network-monitor.spec`. On an
RPM build host, install the build tools first:

```sh
sudo dnf install rpm-build cargo rust gcc systemd-rpm-macros
```

Then build binary and source RPMs with either:

```sh
make rpm
# or
scripts/build-rpm.sh
```

If `cargo` is already installed on the build host, the helper passes
`--with local_cargo` to `rpmbuild`, which skips only the RPM `cargo`/`rust`
BuildRequires checks. Other build requirements, such as `rpm-build`, `gcc`, and
`systemd-rpm-macros`, are still required.

The helper writes artifacts under `target/rpmbuild/RPMS` and
`target/rpmbuild/SRPMS`. The RPM installs:

- `/usr/bin/simple-network-monitor`
- `/etc/simple-network-monitor/monitor.toml` as `%config(noreplace)`
- `simple-network-monitor.service`
- `/var/lib/simple-network-monitor` for the SQLite database

The RPM rewrites the installed config to use the persistent SQLite path:

```toml
database_path = "/var/lib/simple-network-monitor/network-monitor.sqlite3"
```

After installing, edit `/etc/simple-network-monitor/monitor.toml` for the host
list and any local polling/API settings.

## Portable Linux artifacts

GitHub Actions builds stripped, statically linked musl binaries for
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. Each build is
checked for both dynamic library dependencies and a dynamic program
interpreter, then published as a standalone tarball and an RPM with SHA-256
checksums.

- Pushes to `main` update the rolling
  [`main-latest`](https://github.com/terjekv/simple-network-monitor/releases/tag/main-latest)
  prerelease.
- Tags such as `v0.0.1` publish a versioned release. The tag must
  match the version in `Cargo.toml`, point to `main`, and already have a
  successful `main` artifact workflow run.

The musl executable does not depend on the target system's glibc or shared
libraries. It still requires a compatible Linux kernel and any external
programs used by configured modules: the system ICMP backend invokes `ping`,
and usage collection invokes `ssh`.

The RPM embeds the same verified static executable and is broadly portable
across modern systemd-based RPM distributions. It is not a distribution-neutral
package: installation still uses RPM/systemd conventions and declares runtime
dependencies on `shadow-utils`, `iputils`, and optionally OpenSSH. Use the
standalone tarball on non-RPM distributions.

After replacing a config file for a running daemon, send `SIGHUP` to reload it.
Reload validates the new config before changing shared state; invalid configs are
logged and ignored. If the inventory cannot be persisted (for example, because
SQLite is busy), the reload is logged and rejected while the current configuration
and monitors remain active. Send `SIGHUP` again to retry after resolving the error.
Host lists, groups, metadata, polling settings, backend,
usage settings, named TCP checks, and API token reload. `bind` and `database_path` changes are
logged but still require a process restart. `api_workers` also requires a
restart because Actix worker threads are created when the HTTP server starts.

The API serves HTTP/1.1 and binds to `127.0.0.1:3000` by default. Use a reverse
proxy for TLS or HTTP/2, with HTTP/1.1 connections to the monitor. The build
omits Actix's optional HTTP/2 support to exclude the vulnerable `h2` 0.3
dependency ([RUSTSEC-2026-0258](https://rustsec.org/advisories/RUSTSEC-2026-0258.html)).

For example:

```sh
curl http://127.0.0.1:3000/metrics
curl http://127.0.0.1:3000/healthz
curl http://127.0.0.1:3000/v1/namespaces
curl http://127.0.0.1:3000/v1/modules
curl http://127.0.0.1:3000/openapi.json
# Browse http://127.0.0.1:3000/swagger-ui/
curl http://127.0.0.1:3000/v1/hosts
curl 'http://127.0.0.1:3000/v1/hosts?icmp.status=down'
curl 'http://127.0.0.1:3000/v1/hosts?group=core'
curl 'http://127.0.0.1:3000/v1/hosts?metadata.room=net-a'
curl http://127.0.0.1:3000/v1/hosts/router-core-1/history
curl http://127.0.0.1:3000/v1/usage
curl 'http://127.0.0.1:3000/v1/hosts?usage.inactive_console_for=24h'
curl 'http://127.0.0.1:3000/v1/hosts?usage.no_users_for=7days'
curl http://127.0.0.1:3000/v1/hosts/router-core-1/usage/history
curl http://127.0.0.1:3000/v1/hosts/router-core-1/usage/samples
```

Use `[modules.icmp] backend = "system"` for portable macOS/Linux development. `backend = "auto"` tries raw ICMP first and falls back to invoking the system `ping` binary when raw sockets are unavailable. The raw backend is dual-stack: an IPv4 socket is required, an IPv6 socket is opened best-effort (warning logged if the OS denies it).

Usage tracking is disabled by default. `[modules.usage] enabled = true` is the global gate for usage collection; per-host `[hosts.modules.usage] enabled = false` can opt hosts out when the global gate is on. Usernames are not persisted or exposed. Per-poll samples are retained for `modules.usage.sample_retention` (default 30 days). Writes and periodic maintenance prune expired samples, including removed hosts. ICMP and usage change events use `history_retention` (default 365 days); usage retains one observation before the cutoff for inactivity evaluation.

Usage collection autodetects Linux vs macOS by running `uname -s` on the remote
host. Per-host `os = "linux"` or `os = "macos"` is optional and only skips that
probe when you already know the platform.

Usage collection verifies SSH host keys by default. Keep
`ssh_verify_host_key = true` unless an isolated legacy environment cannot
maintain known-host entries. Disabling it turns off strict host-key checking and
discards known-host state for the probe. The setting can be applied globally,
under `[group_settings.<group>.modules.usage]`, or under
`[hosts.modules.usage]`. Per-host settings override group settings; otherwise
group settings override the global default. If several groups configure one
host, `false` wins because it is the more permissive setting.

## Concurrency and workers

The monitor has separate limits for probe concurrency and HTTP API workers:

- `modules.icmp.concurrency` limits simultaneous ICMP checks across all hosts. It defaults to
  `128`.
- `modules.usage.concurrency` limits simultaneous usage probes over SSH. It defaults to
  `32` and only matters when `modules.usage.enabled = true`.
- `api_workers` optionally limits Actix HTTP worker threads. Omit it to use
  Actix's default, which is based on available CPU parallelism.

Probe concurrency is not a CPU worker count. ICMP and SSH checks spend most of
their time waiting on network I/O, so a concurrency value higher than the number
of CPU cores is normal. Raising it can make large host lists converge faster, but
also increases open sockets, `ping` processes when using `modules.icmp.backend = "system"`,
SSH sessions for usage collection, and write pressure on SQLite. If checks start
timing out or the host is resource constrained, lower `modules.icmp.concurrency`
and `modules.usage.concurrency` before lowering API workers.

`api_workers` mainly affects concurrent API request handling. For this service,
one or two API workers is usually enough unless the JSON API is being queried
heavily. Changing module concurrency settings reloads on `SIGHUP`;
changing `api_workers` requires a restart.

Host filters are strict. Dotted filters must use a known namespace and key, except `metadata.<field>` which accepts any metadata field name. Duplicate query keys are rejected with `400`. Metadata filters use exact scalar equality for strings, booleans, and numbers (byte-exact, case-sensitive); arrays and objects are not query-matchable. Use `GET /v1/namespaces` to discover supported filter namespaces and keys, and `GET /v1/modules` for module metadata and config option docs.

The module catalog's `enabled` field reports each module's current global gate,
including TCP, and reflects successful config reloads. It does not indicate
whether any hosts have checks configured for that module.

Config files are validated strictly: unknown keys in `[table]` blocks or `[[hosts]]` entries fail at startup with the offending key, so typos like `bakend = "raw"` surface immediately rather than silently using a default.

OpenAPI is generated from Rust endpoint/type annotations with `utoipa`, available at `GET /openapi.json`, and browsable at `/swagger-ui/`.

Usage filters use duration strings:

- `usage.inactive_console_for=24h`: hosts with zero console users and no console-user usage event during the window.
- `usage.no_users_for=7days`: hosts with zero console and remote users and no user usage event during the window.
- `/usage/history` returns usage change events; `/usage/samples` returns every stored poll sample.

Host listing filters can be combined:

```sh
curl 'http://127.0.0.1:3000/v1/hosts?icmp.status=up&group=core&usage.no_users_for=7days'
curl 'http://127.0.0.1:3000/v1/hosts?group=core&metadata.room=net-a'
```

## Code layout

- `domain`: pure host, ICMP, usage, and filter types.
- `app`: query services, monitor orchestration, and the filter registry.
- `backends`: narrow ICMP and SSH usage collection traits plus implementations.
- `storage`: repository traits and SQLite persistence.
- `api`: Actix routes, DTOs, auth, errors, and OpenAPI wiring.

See [docs/architecture.md](docs/architecture.md) for maintainer notes on
runtime flow, storage invariants, config validation, and size expectations.

## Freshness, paging, and resource limits

Host responses include `icmp_enabled`, `usage_enabled`, `icmp_stale`, and
`usage_stale`. An observation is stale if missing, future-dated, or older than
two resolved collection intervals plus its timeout. Stored status is the last
known observation; a successful API response does not prove probes are current.
`/healthz` is unauthenticated process liveness. Authenticated `/readyz` returns
200 when enabled checks have current observations and 503 while waiting.
An unreachable host can still have a current failed observation.

`GET /v1/inventory/hosts?limit=500&after=host-id` returns
`{"hosts": [...], "next_after": "..."}` in host-ID order. `limit` must be
1..1000; omit `after` for the first page, and stop at a null `next_after`.
Pages support `ETag` and `If-None-Match`; authentication is still required for
304 responses. Pages represent successive current reads, not a transaction
spanning an entire polling cycle. An inventory reload during traversal may
require another refresh. The existing filtered `/v1/hosts` array API is retained.
Host details remain at `/v1/hosts/{id}`, including `/v1/hosts/page` for a host
whose ID is `page`.

Inactivity filters require an enabled collector, a fresh observation, and
continuous coverage of the requested window. Gaps longer than the freshness
allowance reset coverage. Retention bounds how far back an affirmative result
can extend. On the first upgrade from schema version 1, current observations
are cleared because the old schema did not record host addresses; histories
are retained. Later address changes, including across restart, reset current
state and coverage. Back up the SQLite database before upgrading: schema v3
cannot be opened by v1/v2 binaries. The v2-to-v3 migration adds TCP current
observations and preserves existing ICMP and usage data.

Configuration durations and inactivity windows must be between 1 ms and 10
years. API worker counts are 1..256 and probe concurrency is 1..4096. Four
SQLite reads are admitted concurrently; overload returns 503 so clients can
back off without growing a blocking-task queue. Subprocess stdout and stderr
are bounded to 64 KiB each; retained diagnostics are bounded to 4 KiB. Raw-ping
deadlines include DNS resolution, and system-ping latency is parsed ICMP RTT.
At most 32 OS DNS lookups run concurrently; their admission slots remain held
until the resolver exits even if a probe deadline has expired.

Reload validation uses the listener that is actually running. Changing the
configured bind to loopback cannot remove authentication from a public listener.
Tokens contain 1..4096 visible ASCII characters. Config parse diagnostics omit
source snippets to protect secrets. Probe destinations must be IP literals or
DNS/SSH aliases; configure SSH usernames through the service account's SSH
configuration. Strict verification explicitly sets `StrictHostKeyChecking=yes`.

## Check the backend and frontend together

With the frontend checkout in `../simple-network-monitor-frontend`, run:

```sh
cd ../simple-network-monitor-frontend
npm ci
npx playwright install chromium
cd ../simple-network-monitor
bash scripts/check-projects.sh ../simple-network-monitor-frontend
```

The shared command validates both trees and runs the actual backend, production
proxy, and browser with reserved example inventory and probes disabled. Results
and desktop/mobile screenshots go to the frontend's ignored `test-results/`.
`SNM_TEST_HOSTS` controls the browser fixture size (at least 100); the default is
501 so the test crosses an API page boundary. It records 1/10/50-viewer request
latencies and overload responses. These are diagnostic measurements, not
production capacity guarantees.

The `Backend and frontend compatibility` workflow uses an approved frontend
repository and full commit SHA from repository settings. To run it on every
backend CI event, set the repository variable `SNM_FRONTEND_REF` to a tested
full SHA, and optionally `SNM_FRONTEND_REPOSITORY`. Manual runs use the same
approved pair; change these variables to select another frontend revision.
Caller-supplied inputs cannot select executable code, and npm caching is disabled.
The frontend uses `SNM_BACKEND_REF` and optionally
`SNM_BACKEND_REPOSITORY` for the reciprocal checks. Set these after publishing
the coordinated commits; no moving counterpart branch is silently selected.
Configure the resulting jobs as required checks in repository settings.

Tagged release reruns compare existing assets byte-for-byte and fail on a
mismatch. Only the separate `main-latest` publication can replace assets.

## Named TCP checks

Enable `[modules.tcp] enabled = true` and configure checks per host:

```toml
[hosts.modules.tcp]
checks = [{ id = "ssh", port = 22 }, { id = "web", port = 443 }]
# Optional host overrides:
interval = "30s"
timeout = "3s"
```

TCP is disabled by default. The global gate, host opt-out, interval, and timeout
follow the existing module conventions. Ports must be 1..65535; IDs must be
unique within a host's TCP module and contain 1..128 ASCII letters, digits,
dash, underscore, or dot (excluding `.` and `..`). At most 64 checks are allowed
per host. The module defaults to 64 concurrent checks (valid range 1..4096).
The deadline covers DNS and sequential attempts at resolved addresses. Resolution
shares the bounded resolver with ICMP and retains at most 64 addresses. A TCP
success means that a connection was established; it does not test TLS or an
application protocol.

Host JSON responses include a `tcp` array with `id`, `port`, `enabled`, `stale`,
and `observation`. An observation contains `observed_at`, `success`,
`duration_seconds`, and `error`; it is null until a result exists. `/readyz`
includes enabled TCP checks, and a fresh failed connection counts as a current
observation. Current TCP results persist across restart. Changing a host address
or check port, removing a check, or disabling TCP clears its current result.
TCP currently stores latest observations; historical charts come from the
metrics backend. Existing ICMP and usage response fields retain their meaning.

## Prometheus and OpenTelemetry

`GET /metrics` serves Prometheus text format on the existing HTTP listener,
with the same bearer-token authentication and reload behavior as the JSON API.
It reads persisted state without performing network checks. A failed storage
read fails the scrape instead of returning an empty successful response.
Keep the default loopback binding; use the documented authenticated listener
and TLS reverse proxy when scraping remotely.

A local Prometheus scrape configuration (with `api_token` configured on the
monitor) can use a separately provisioned token file:

```yaml
scrape_configs:
  - job_name: simple-network-monitor
    scrape_interval: 15s
    static_configs:
      - targets: ['127.0.0.1:3000']
    authorization:
      type: Bearer
      credentials_file: /etc/prometheus/simple-network-monitor.token
```

The token file contains the configured token, without the `Bearer` prefix.
Omit `authorization` only for a loopback deployment without a configured token.
In containers, loopback refers to that container's network namespace; use a
reachable authenticated endpoint for separate containers.

Metric families:

| Metric | Type and meaning |
| --- | --- |
| `snm_check_enabled` | Gauge: configured check is enabled |
| `snm_check_fresh` | Gauge: enabled check has a current observation |
| `snm_check_last_observed_timestamp_seconds` | Gauge: observation time, including failed attempts |
| `snm_icmp_status{state="unknown\|up\|down"}` | Gauges: exactly one state is 1 per enabled host |
| `snm_icmp_round_trip_seconds` | Gauge: latest RTT, present for a fresh successful attempt |
| `snm_usage_collection_success` | Gauge: latest usage attempt succeeded |
| `snm_usage_users{session="console\|remote"}` | Gauge: user count, present only for fresh successful collection |
| `snm_tcp_connect_success` | Gauge: latest TCP attempt succeeded |
| `snm_tcp_connect_duration_seconds` | Gauge: fresh successful TCP attempt duration, including DNS |
| `snm_check_runs_total{kind,result}` | Counter: completed executions, with success/failure result |
| `snm_check_duration_seconds{kind}` | Histogram: execution time, excluding queueing and persistence |
| `snm_storage_retries_total{kind}` | Counter: persistence retries |

The common check gauges use `host_id` and `check_id` labels. Check IDs are `icmp`,
`usage`, or `tcp.<configured-id>`. TCP-specific metrics use the same two labels;
ICMP/usage-specific metrics use `host_id` plus the documented state/session label.
Metadata, addresses, usernames, and error text are excluded from labels.
State gauges show last known results, so alerting must also check freshness.
Freshness uses the same two intervals plus timeout allowance as the JSON API;
missing and future-dated observations are not fresh. Observation timestamps are
metric values, not explicit sample timestamps.

Disabled checks retain only their enabled/fresh gauges (both zero); removed hosts
and checks disappear on subsequent scrapes. Unknown observations omit their
observation timestamp and result measurements. ICMP retains its explicit unknown
state. Failed usage collection never produces a fabricated zero-user count.
The counters and histograms begin with the first completed execution, survive
configuration reloads, and reset at process restart. Scrapes and persistence
retries never count as additional check executions.

Example alert rules:

```yaml
groups:
  - name: simple-network-monitor
    rules:
      - alert: NetworkMonitorUnavailable
        expr: up{job="simple-network-monitor"} == 0
        for: 2m
      - alert: NetworkCheckStale
        expr: (snm_check_enabled == 1) and (snm_check_fresh == 0)
        for: 2m
      - alert: NetworkHostDown
        expr: >-
          (snm_icmp_status{state="down"} == 1)
          and on(job, instance, host_id)
          (snm_check_fresh{check_id="icmp"} == 1)
        for: 1m
      - alert: NetworkTcpCheckFailed
        expr: (snm_tcp_connect_success == 0) and (snm_check_fresh == 1)
        for: 1m
```

Prometheus's `up` metric describes scrape availability; it does not describe a
monitored host. ICMP state follows configured failure/success thresholds, while
TCP reports each connection result directly. A freshness alert detects a stalled
collector even while the HTTP server remains reachable.

An OpenTelemetry Collector can scrape this endpoint with its
[Prometheus receiver](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/receiver/prometheusreceiver)
and forward the metrics through its configured OTLP exporter. Put the same
`scrape_configs` under `receivers.prometheus.config` and enable that receiver in
the Collector's metrics pipeline. Native OTLP export is not required by the daemon.

## Extending checks

Checks use a typed async `domain::check::Check` boundary. Existing `PingBackend`
and `UsageCollector` traits remain supported through adapters; TCP implements
`Check` directly. Module preparation constructs dependencies before a startup or
reload is committed, and the shared runner handles timing, concurrency,
persistence retries, execution metrics, and task supervision. See
[Writing Monitor Modules](docs/modules.md) for the extension workflow and
invariants. Extensions are compiled into the binary.
