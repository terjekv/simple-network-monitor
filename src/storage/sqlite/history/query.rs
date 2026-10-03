use super::*;
use std::collections::BTreeSet;

struct Epoch {
    id: i64,
    host: String,
    name: String,
    groups: Vec<String>,
    start: i64,
    end: i64,
    last: Option<i64>,
    body: Option<String>,
    fresh: i64,
}
fn epochs(
    conn: &Connection,
    request: &HistoryRequest,
    now: i64,
) -> Result<Vec<Epoch>, StorageError> {
    let mut statement = conn.prepare(
        "SELECT id,host_id,name,groups_json,started_ms,ended_ms,last_ms,last_json,freshness_ms,check_id
         FROM history_epochs WHERE module=?1 AND started_ms<?3 AND (ended_ms IS NULL OR ended_ms>?2)
         AND (?4='[]' OR host_id IN (SELECT value FROM json_each(?4)))
         AND (?5='[]' OR EXISTS (SELECT 1 FROM json_each(groups_json) g
                                JOIN json_each(?5) selected ON g.value=selected.value))
         AND (?6 IS NULL OR check_id=?6) LIMIT 10001",
    )?;
    let mut rows = statement.query(params![
        request.module,
        request.from_ms,
        request.to_ms,
        serde_json::to_string(&request.hosts)?,
        serde_json::to_string(&request.groups)?,
        request.check
    ])?;
    let mut result = Vec::new();
    let mut scanned = 0;
    while let Some(r) = rows.next()? {
        scanned += 1;
        if scanned > 10000 {
            return Err(StorageError::Busy);
        }
        let host: String = r.get(1)?;
        let groups: Vec<String> = serde_json::from_str(&r.get::<_, String>(3)?)?;
        result.push(Epoch {
            id: r.get(0)?,
            host,
            name: r.get(2)?,
            groups,
            start: r.get(4)?,
            end: r.get::<_, Option<i64>>(5)?.unwrap_or(now).min(now),
            last: r.get(6)?,
            body: r.get(7)?,
            fresh: r.get(8)?,
        });
    }
    Ok(result)
}
fn labels(epoch: &Epoch, request: &HistoryRequest) -> Vec<(String, String)> {
    match request.breakdown.as_str() {
        "host" => vec![(epoch.host.clone(), epoch.name.clone())],
        "group" => epoch
            .groups
            .iter()
            .filter(|g| request.groups.is_empty() || request.groups.contains(g))
            .map(|g| (g.clone(), g.clone()))
            .collect(),
        _ => vec![("all".into(), "All selected hosts".into())],
    }
}
fn apply_span(
    series: &mut BTreeMap<String, HistorySeries>,
    keys: &[(String, String)],
    start: i64,
    end: i64,
    from: i64,
    step: i64,
    observation: &HistoryObservation,
) {
    let mut cursor = start.max(from);
    let width = step * 1000;
    while cursor < end {
        let index = ((cursor - from) / width) as usize;
        let until = end.min(from + (index as i64 + 1) * width);
        let stats = observation.duration_stats((until - cursor) as u64);
        for (key, _) in keys {
            if let Some(bucket) = series.get_mut(key).and_then(|s| s.buckets.get_mut(index)) {
                bucket.stats.merge(&stats);
            }
        }
        cursor = until;
    }
}
impl SqliteStorage {
    pub async fn history_series(
        &self,
        request: HistoryRequest,
    ) -> Result<HistoryResponse, StorageError> {
        let config = self
            .history_policy
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .clone();
        self.with_reader(move |conn| query(conn, request, config, Utc::now().timestamp_millis()))
            .await
    }
    pub async fn history_events(
        &self,
        request: HistoryRequest,
        before: Option<i64>,
        limit: usize,
        samples: bool,
    ) -> Result<HistoryEventsResponse, StorageError> {
        request
            .validate(Utc::now().timestamp_millis())
            .map_err(|e| StorageError::InvalidQuery(e.into()))?;
        if !(1..=1000).contains(&limit) {
            return Err(StorageError::InvalidQuery("limit must be 1..1000".into()));
        }
        self.with_reader(move |conn| {
            let tx = conn;
            let epochs = epochs(tx, &request, Utc::now().timestamp_millis())?;
            if epochs.is_empty() {
                return Ok(HistoryEventsResponse { events: vec![], next_before: None });
            }
            let ids = epochs.iter().map(|e| e.id.to_string()).collect::<Vec<_>>().join(",");
            let epoch_map: HashMap<_, _> = epochs.iter().map(|e| (e.id, e)).collect();
            let table = if samples { "history_raw" } else { "history_events" };
            let previous = if samples { "NULL" } else { "previous_state" };
            let sql = format!("SELECT r.id,r.epoch_id,r.at_ms,{previous},r.observation,e.module,e.check_id FROM {table} r JOIN history_epochs e ON e.id=r.epoch_id WHERE r.epoch_id IN ({ids}) AND r.at_ms>=?1 AND r.at_ms<?2 AND r.id<?3 ORDER BY r.id DESC LIMIT ?4");
            let mut statement = tx.prepare(&sql)?;
            let mut rows = statement.query(params![request.from_ms, request.to_ms, before.unwrap_or(i64::MAX), (limit + 1) as i64])?;
            let mut events = Vec::new();
            while let Some(r) = rows.next()? {
                let epoch = &epoch_map[&r.get::<_, i64>(1)?];
                events.push(HistoryEvent {
                    id: r.get(0)?,
                    host_id: epoch.host.clone(),
                    name: epoch.name.clone(),
                    groups: epoch.groups.clone(),
                    module: r.get(5)?,
                    check_id: r.get(6)?,
                    at_ms: r.get(2)?,
                    previous_state: r.get(3)?,
                    observation: serde_json::from_str(&r.get::<_, String>(4)?)?,
                });
            }
            let more = events.len() > limit;
            events.truncate(limit);
            let next_before = if more { events.last().map(|e| e.id) } else { None };
            Ok(HistoryEventsResponse { events, next_before })
        })
        .await
    }
}
pub(super) fn query(
    conn: &Connection,
    mut request: HistoryRequest,
    config: HistoryConfig,
    now: i64,
) -> Result<HistoryResponse, StorageError> {
    request
        .validate(now)
        .map_err(|e| StorageError::InvalidQuery(e.into()))?;
    let tx = conn;
    request.to_ms = request.to_ms.min(now);
    // Include identities intersecting the rounded source buckets, including range edges.
    let mut aligned = request.clone();
    aligned.from_ms = floor(request.from_ms, 86400);
    aligned.to_ms = (floor(request.to_ms.saturating_sub(1), 86400) + 86400000).min(now);
    let mut selected = epochs(tx, &aligned, now)?;
    let desired = (request.to_ms - request.from_ms) / (request.max_points as i64 * 1000);
    let mut source = if request.from_ms < cutoff(now, config.hourly_retention) || desired >= 86400 {
        86400
    } else if request.from_ms < cutoff(now, config.five_minute_retention) || desired >= 3600 {
        3600
    } else {
        300
    };
    while source < 86400
        && selected.len() as i64 * ((request.to_ms - request.from_ms) / (source * 1000) + 1)
            > 150_000
    {
        source = if source == 300 { 3600 } else { 86400 };
    }
    let from = floor(request.from_ms, source);
    let to = (floor(request.to_ms.saturating_sub(1), source) + source * 1000).min(now);
    selected.retain(|e| e.start < to && e.end > from);
    let retained_from = floor(cutoff(now, retention(&config, source)), source);
    let step = (((to - from) + (request.max_points as i64 * source * 1000) - 1)
        / (request.max_points as i64 * source * 1000))
        .max(1)
        * source;
    let count = ((to - from + step * 1000 - 1) / (step * 1000)) as usize;
    let mut output: BTreeMap<String, HistorySeries> = BTreeMap::new();
    let mut epoch_keys = HashMap::new();
    for epoch in &selected {
        let keys = labels(epoch, &request);
        for (key, label) in &keys {
            output.entry(key.clone()).or_insert_with(|| HistorySeries {
                id: key.clone(),
                label: label.clone(),
                buckets: (0..count)
                    .map(|i| HistoryBucket {
                        start_ms: from + i as i64 * step * 1000,
                        end_ms: (from + (i as i64 + 1) * step * 1000).min(to),
                        eligible_ms: 0,
                        stats: Default::default(),
                        latency_p95_ms: None,
                    })
                    .collect(),
            });
            if output.len() > 16 {
                return Err(StorageError::InvalidQuery(
                    "select at most 16 series for comparison".into(),
                ));
            }
            // Epochs of a host/check do not overlap. Joining groups never duplicates a combined series.
            for bucket in &mut output.get_mut(key).expect("inserted series").buckets {
                bucket.eligible_ms += epoch
                    .end
                    .min(bucket.end_ms)
                    .saturating_sub(epoch.start.max(bucket.start_ms))
                    .max(0) as u64;
            }
        }
        epoch_keys.insert(epoch.id, keys);
    }
    if !selected.is_empty() {
        let ids = selected
            .iter()
            .map(|e| e.id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = tx.prepare(&format!("SELECT epoch_id,start_ms,stats FROM history_buckets WHERE epoch_id IN ({ids}) AND resolution=?1 AND start_ms>=?2 AND start_ms<?3 LIMIT 200001"))?;
        let mut rows = statement.query(params![source, from.max(retained_from), to])?;
        let mut scanned = 0;
        while let Some(r) = rows.next()? {
            scanned += 1;
            if scanned > 200000 {
                return Err(StorageError::Busy);
            }
            let epoch: i64 = r.get(0)?;
            let at: i64 = r.get(1)?;
            let stats: HistoryStats = serde_json::from_str(&r.get::<_, String>(2)?)?;
            let index = ((at - from) / (step * 1000)) as usize;
            for (key, _) in &epoch_keys[&epoch] {
                if let Some(b) = output.get_mut(key).and_then(|s| s.buckets.get_mut(index)) {
                    b.stats.merge(&stats);
                }
            }
        }
        let mut statement = tx.prepare(&format!("SELECT epoch_id,cursor_ms,end_ms,observation FROM history_spans WHERE epoch_id IN ({ids}) AND cursor_ms<?2 AND end_ms>?1 LIMIT 50001"))?;
        let mut rows = statement.query(params![from, to])?;
        let mut scanned = 0;
        while let Some(r) = rows.next()? {
            scanned += 1;
            if scanned > 50000 {
                return Err(StorageError::Busy);
            }
            let epoch: i64 = r.get(0)?;
            let start: i64 = r.get(1)?;
            let end: i64 = r.get(2)?;
            let observation: HistoryObservation = serde_json::from_str(&r.get::<_, String>(3)?)?;
            apply_span(
                &mut output,
                &epoch_keys[&epoch],
                start.max(retained_from),
                end.min(to),
                from,
                step,
                &observation,
            );
        }
        for epoch in &selected {
            if let (Some(last), Some(body)) = (epoch.last, &epoch.body) {
                let observation: HistoryObservation = serde_json::from_str(body)?;
                apply_span(
                    &mut output,
                    &epoch_keys[&epoch.id],
                    last.max(retained_from),
                    epoch.end.min(to).min(last.saturating_add(epoch.fresh)),
                    from,
                    step,
                    &observation,
                );
            }
        }
    }
    // Rows before a tier's retention boundary are deliberately unavailable, not zero measurements.
    for series in output.values_mut() {
        for bucket in &mut series.buckets {
            if bucket.end_ms <= retained_from {
                bucket.stats = Default::default();
            }
            bucket.latency_p95_ms = bucket.stats.latency_p95();
        }
    }
    let available = selected
        .iter()
        .map(|e| e.start.max(retained_from))
        .collect::<BTreeSet<_>>()
        .first()
        .copied();
    Ok(HistoryResponse {
        from_ms: from,
        to_ms: to,
        resolution_seconds: step,
        source_resolution_seconds: source,
        available_from_ms: available,
        membership: "at_observation_time".into(),
        series: output.into_values().collect(),
    })
}
