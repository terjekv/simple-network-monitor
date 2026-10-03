# History and database maintenance

The daemon collects history from new completed ICMP, TCP, and usage observations.
Existing latest-state records cannot reconstruct historical latency, sample counts,
or continuous coverage. Schema migration preserves legacy transition and usage
history; the legacy endpoints remain available. New charts begin collecting on
upgrade. Historical scopes can include removed hosts and former groups.

## Storage and retention

| Data | Default retention | Resolution |
| --- | --- | --- |
| ICMP/TCP observations | 3 days | Every completed poll |
| Usage observations | 30 days (`modules.usage.sample_retention`) | Every completed poll |
| Short-term summaries | 30 days | 5 minutes |
| Medium-term summaries | 365 days | 1 hour |
| Long-term summaries | 1095 days | 1 day |
| Observed change events | 1095 days | Exact collected timestamp |
| Maintenance run records | 30 days | One record per job batch |

Configure tiers under `[history]` in `monitor.example.toml`. Durations must be
between one day and ten years and summary retention must increase with bucket
width. Retention is a time policy, not a hard disk quota. Size depends on host/check
count, polling rate, event churn, and indexes. In particular, usage raw retention
is independent of ICMP/TCP raw retention. Monitor allocated/reusable/WAL bytes in
the Maintenance view and choose windows that fit the deployment.

Ingestion atomically writes the raw observation, change event where appropriate,
and mergeable sample statistics at all three resolutions. Duration spans are
queued durably and materialized in bounded background batches, with a persisted
cursor. Retrying an already persisted observation does not count it again. This
allows raw samples to expire without requiring a scan or recomputation from them.
The read transaction combines materialized durations, remaining queued spans,
and the latest fresh observation tail, so maintenance progress does not change
chart values. A transaction either advances every retained tier or none of them.

Each host/module/check has a versioned identity, including its address, display
name, groups, check port, interval, and timeout. Changes close the old epoch and
start another. Configuration reloads therefore never rewrite old group membership.
No duration bridges an identity change. A result applies until the next result,
an epoch boundary, or its freshness limit (two polling intervals plus timeout).
Gaps after that are unknown, including stopped collectors and service downtime.
Availability uses the thresholded ICMP state; per-poll successes remain separate.
TCP availability across several check IDs is check-time weighted.

Buckets preserve counts, latency sum/min/max, a 17-bin latency histogram, up/down
milliseconds, covered usage milliseconds, and console/remote user-milliseconds.
Mean latency is sample weighted; availability is up time / known up+down time;
coverage is known time / eligible configured time. P95 is a histogram upper-bound
estimate (the overflow bin uses its observed maximum). Users are the duration-
weighted average per observed host. Unknown usage never becomes zero users.
Comparing overlapping groups repeats a host in each relevant group; the combined
union includes each host/check epoch once. Do not add group series together.

## Read-only API

All endpoints follow the existing bearer-token policy and return `Cache-Control:
no-store`. Times are UTC Unix milliseconds; the frontend displays local time and
labels custom inputs UTC.

- `GET /v1/history?from=...&to=...&module=icmp&breakdown=all&max_points=600`
- `GET /v1/history/events?from=...&to=...&module=icmp&limit=100`
- `GET /v1/history/samples?from=...&to=...&module=tcp&check=web&limit=100`
- `GET /v1/system/maintenance`

History accepts `module=icmp|tcp|usage`, an optional TCP `check`, comma-separated
`hosts` and `groups` (up to 16 each), and `breakdown=all|group|host`. When both host
and group scopes are supplied, their intersection is selected. Group selection
uses membership at observation time. Comparisons are limited to 16 series.
`max_points` is 10..1000 per series, default 600. The frontend requests 288.
Duplicate/unknown parameters and invalid ranges are rejected with 400.

The server selects a retained source resolution based on the oldest requested
time, point budget, and estimated query work. It may choose a coarser source for
large fleets. Start/end are rounded outward to source bucket boundaries, capped
at now. Larger output buckets are integer multiples of that source. Responses
include the actual returned range, output/source resolutions, eligible duration,
coverage statistics, and the earliest configured epoch within the retained tier.
Empty buckets carry zero counts and no latency estimate, not a measured zero.
Requests exceeding bounded read capacity receive 503 and can be retried with a
smaller scope/window. Requests return at most 1000 points per series and 16 series;
internal scans cap identity, bucket, and queued-span rows.

Events and raw samples use the same time/scope parameters with a `limit` of
1..1000. Follow `next_before` as the `before` parameter without changing the
window. Results are ordered by descending ingestion ID, which is stable for
pagination but may differ slightly from timestamp order across concurrent hosts.
Retention can remove old records between pages. Initial observations are events,
as are state/user-count/error changes; repeated latency changes alone are samples.

## Scheduled maintenance

The scheduler is separate from probe orchestration and configuration reloads.
It starts with the service, runs one due job at a time, and persists job claims,
completion, last success, next due time, duration, work count, failures, and a
bounded diagnostic message. Interrupted claims recover on startup. Failures back
off from 5 seconds to at most one hour while other due jobs continue. Pending
batches resume after one second, allowing other due work to run. Shutdown waits
for an admitted blocking database operation to finish.

| Job | Default interval | Work |
| --- | --- | --- |
| History rollup | 60 seconds | Up to 500 duration segments |
| Retention | 5 minutes | Up to 500 rows per new history table/tier; legacy tables use their existing 5000-row batches |
| Space reclamation | 1 hour | Up to 1000 free pages when both 256 MiB and 20% free-space thresholds are met |
| Query optimization | 1 day | SQLite `PRAGMA optimize` |
| WAL checkpoint | 5 minutes | Non-blocking/passive checkpoint; SQLite automatic checkpointing stays enabled |
| Job history cleanup | 1 day | Up to 500 expired run records |

The first four intervals, batch size, reclamation thresholds, and page limit are
configurable under `[maintenance]`. Reload applies identities, schedules, and
retention policies atomically without restarting the scheduler. A failed update
leaves the previous monitoring generation and settings active. The view shows `pending`, `running`, `ok`, `error`, or `interrupted`.
The task may complete successfully with zero work, such as when reusable space
is below the threshold. Database errors are logged server-side; the API exposes
only a generic failure message and retry state.

`reusable_bytes` is free pages already available for new writes. Deleting rows
usually does not shrink the database file immediately. Incremental vacuum returns
whole free pages to the filesystem; it does not guarantee removal of all internal
fragmentation. WAL size is displayed separately and includes reusable WAL capacity.
Large or long-lived readers can delay checkpoints. These are SQLite maintenance
primitives, not a replacement for backups or capacity monitoring.

## Existing database setup

New databases enable `auto_vacuum=INCREMENTAL` before creating tables. Existing
databases retain their current vacuum mode; startup never triggers a full rewrite.
To convert and compact an existing database:

1. Stop the monitor service and other database writers; take a consistent backup.
2. Ensure sufficient temporary disk space (SQLite VACUUM can need roughly twice
   the database size in free space).
3. Run `simple-network-monitor --config monitor.toml --compact-database` as the
   service account, using the same config path and working directory as the service.
4. Restart the service and check that Maintenance reports incremental reclamation
   enabled. The command prints confirmation only after successful completion.

The offline command does not start probes or synchronize host identities. It
requires an existing database, takes the same ownership lock as this daemon,
sets incremental vacuum, runs `VACUUM`, and optimizes. The companion `.snm-lock`
file is intentionally persistent; its OS lock is released when the process exits.
Do not delete it while the daemon runs. The lock coordinates this daemon/version,
not arbitrary SQLite clients or older daemon versions, which must also be stopped.
Back up with SQLite's backup mechanism or a stopped database, not a live main-file
copy that omits WAL contents. SQLite schema v4 cannot be opened by older binaries;
keep a pre-upgrade backup for rollback.

## Adding cleanup tasks

Add a `JobKind` with a stable ID and interval in `src/domain/maintenance.rs`, then
implement its bounded, idempotent operation in `src/storage/sqlite/jobs.rs`.
`configure_maintenance` registers it automatically, and the runner provides serial
execution, retry, persistence, and status reporting. Store a transactional cursor
for work spanning batches; never infer completion from a timer or delete source
data before its durable replacement exists. Keep network/external actions outside
the SQLite writer, and add focused restart/failure/retention regression tests.
