//! Finite saved-attempt scan windows; no write-worker or persistent aggregates.
use super::*;
use crate::protocol::{PageCursor, UsageGetArgs};
use chrono::{Duration, FixedOffset, Local, Utc};
use sha2::{Digest, Sha256};

const ROW_LIMIT: usize = 10_000;
// A page may hold at most this much summary JSON plus one lookahead row.
const COMPACTION_PAGE_BYTES: usize = 4 * 1024 * 1024;
const GROUP_LIMIT: u32 = 168;
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}
fn expired() -> ApiError {
    ApiError::new(ErrorCode::CursorExpired, "invalid usage breakdown cursor")
}
fn offset(zone: &str) -> Result<Option<FixedOffset>, ApiError> {
    match zone {
        "local" => Ok(None),
        "UTC" => Ok(FixedOffset::east_opt(0)),
        _ => {
            let b = zone.as_bytes();
            if b.len() != 6
                || !matches!(b[0], b'+' | b'-')
                || b[3] != b':'
                || ![b[1], b[2], b[4], b[5]].iter().all(u8::is_ascii_digit)
            {
                return Err(invalid("timezone must be local, UTC, or +HH:MM/-HH:MM"));
            }
            let hours = (b[1] - b'0') as i32 * 10 + (b[2] - b'0') as i32;
            let minutes = (b[4] - b'0') as i32 * 10 + (b[5] - b'0') as i32;
            if hours > 23 || minutes > 59 {
                return Err(invalid("invalid timezone offset"));
            }
            Ok(FixedOffset::east_opt(
                (hours * 3600 + minutes * 60) * if b[0] == b'-' { -1 } else { 1 },
            ))
        }
    }
}
pub(crate) fn validate(a: &UsageGetArgs) -> Result<(), ApiError> {
    if let Some(group) = &a.group_by {
        if !matches!(group.as_ref(), "hour" | "day" | "thread" | "model") {
            return Err(invalid("group_by must be hour, day, thread, or model"));
        }
        offset(a.timezone.as_deref().unwrap_or("local"))?;
        if !(1..=GROUP_LIMIT).contains(&a.limit.unwrap_or(GROUP_LIMIT)) {
            return Err(invalid("usage group limit must be 1..=168"));
        }
        if a.include_children && a.thread_id.is_none() {
            return Err(invalid(
                "grouped include_children requires an explicit thread",
            ));
        }
        window(a, None)?;
    } else if a.timezone.is_some() || a.limit.is_some() || a.cursor.is_some() {
        return Err(invalid("timezone, limit and cursor require group_by"));
    }
    Ok(())
}
fn window(a: &UsageGetArgs, saved: Option<&Scan>) -> Result<crate::readback::Window, ApiError> {
    let until = a
        .until
        .clone()
        .or_else(|| saved.map(|s| s.until.clone()))
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let end = DateTime::parse_from_rfc3339(&until).map_err(|_| invalid("invalid until"))?;
    let since = a
        .since
        .clone()
        .or_else(|| saved.map(|s| s.since.clone()))
        .map(Ok)
        .unwrap_or_else(|| {
            end.checked_sub_signed(Duration::hours(24))
                .map(|t| t.to_rfc3339())
                .ok_or_else(|| invalid("invalid default period"))
        })?;
    let w = crate::readback::Window::new(Some(&since), Some(&until))?;
    if w.until.unwrap() - w.since.unwrap() > Duration::days(31) {
        return Err(invalid("grouped usage period must not exceed 31 days"));
    }
    Ok(w)
}
#[derive(Serialize, Deserialize)]
struct Scan {
    since: String,
    until: String,
    after: i64,
    compaction_after: i64,
    compaction_watermark: i64,
    group_offset: usize,
    continued: bool,
}
fn query() -> String {
    // Page by the existing primary rowid before time/scope filtering: even an
    // empty period cannot make one request walk the full attempt table.
    let source = "CASE WHEN a.started_at>=?1 AND a.started_at<?2 THEN a.usage_json END";
    format!(
        "WITH candidates AS MATERIALIZED (
        SELECT rowid scan_id,id FROM model_attempt WHERE rowid>?3 AND rowid<=?4
        ORDER BY rowid LIMIT {}
        ) SELECT c.scan_id,a.id,a.turn_id,u.run_id,u.thread_id,t.deleted_at,
            a.state,a.started_at,a.ended_at,
            CASE WHEN a.started_at>=?1 AND a.started_at<?2 THEN 1 ELSE 0 END,{},
            a.profile_id,a.model_id,
            CASE WHEN json_type(a.input_manifest_json,'$.rate_observation.status')='integer' AND typeof(json_extract(a.input_manifest_json,'$.rate_observation.status'))='integer' THEN json_extract(a.input_manifest_json,'$.rate_observation.status') END,
            CASE WHEN json_type(a.input_manifest_json,'$.rate_observation.applied_wait_ms')='integer' AND typeof(json_extract(a.input_manifest_json,'$.rate_observation.applied_wait_ms'))='integer' THEN json_extract(a.input_manifest_json,'$.rate_observation.applied_wait_ms') END
        FROM candidates c JOIN model_attempt a ON a.id=c.id
        JOIN turn u ON u.id=a.turn_id JOIN thread t ON t.id=u.thread_id
        ORDER BY c.scan_id",
        ROW_LIMIT + 1,
        usage::sql_projection(source)
    )
}
fn saved_utc(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?;
    if raw.len() > 64 || !raw.ends_with("+00:00") {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|v| v.with_timezone(&Utc))
}

struct Row {
    key: i64,
    id: String,
    turn: String,
    run: String,
    thread: String,
    deleted: bool,
    state: String,
    started: Option<String>,
    ended: Option<String>,
    in_range: bool,
    usage: usage::StandardUsage,
    profile: String,
    model: String,
    rate_status: Option<u64>,
    rate_wait: Option<u64>,
}
#[derive(Default, Serialize)]
struct Combined {
    model_attempt_records: u64,
    summary_requests: u64,
    input_tokens: Number,
    cached_tokens: Number,
    cache_creation_tokens: Number,
    output_tokens: Number,
    reasoning_tokens: Number,
    total_tokens: Number,
}
#[derive(Default)]
struct Group {
    attempts: BTreeMap<String, Attempts>,
    compaction: crate::compaction::SummaryUsage,
    compactions: u64,
    rates: BTreeMap<String, RateCounts>,
    combined: Combined,
    inputs: Vec<u64>,
    unknown_inputs: u64,
    max_input: Option<(u64, Value)>,
    paired: u64,
    unpaired: u64,
    paired_input: u128,
    paired_cache: u128,
}
impl Group {
    fn add(&mut self, row: Row) {
        self.rates
            .entry(row.profile.clone())
            .or_default()
            .add(row.rate_status, row.rate_wait);
        let terminal = matches!(row.state.as_str(), "completed" | "failed" | "incomplete");
        let group = self.attempts.entry(row.state.clone()).or_default();
        group.count += 1;
        let u = if terminal {
            row.usage
        } else {
            usage::StandardUsage::default()
        };
        let fields = [
            u.input_tokens,
            u.cached_tokens,
            u.output_tokens,
            u.reasoning_tokens,
            u.total_tokens,
            saved_ms(row.started.as_deref(), row.ended.as_deref())
                .and_then(|n| u64::try_from(n).ok()),
        ];
        self.combined.model_attempt_records = self.combined.model_attempt_records.saturating_add(1);
        for (number, value) in [
            &mut self.combined.input_tokens,
            &mut self.combined.cached_tokens,
            &mut self.combined.output_tokens,
            &mut self.combined.reasoning_tokens,
            &mut self.combined.total_tokens,
        ]
        .into_iter()
        .zip([
            u.input_tokens,
            u.cached_tokens,
            u.output_tokens,
            u.reasoning_tokens,
            u.total_tokens,
        ]) {
            number.add(value.map(u128::from), u64::from(value.is_none()));
        }
        // Grouped SQL intentionally leaves this existing saved counter unread.
        self.combined.cache_creation_tokens.add(None, 1);
        for (number, value) in group.fields().into_iter().zip(fields) {
            number.add(value.map(u128::from), u64::from(value.is_none()));
        }
        let start = start_key(row.started.as_deref());
        if let Some(key) = start {
            if group.first_start_key.as_ref().is_none_or(|v| v > &key) {
                group.first_start_key = Some(key.clone());
            }
            if group.last_start_key.as_ref().is_none_or(|v| v < &key) {
                group.last_start_key = Some(key);
            }
        } else {
            group.unknown_start_count += 1;
        }
        if !terminal {
            return;
        }
        if let Some(input) = u.input_tokens {
            self.inputs.push(input);
            let id = &row.id;
            let replace = self.max_input.as_ref().is_none_or(|(n, r)| {
                input > *n || (input == *n && id.as_str() < r["attempt_id"].as_str().unwrap_or(""))
            });
            if replace {
                self.max_input = Some((
                    input,
                    json!({"attempt_id":id,"turn_id":row.turn,"run_id":row.run,"thread_id":row.thread}),
                ));
            }
        } else {
            self.unknown_inputs += 1;
        }
        if let (Some(input), Some(cache)) = (u.input_tokens, u.cached_tokens) {
            self.paired += 1;
            self.paired_input += input as u128;
            self.paired_cache += cache as u128;
        } else {
            self.unpaired += 1;
        }
    }
    fn add_compaction(&mut self, usage: &crate::compaction::SummaryUsage) {
        self.compactions = self.compactions.saturating_add(1);
        self.compaction.merge(usage);
    }
    fn json(mut self, key: &str, include_compaction: bool) -> Value {
        self.inputs.sort_unstable();
        let n = self.inputs.len();
        let percentile = |p: usize| {
            n.checked_sub(1)
                .map(|_| self.inputs[(n * p).div_ceil(100) - 1])
        };
        let mean =
            (n > 0).then(|| self.inputs.iter().map(|n| *n as u128).sum::<u128>() as f64 / n as f64);
        if include_compaction {
            self.combined.summary_requests = self.compaction.requests;
        }
        for (combined, summary) in [
            (
                &mut self.combined.input_tokens,
                &self.compaction.input_tokens,
            ),
            (
                &mut self.combined.cached_tokens,
                &self.compaction.cached_tokens,
            ),
            (
                &mut self.combined.cache_creation_tokens,
                &self.compaction.cache_creation_tokens,
            ),
            (
                &mut self.combined.output_tokens,
                &self.compaction.output_tokens,
            ),
        ] {
            if include_compaction {
                combined.add(summary.known_sum.map(u128::from), summary.unknown_count);
                if summary.overflow {
                    combined.overflow = true;
                    combined.known_sum = None;
                }
            }
        }
        if include_compaction {
            self.combined
                .reasoning_tokens
                .add(None, self.compaction.requests);
            self.combined
                .total_tokens
                .add(None, self.compaction.requests);
        }
        let compaction = if include_compaction {
            let mut compaction = json!({"compactions":self.compactions});
            if let (Value::Object(target), Value::Object(fields)) =
                (&mut compaction, json!(self.compaction))
            {
                target.extend(fields);
            }
            compaction
        } else {
            json!({"compactions":null,"coverage":"summary usage is not attributed by model"})
        };
        let total = if include_compaction {
            json!(self.combined)
        } else {
            json!({"model_attempt_records":self.combined.model_attempt_records,
                "summary_requests":null,"input_tokens":self.combined.input_tokens,
                "cached_tokens":self.combined.cached_tokens,"cache_creation_tokens":null,
                "output_tokens":self.combined.output_tokens,"reasoning_tokens":self.combined.reasoning_tokens,
                "total_tokens":self.combined.total_tokens,"coverage":"summary usage is not attributed by model"})
        };
        json!({"key":key,"model_attempts":self.attempts,"compaction":compaction,
            "connection_rates":{"by_connection":self.rates,"coverage":"current candidate window; unknown facts excluded from counts"},
            "total":total,
            "input_per_request":{"states":["completed","failed","incomplete"],"known_count":n,"unknown_count":self.unknown_inputs,
            "mean":mean,"max":self.inputs.last(),"p50":percentile(50),"p95":percentile(95),
            "percentile_method":"nearest_rank","max_attempt":self.max_input.map(|(_, r)|r)},
            "cache":{"ratio":(self.paired_input>0).then(||self.paired_cache as f64/self.paired_input as f64),
                "paired_count":self.paired,"unknown_pair_count":self.unpaired}})
    }
}
fn bucket_time(time: DateTime<FixedOffset>, group: &str, zone: Option<FixedOffset>) -> String {
    let local = match zone {
        Some(offset) => time.with_timezone(&offset),
        None => time.with_timezone(&Local).fixed_offset(),
    };
    local
        .format(if group == "hour" {
            "%Y-%m-%dT%H:00:00%:z"
        } else {
            "%Y-%m-%d"
        })
        .to_string()
}
fn bucket(row: &Row, group: &str, zone: Option<FixedOffset>) -> Option<String> {
    if group == "thread" {
        return Some(row.thread.clone());
    }
    if group == "model" {
        return Some(row.model.clone());
    }
    let time = DateTime::parse_from_rfc3339(row.started.as_deref()?).ok()?;
    Some(bucket_time(time, group, zone))
}
fn compaction_query() -> String {
    format!(
        "SELECT rowid,CASE WHEN octet_length(key)<=?3 THEN key ELSE '' END,
         CASE WHEN octet_length(value)<=?3 THEN value END
         FROM app_meta WHERE rowid>?1 AND rowid<=?2 ORDER BY rowid LIMIT {}",
        ROW_LIMIT + 1
    )
}

pub(crate) fn read(db: &Connection, a: &UsageGetArgs, instance: &str) -> Result<Value, ApiError> {
    validate(a)?;
    let started = std::time::Instant::now();
    let mut filter_args = a.clone();
    filter_args.cursor = None;
    let filter = Sha256::digest(serde_json::to_vec(&(instance, &filter_args)).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let cursor = a
        .cursor
        .as_deref()
        .map(|s| PageCursor::decode(s, "usage.groups.v2", &filter, 0, None))
        .transpose()?;
    let saved: Option<Scan> = cursor
        .as_ref()
        .map(|c| serde_json::from_str(&c.last_sort_key).map_err(|_| expired()))
        .transpose()?;
    let w = window(a, saved.as_ref())?;
    let group_by = a
        .group_by
        .as_deref()
        .ok_or_else(|| invalid("group_by required"))?;
    let include_compaction = group_by != "model";
    let initial_compaction_watermark = if include_compaction {
        db.query_row("SELECT COALESCE(max(rowid),0) FROM app_meta", [], |r| {
            r.get(0)
        })
        .map_err(error)?
    } else {
        0
    };
    let mut scan = saved.unwrap_or_else(|| Scan {
        since: w.since.unwrap().to_rfc3339(),
        until: w.until.unwrap().to_rfc3339(),
        after: 0,
        compaction_after: 0,
        compaction_watermark: initial_compaction_watermark,
        group_offset: 0,
        continued: false,
    });
    if scan.group_offset > ROW_LIMIT {
        return Err(expired());
    }
    let watermark = match &cursor {
        Some(c) => i64::try_from(c.watermark).map_err(|_| expired())?,
        None => db
            .query_row(
                "SELECT COALESCE(max(rowid),0) FROM model_attempt",
                [],
                |r| r.get(0),
            )
            .map_err(error)?,
    };
    let scope = if let Some(id) = &a.turn_id {
        Some(Scope::Turn(id.clone()))
    } else if let Some(id) = &a.run_id {
        Some(Scope::Run(id.clone()))
    } else {
        a.thread_id.as_ref().map(|id| Scope::Thread {
            id: id.clone(),
            children: a.include_children,
        })
    };
    let selection = scope
        .as_ref()
        .map(|s| selection(db, s, &mut Trace::default()))
        .transpose()?;
    // Coarse whole-second string bounds include both fractional boundary
    // seconds. The bounded Rust pass validates UTC and exact [since,until).
    let lower = w.since.unwrap().format("%Y-%m-%dT%H:%M:%S").to_string();
    let upper = w
        .until
        .unwrap()
        .checked_add_signed(Duration::seconds(1))
        .ok_or_else(|| invalid("invalid upper period"))?
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let mut statement = db.prepare(&query()).map_err(error)?;
    let mut rows = statement
        .query_map(params![lower, upper, scan.after, watermark], |r| {
            Ok(Row {
                key: r.get(0)?,
                id: r.get(1)?,
                turn: r.get(2)?,
                run: r.get(3)?,
                thread: r.get(4)?,
                deleted: r.get::<_, Option<String>>(5)?.is_some(),
                state: r.get(6)?,
                started: r.get(7)?,
                ended: r.get(8)?,
                in_range: r.get(9)?,
                usage: usage::StandardUsage::read(r, 10)?,
                profile: r.get(15)?,
                model: r.get(16)?,
                rate_status: r
                    .get::<_, Option<i64>>(17)?
                    .and_then(|v| u64::try_from(v).ok()),
                rate_wait: r
                    .get::<_, Option<i64>>(18)?
                    .and_then(|v| u64::try_from(v).ok()),
            })
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    let row_more = rows.len() > ROW_LIMIT;
    rows.truncate(ROW_LIMIT);
    let scanned = rows.len();
    let tail = rows.last().map(|r| r.key);
    let mut compaction_statement = if include_compaction {
        Some(db.prepare(&compaction_query()).map_err(error)?)
    } else {
        None
    };
    let mut candidate_rows = match compaction_statement.as_mut() {
        Some(statement) => Some(
            statement
                .query(params![
                    scan.compaction_after,
                    scan.compaction_watermark,
                    crate::compaction::SUMMARY_USAGE_MAX_BYTES
                ])
                .map_err(error)?,
        ),
        None => None,
    };
    let mut compaction_rows = Vec::new();
    let mut compaction_bytes = 0usize;
    let mut compaction_row_more = false;
    while let Some(row) = match candidate_rows.as_mut() {
        Some(rows) => rows.next().map_err(error)?,
        None => None,
    } {
        let id: i64 = row.get(0).map_err(error)?;
        let key_bytes = row
            .get_ref(1)
            .map_err(error)?
            .as_bytes()
            .map_err(|e| {
                error(rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                ))
            })?
            .len();
        let body_bytes = match row.get_ref(2).map_err(error)? {
            rusqlite::types::ValueRef::Null => 0,
            value => value
                .as_bytes()
                .map_err(|e| {
                    error(rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    ))
                })?
                .len(),
        };
        let bytes = key_bytes.saturating_add(body_bytes);
        if compaction_rows.len() == ROW_LIMIT
            || (!compaction_rows.is_empty()
                && compaction_bytes.saturating_add(bytes) > COMPACTION_PAGE_BYTES)
        {
            compaction_row_more = true;
            break;
        }
        compaction_bytes = compaction_bytes.saturating_add(bytes);
        let key: String = row.get(1).map_err(error)?;
        let body: Option<String> = row.get(2).map_err(error)?;
        compaction_rows.push((id, key, body));
    }
    let compaction_scanned = compaction_rows.len();
    let compaction_tail = compaction_rows.last().map(|row| row.0);
    let zone = offset(a.timezone.as_deref().unwrap_or("local"))?;
    let group_by = a
        .group_by
        .as_deref()
        .ok_or_else(|| invalid("group_by required"))?;
    let mut groups = BTreeMap::<String, Group>::new();
    let mut matched = 0;
    let mut matched_compactions = 0u64;
    let mut malformed_compactions = 0u64;
    let mut unknown_time_compactions = 0u64;
    let mut excluded_times = 0;
    let mut outside_period = 0;
    for row in rows {
        let Some(at) = saved_utc(row.started.as_deref()) else {
            excluded_times += 1;
            continue;
        };
        if !row.in_range || at < w.since.unwrap() || at >= w.until.unwrap() {
            outside_period += 1;
            continue;
        }
        if row.deleted {
            continue;
        }
        if let Some(s) = &selection {
            let matches = match s.kind {
                "turn" => a.turn_id.as_ref() == Some(&row.turn),
                "run" => a.run_id.as_ref() == Some(&row.run),
                _ => s.threads.contains(&row.thread),
            };
            if !matches {
                continue;
            }
        }
        if let Some(key) = bucket(&row, group_by, zone) {
            matched += 1;
            groups.entry(key).or_default().add(row);
        }
    }
    let mut live_thread = db
        .prepare("SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL")
        .map_err(error)?;
    for (_rowid, key, body) in &compaction_rows {
        let Some(suffix) = key.strip_prefix(crate::compaction::SUMMARY_USAGE_PREFIX) else {
            continue;
        };
        if selection.as_ref().is_some_and(|selected| {
            !selected
                .threads
                .iter()
                .any(|id| suffix.starts_with(&format!("{id}.")))
        }) {
            continue;
        }
        let Some(body) = body.as_deref() else {
            malformed_compactions = malformed_compactions.saturating_add(1);
            continue;
        };
        let Ok(mut raw) = serde_json::from_str::<Value>(body) else {
            malformed_compactions = malformed_compactions.saturating_add(1);
            continue;
        };
        let started_at = raw
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !raw.get("started_at").is_some_and(Value::is_string) {
            let Some(fields) = raw.as_object_mut() else {
                malformed_compactions = malformed_compactions.saturating_add(1);
                continue;
            };
            // Keep an otherwise valid legacy record countable when its time is absent.
            fields.insert("started_at".into(), Value::String(String::new()));
        }
        let Ok(record) = serde_json::from_value::<crate::compaction::SummaryUsageRecord>(raw)
        else {
            malformed_compactions = malformed_compactions.saturating_add(1);
            continue;
        };
        if key
            != &format!(
                "{}{}.{}",
                crate::compaction::SUMMARY_USAGE_PREFIX,
                record.thread_id,
                record.id
            )
            || !suffix.starts_with(&format!("{}.", record.thread_id))
        {
            malformed_compactions = malformed_compactions.saturating_add(1);
            continue;
        }
        if live_thread
            .query_row([&record.thread_id], |_| Ok(()))
            .optional()
            .map_err(error)?
            .is_none()
            || selection.as_ref().is_some_and(|selected| {
                selected.kind == "turn" && record.turn_id.as_deref() != a.turn_id.as_deref()
            })
            || selection.as_ref().is_some_and(|selected| {
                selected.kind == "run" && record.run_id.as_deref() != a.run_id.as_deref()
            })
        {
            continue;
        }
        let Ok(at) = DateTime::parse_from_rfc3339(&started_at) else {
            unknown_time_compactions = unknown_time_compactions.saturating_add(1);
            continue;
        };
        let at_utc = at.with_timezone(&Utc);
        if at_utc < w.since.unwrap() || at_utc >= w.until.unwrap() {
            continue;
        }
        let group_key = if group_by == "thread" {
            record.thread_id.clone()
        } else {
            bucket_time(at, group_by, zone)
        };
        matched_compactions = matched_compactions.saturating_add(1);
        let group = groups.entry(group_key).or_default();
        group.add_compaction(&record.usage);
        let entry = group.rates.entry(record.profile_id).or_default();
        let observed = (record.rate_observations.len() as u64).min(record.usage.requests);
        for fact in record.rate_observations.into_iter().take(observed as usize) {
            entry.add(
                fact.get("status").and_then(Value::as_u64),
                fact.get("applied_wait_ms").and_then(Value::as_u64),
            );
        }
        let unknown = record.usage.requests.saturating_sub(observed);
        entry.status_unknown = entry.status_unknown.saturating_add(unknown);
        entry.wait_unknown = entry.wait_unknown.saturating_add(unknown);
    }
    let group_more = groups.len() > scan.group_offset + a.limit.unwrap_or(GROUP_LIMIT) as usize;
    let items = groups
        .into_iter()
        .skip(scan.group_offset)
        .take(a.limit.unwrap_or(GROUP_LIMIT) as usize)
        .map(|(k, g)| g.json(&k, include_compaction))
        .collect::<Vec<_>>();
    let continued = scan.continued;
    let more = group_more || row_more || compaction_row_more;
    let next_cursor = if more {
        if group_more {
            scan.group_offset += items.len();
        } else {
            scan.after = tail.unwrap_or(scan.after);
            scan.compaction_after = compaction_tail.unwrap_or(scan.compaction_after);
            scan.group_offset = 0;
            scan.continued = true;
        }
        Some(
            PageCursor {
                version: 1,
                scope: "usage.groups.v2".into(),
                filter,
                last_sort_key: serde_json::to_string(&scan).unwrap(),
                watermark: watermark as u64,
                revision: 0,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(
        json!({"thread_id":selection.as_ref().and_then(|s|s.threads.first()).filter(|id|id.len()<=512),"group_by":group_by,"timezone":a.timezone.as_deref().unwrap_or("local"),"requested_period":w.json(),
        "items":items,"more":more,"next_cursor":next_cursor,"scanned_candidates":scanned,"matched_attempts":matched,
        "scanned_compaction_candidates":if include_compaction {json!(compaction_scanned)} else {Value::Null},"scanned_compaction_bytes":if include_compaction {json!(compaction_bytes)} else {Value::Null},"compaction_byte_limit":COMPACTION_PAGE_BYTES,"matched_compactions":if include_compaction {json!(matched_compactions)} else {Value::Null},
        "malformed_compaction_records":if include_compaction {json!(malformed_compactions)} else {Value::Null},"unknown_time_compaction_records":if include_compaction {json!(unknown_time_compactions)} else {Value::Null},
        "excluded_time_count":excluded_times,"outside_period_count":outside_period,"row_limit":ROW_LIMIT,"group_limit":a.limit.unwrap_or(GROUP_LIMIT),"scan_truncated":row_more || compaction_row_more || continued,
        "scope_complete":selection.as_ref().is_none_or(|s|s.complete),"statistics_scope":"current_candidate_window",
        "coverage":{"unknown_starts":"excluded_time_count includes missing, malformed and non-current UTC formats","summary":if include_compaction {"recorded_first_request_at"} else {"not_attributed_by_model"},"summary_unknown_time":if include_compaction {"unknown_time_compaction_records cannot be assigned to a period"} else {"not_scanned_for_model_group"},"summary_malformed":if include_compaction {"malformed_compaction_records are excluded, never treated as zero"} else {"not_scanned_for_model_group"},"embeddings":"not_recorded"},"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}),
    )
}

#[cfg(test)]
mod tests;
