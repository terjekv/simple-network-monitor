use super::{ApiError, ApiState, auth::authorize};
use crate::{
    app::telemetry::{CheckStats, DURATION_BUCKETS},
    domain::{HostRecord, HostStatus, UsageCollectionStatus, host::is_fresh},
};
use actix_web::{HttpRequest, HttpResponse, get, web};
use chrono::{DateTime, Utc};
use std::{
    collections::BTreeMap,
    fmt::{Display, Write},
    time::Duration,
};

#[utoipa::path(get, path = "/metrics", responses(
    (status = 200, description = "Prometheus text exposition", body = String, content_type = "text/plain"),
    (status = 401, description = "Unauthorized"),
    (status = 503, description = "Storage busy")
))]
#[get("/metrics")]
pub(crate) async fn metrics(
    req: HttpRequest,
    state: web::Data<ApiState>,
) -> Result<HttpResponse, ApiError> {
    authorize(&req, &state)?;
    let records = state.hosts.hosts(Default::default()).await?;
    Ok(HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .insert_header(("Cache-Control", "no-store"))
        .body(render(&records, &state.metrics.snapshot(), Utc::now())))
}

// Fixed metric families and a small text encoder avoid a process-global registry.
// Dynamic values appear only in escaped labels; errors and metadata are excluded.
#[derive(Clone, Copy)]
struct Metric(&'static str, &'static str, &'static str);

const ENABLED: Metric = Metric(
    "snm_check_enabled",
    "Whether the configured check is enabled.",
    "gauge",
);
const FRESH: Metric = Metric(
    "snm_check_fresh",
    "Whether an enabled check has a current observation.",
    "gauge",
);
const OBSERVED: Metric = Metric(
    "snm_check_last_observed_timestamp_seconds",
    "Unix timestamp of the latest observation, including failed attempts.",
    "gauge",
);
const ICMP_STATUS: Metric = Metric(
    "snm_icmp_status",
    "Last thresholded ICMP state, one active state per host.",
    "gauge",
);
const RTT: Metric = Metric(
    "snm_icmp_round_trip_seconds",
    "Round trip time of the latest successful ICMP attempt.",
    "gauge",
);
const USAGE_SUCCESS: Metric = Metric(
    "snm_usage_collection_success",
    "Whether the latest usage collection succeeded.",
    "gauge",
);
const USERS: Metric = Metric(
    "snm_usage_users",
    "User count from a fresh successful usage observation.",
    "gauge",
);
const TCP_SUCCESS: Metric = Metric(
    "snm_tcp_connect_success",
    "Whether the latest TCP connection attempt succeeded.",
    "gauge",
);
const TCP_DURATION: Metric = Metric(
    "snm_tcp_connect_duration_seconds",
    "Duration of the latest successful TCP check, including DNS.",
    "gauge",
);
const RUNS: Metric = Metric(
    "snm_check_runs_total",
    "Completed check executions since process start.",
    "counter",
);
const RETRIES: Metric = Metric(
    "snm_storage_retries_total",
    "Observation persistence retries since process start.",
    "counter",
);
const DURATION: Metric = Metric(
    "snm_check_duration_seconds",
    "Execution duration of completed checks, excluding queueing and persistence.",
    "histogram",
);

#[derive(Default)]
struct Exposition(BTreeMap<&'static str, (Metric, String)>);

impl Exposition {
    fn sample(
        &mut self,
        metric: Metric,
        suffix: &str,
        labels: &[(&str, &str)],
        value: impl Display,
    ) {
        let (_, samples) = self
            .0
            .entry(metric.0)
            .or_insert_with(|| (metric, String::new()));
        write!(samples, "{}{suffix}", metric.0).expect("writing to String");
        if !labels.is_empty() {
            samples.push('{');
            for (index, (key, value)) in labels.iter().enumerate() {
                if index > 0 {
                    samples.push(',');
                }
                let value = value
                    .replace('\\', "\\\\")
                    .replace('\n', "\\n")
                    .replace('"', "\\\"");
                write!(samples, "{key}=\"{value}\"").expect("writing to String");
            }
            samples.push('}');
        }
        writeln!(samples, " {value}").expect("writing to String");
    }

    fn finish(self) -> String {
        let mut output = String::new();
        for (Metric(name, help, kind), samples) in self.0.into_values() {
            writeln!(
                output,
                "# HELP {name} {help}\n# TYPE {name} {kind}\n{samples}"
            )
            .expect("writing to String");
        }
        output
    }
}

struct Timing {
    enabled: bool,
    observed: Option<DateTime<Utc>>,
    interval: Duration,
    timeout: Duration,
}

fn common(
    out: &mut Exposition,
    labels: &[(&str, &str)],
    timing: Timing,
    now: DateTime<Utc>,
) -> bool {
    let fresh = timing.enabled && is_fresh(timing.observed, timing.interval, timing.timeout, now);
    out.sample(ENABLED, "", labels, u8::from(timing.enabled));
    out.sample(FRESH, "", labels, u8::from(fresh));
    if timing.enabled
        && let Some(observed) = timing.observed
    {
        out.sample(
            OBSERVED,
            "",
            labels,
            observed.timestamp_millis() as f64 / 1000.0,
        );
    }
    fresh
}

fn render(
    records: &[HostRecord],
    stats: &BTreeMap<&'static str, CheckStats>,
    now: DateTime<Utc>,
) -> String {
    let mut out = Exposition::default();
    for record in records {
        icmp_metrics(&mut out, record, now);
        usage_metrics(&mut out, record, now);
        tcp_metrics(&mut out, record, now);
    }
    for (kind, stats) in stats {
        runtime_metrics(&mut out, kind, stats);
    }
    out.finish()
}

fn icmp_metrics(out: &mut Exposition, record: &HostRecord, now: DateTime<Utc>) {
    let host = &record.host;
    let labels = [("host_id", host.id.as_str())];
    let icmp = &host.modules.icmp;
    let fresh = common(
        out,
        &[("host_id", &host.id), ("check_id", "icmp")],
        Timing {
            enabled: icmp.enabled,
            observed: record.state.last_checked_at,
            interval: icmp.interval,
            timeout: icmp.timeout,
        },
        now,
    );
    if icmp.enabled {
        for status in [HostStatus::Unknown, HostStatus::Up, HostStatus::Down] {
            out.sample(
                ICMP_STATUS,
                "",
                &[("host_id", &host.id), ("state", status.as_str())],
                u8::from(record.state.status == status),
            );
        }
        if fresh && let Some(latency) = record.state.latency {
            out.sample(RTT, "", &labels, latency.as_secs_f64());
        }
    }
}

fn usage_metrics(out: &mut Exposition, record: &HostRecord, now: DateTime<Utc>) {
    let host = &record.host;
    let labels = [("host_id", host.id.as_str())];
    let usage = &host.modules.usage;
    let fresh = common(
        out,
        &[("host_id", &host.id), ("check_id", "usage")],
        Timing {
            enabled: usage.enabled,
            observed: record.usage.as_ref().map(|s| s.collected_at),
            interval: usage.interval,
            timeout: usage.timeout,
        },
        now,
    );
    if usage.enabled
        && let Some(snapshot) = &record.usage
    {
        out.sample(
            USAGE_SUCCESS,
            "",
            &labels,
            u8::from(snapshot.status == UsageCollectionStatus::Ok),
        );
        if fresh && snapshot.status == UsageCollectionStatus::Ok {
            for (session, count) in [
                ("console", snapshot.console_users),
                ("remote", snapshot.remote_users),
            ] {
                if let Some(count) = count {
                    out.sample(
                        USERS,
                        "",
                        &[("host_id", &host.id), ("session", session)],
                        count,
                    );
                }
            }
        }
    }
}

fn tcp_metrics(out: &mut Exposition, record: &HostRecord, now: DateTime<Utc>) {
    let host = &record.host;
    let tcp = &host.modules.tcp;
    for check in &tcp.checks {
        // Prefix reserves separate namespaces for built-ins and named checks.
        let check_id = format!("tcp.{}", check.id.as_str());
        let labels = [
            ("host_id", host.id.as_str()),
            ("check_id", check_id.as_str()),
        ];
        let snapshot = record.tcp.get(&check.id);
        let fresh = common(
            out,
            &labels,
            Timing {
                enabled: tcp.enabled,
                observed: snapshot.map(|s| s.observed_at()),
                interval: tcp.interval,
                timeout: tcp.timeout,
            },
            now,
        );
        if tcp.enabled
            && let Some(snapshot) = snapshot
        {
            out.sample(TCP_SUCCESS, "", &labels, u8::from(snapshot.success()));
            if fresh && let Some(duration) = snapshot.duration() {
                out.sample(TCP_DURATION, "", &labels, duration.as_secs_f64());
            }
        }
    }
}

fn runtime_metrics(out: &mut Exposition, kind: &str, stats: &CheckStats) {
    let labels = [("kind", kind)];
    out.sample(
        RUNS,
        "",
        &[("kind", kind), ("result", "success")],
        stats.successes,
    );
    out.sample(
        RUNS,
        "",
        &[("kind", kind), ("result", "failure")],
        stats.failures,
    );
    out.sample(RETRIES, "", &labels, stats.storage_retries);
    for (boundary, count) in DURATION_BUCKETS.iter().zip(stats.buckets) {
        out.sample(
            DURATION,
            "_bucket",
            &[("kind", kind), ("le", &boundary.to_string())],
            count,
        );
    }
    let total = stats.successes + stats.failures;
    out.sample(
        DURATION,
        "_bucket",
        &[("kind", kind), ("le", "+Inf")],
        total,
    );
    out.sample(DURATION, "_count", &labels, total);
    out.sample(DURATION, "_sum", &labels, stats.duration_sum);
}

#[cfg(test)]
mod tests;
