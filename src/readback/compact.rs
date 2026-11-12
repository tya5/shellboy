use super::*;
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;
mod bounded;
pub(super) use bounded::read_display;
const AUTO: &str = "CASE WHEN json_valid(a.error_json) THEN json_type(a.error_json,'$.automatic_trim')='object' OR json_type(a.error_json,'$.automatic_trim_attempt')='object' ELSE 0 END";
pub(super) const SPILL_COVERAGE_SQL: &str = "SELECT r.output_node_id FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id WHERE tr.thread_id=?1 AND r.format_version='spill-v1' AND usage_saved_start(r.created_at)>=usage_saved_start(?2) AND usage_saved_start(r.created_at)<=usage_saved_start(?3) AND NOT EXISTS(SELECT 1 FROM replacement_source q WHERE q.replacement_id=r.id AND NOT EXISTS(SELECT 1 FROM app_meta m,json_each(json_extract(m.value,'$.outcome.result.compact.original_node_ids')) j WHERE m.key=?4 AND j.value=q.source_node_id)) ORDER BY usage_saved_start(r.created_at) DESC,r.id DESC LIMIT ?5";
fn metrics(root: &str) -> String {
    let paths = [
        ("before_characters", "before_characters"),
        ("after_characters", "after_characters"),
        ("omitted_nodes", "omitted_nodes"),
        ("marker_node_id", "marker_node_id"),
    ];
    let mut fields = paths
        .iter()
        .map(|(key, path)| format!("'{key}',json_extract({root},'$.{path}')"))
        .collect::<Vec<_>>();
    for field in [
        "source_nodes",
        "summary_characters",
        "summary_limit",
        "source_characters",
        "model_requests",
        "spilled_groups",
        "summary_node_id",
        "summary_rejected",
        "no_target",
    ] {
        fields.push(format!("'{field}',COALESCE(json_extract({root},'$.compact.{field}'),json_extract({root},'$.{field}'))"));
    }
    // #133: per-compaction summary request usage, an object (copied, then
    // re-encoded by `parse`).
    fields.push(format!("'summary_usage',json(COALESCE(json_extract({root},'$.compact.summary_usage'),json_extract({root},'$.summary_usage')))"));
    for (key, path) in [("first_node_id", "[0]"), ("last_node_id", "[#-1]")] {
        fields.push(format!("'{key}',COALESCE(json_extract({root},'$.compact.original_node_ids{path}'),json_extract({root},'$.original_node_ids{path}'))"));
    }
    format!("json_object({})", fields.join(","))
}
fn output_node(root: &str) -> String {
    format!(
        "COALESCE(json_extract({root},'$.compact.summary_node_id'),json_extract({root},'$.summary_node_id'),json_extract({root},'$.marker_node_id'))"
    )
}
fn enriched(root: &str) -> String {
    let base = metrics(root);
    format!(
        "json_set({base},'$.after_current_request_nodes',CASE WHEN json_extract({root},'$.omitted_nodes')=0 THEN 0 ELSE json_extract(n.payload_json,'$.context_trim.after_current_request_nodes') END,'$.summary_characters',COALESCE(json_extract({root},'$.compact.summary_characters'),json_extract({root},'$.summary_characters'),CASE WHEN n.kind='summary' THEN readback_chars(json_extract(n.payload_json,'$.item.content[0].text')) END),'$.summary_usage',json(COALESCE(json_extract({root},'$.compact.summary_usage'),json_extract({root},'$.summary_usage'),CASE WHEN n.kind='summary' THEN json_extract(n.payload_json,'$.summary_usage') END)))"
    )
}
pub(super) fn receipt_query(w: &Window) -> String {
    receipt_source_query(w, false)
}
fn receipt_source_query(w: &Window, display: bool) -> String {
    let join = if display { "CROSS JOIN" } else { "JOIN" };
    let source = if display {
        "operation_receipt o"
    } else {
        "operation_receipt o INDEXED BY compact_operation_thread"
    };
    let meta = "CASE WHEN json_valid(m.value) THEN m.value ELSE '{}' END";
    let root = format!(
        "COALESCE(json_extract({meta},'$.outcome.result'),json_extract({meta},'$.outcome.error.details'),'{{}}')"
    );
    let kind = "COALESCE(json_extract(o.result_ref_json,'$.result.context_edit.action'),'compact')";
    let at = format!("COALESCE(n.created_at,json_extract({meta},'$.ended_at'))");
    let node = output_node(&root);
    let group = format!(
        "CASE WHEN {kind}='restore' THEN o.operation_id ELSE COALESCE({node},o.operation_id) END"
    );
    let spilled = format!(
        "COALESCE(json_extract({root},'$.compact.spilled_groups'),json_extract({root},'$.spilled_groups'))"
    );
    format!(
        "SELECT min(o.operation_id),o.created_at,{at},{kind},{},json_extract({meta},'$.state'),count(*),CASE WHEN typeof({spilled})='integer' AND {spilled}>0 THEN {spilled} ELSE 0 END FROM {source} {join} app_meta m ON m.key='compact.result.'||o.operation_id LEFT JOIN node n ON n.id={node} AND {kind}<>'restore' WHERE CASE WHEN json_valid(o.result_ref_json) THEN json_extract(o.result_ref_json,'$.result.status')='queued' ELSE 0 END AND json_extract(o.result_ref_json,'$.result.thread_id')=?1 AND json_valid(m.value) AND {} GROUP BY {group} ORDER BY usage_saved_start({at}) DESC,min(o.operation_id) DESC LIMIT ?2",
        enriched(&root),
        w.sql(&at)
    )
}
pub(super) fn automatic_query(w: &Window) -> String {
    automatic_source_query(w, false)
}
fn automatic_source_query(w: &Window, display: bool) -> String {
    let source = if display {
        "turn u JOIN model_attempt a"
    } else {
        "turn u INDEXED BY turns_by_thread JOIN model_attempt a INDEXED BY automatic_compaction_turn"
    };
    let failures = if display {
        "(SELECT CASE WHEN count(*)<201 THEN sum(state IN ('failed','incomplete') AND native_rowid<=a.rowid) END FROM (SELECT rowid native_rowid,state FROM main.model_attempt INDEXED BY attempts_by_turn WHERE turn_id=a.turn_id ORDER BY started_at DESC LIMIT 201))"
    } else {
        "(SELECT count(*) FROM model_attempt earlier INDEXED BY attempts_by_turn WHERE earlier.turn_id=a.turn_id AND earlier.rowid<=a.rowid AND earlier.state IN ('failed','incomplete'))"
    };
    let root = "COALESCE(json_extract(a.error_json,'$.automatic_trim'),json_extract(a.error_json,'$.automatic_trim_attempt.result'),json_extract(a.error_json,'$.automatic_trim_attempt.summary'),json_extract(a.error_json,'$.automatic_trim_attempt.error.details'),'{}')";
    let node = output_node(root);
    let at = "COALESCE(n.created_at,a.ended_at,a.started_at)";
    format!(
        "SELECT a.id,{at},{},json_extract(a.error_json,'$.error.kind'),json_extract(a.error_json,'$.automatic_retry.number'),a.turn_id,COALESCE(json_extract(a.error_json,'$.automatic_trim_attempt.outcome'),'recorded'),{failures},CASE WHEN json_type(a.error_json,'$.automatic_trim_attempt.reason')='text' THEN json_extract(a.error_json,'$.automatic_trim_attempt.reason') END,CASE WHEN json_type(a.error_json,'$.automatic_trim_attempt.trigger')='object' THEN json_extract(a.error_json,'$.automatic_trim_attempt.trigger') END FROM {source} ON a.turn_id=u.id LEFT JOIN node n ON n.id={node} WHERE u.thread_id=?1 AND {AUTO} AND {} ORDER BY usage_saved_start({at}) DESC,a.id DESC LIMIT ?2",
        enriched(root),
        w.sql(at)
    )
}
pub(super) fn replacement_query(w: &Window) -> String {
    replacement_source_query(w, false)
}
fn replacement_source_query(w: &Window, display: bool) -> String {
    let join = if display { "CROSS JOIN" } else { "JOIN" };
    let source_count = if display {
        "(SELECT CASE WHEN count(*)<201 THEN count(*) END FROM (SELECT 1 FROM replacement_source q WHERE q.replacement_id=r.id LIMIT 201))"
    } else {
        "(SELECT count(*) FROM replacement_source q WHERE q.replacement_id=r.id)"
    };
    format!(
        "SELECT r.output_node_id,r.created_at,r.format_version,json_object('source_nodes',{source_count},'first_node_id',(SELECT source_node_id FROM replacement_source q WHERE q.replacement_id=r.id ORDER BY ordinal LIMIT 1),'last_node_id',(SELECT source_node_id FROM replacement_source q WHERE q.replacement_id=r.id ORDER BY ordinal DESC LIMIT 1),'before_characters',json_extract(n.payload_json,'$.context_trim.history_characters'),'omitted_nodes',json_extract(n.payload_json,'$.context_trim.omitted_nodes'),'after_current_request_nodes',json_extract(n.payload_json,'$.context_trim.after_current_request_nodes'),'summary_characters',CASE WHEN n.kind='summary' THEN readback_chars(json_extract(n.payload_json,'$.item.content[0].text')) END,'summary_usage',json(CASE WHEN n.kind='summary' THEN json_extract(n.payload_json,'$.summary_usage') END),'reason',json_extract(n.payload_json,'$.context_trim.reason')),EXISTS(SELECT 1 FROM main.thread f JOIN main.thread_replacement p ON p.thread_id=f.parent_thread_id WHERE f.id=?1 AND p.replacement_id=r.id) FROM thread_replacement tr {join} replacement r ON r.id=tr.replacement_id {join} node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND {} ORDER BY usage_saved_start(r.created_at) DESC,r.id DESC LIMIT ?2",
        w.sql("r.created_at")
    )
}
fn parse(raw: String) -> Result<Value, ApiError> {
    {
        let mut v: Value =
            serde_json::from_str(&raw).map_err(|_| invalid("malformed compaction projection"))?;
        for (key, value) in v
            .as_object_mut()
            .ok_or_else(|| invalid("malformed compaction metrics"))?
        {
            if [
                "first_node_id",
                "last_node_id",
                "summary_node_id",
                "marker_node_id",
                "reason",
            ]
            .contains(&key.as_str())
            {
                if value.as_str().is_none_or(|s| s.len() > 1024) {
                    *value = Value::Null;
                }
            } else if key == "summary_usage" {
                *value = crate::compaction::SummaryUsage::sanitize(value);
            } else if key == "no_target" || key == "summary_rejected" {
                if let Some(n) = value.as_i64().filter(|n| *n == 0 || *n == 1) {
                    *value = json!(n == 1);
                } else if !value.is_boolean() {
                    *value = Value::Null;
                }
            } else if !value.is_null() && value.as_u64().is_none() {
                *value = Value::Null;
            }
        }
        Ok(v)
    }
}
fn event_id(stats: &Value, fallback: &str) -> String {
    stats["summary_node_id"]
        .as_str()
        .or(stats["marker_node_id"].as_str())
        .unwrap_or(fallback)
        .to_owned()
}
fn insert(events: &mut BTreeMap<String, Value>, id: String, value: Value) {
    if let Some(old) = events.get_mut(&id) {
        for (k, v) in value.as_object().unwrap() {
            if k == "operation_ids" {
                if let Some(dst) = old[k].as_array_mut() {
                    for id in v.as_array().unwrap() {
                        if !dst.contains(id) && dst.len() < 200 {
                            dst.push(id.clone());
                        }
                    }
                }
            } else if k == "metrics" {
                for (key, val) in v.as_object().unwrap() {
                    if old[k][key].is_null() {
                        old[k][key] = val.clone();
                    }
                }
            } else if old[k].is_null() {
                old[k] = v.clone();
            }
        }
    } else {
        events.insert(id, value);
    }
}
pub(crate) struct RawCompactions {
    thread: String,
    limit: usize,
    window: Window,
    cap: usize,
    saturated: bool,
    receipts: Vec<ReceiptRow>,
    automatic: Vec<AutomaticRow>,
    replacements: Vec<ReplacementRow>,
    covered_spills: std::collections::HashSet<String>,
    /// Replacement ID -> first model request started after that adoption (#230).
    post_input: std::collections::HashMap<String, Option<PostInput>>,
    display_latest_complete: Option<bool>,
}
struct ReceiptRow {
    id: String,
    at: Option<String>,
    kind: String,
    raw: String,
    state: Option<String>,
    operation_count: i64,
    in_window: bool,
}
struct AutomaticRow {
    id: String,
    at: Option<String>,
    raw: String,
    failure: Option<String>,
    retry: Option<i64>,
    turn: String,
    outcome: String,
    failures: Option<i64>,
    reason: Option<String>,
    trigger: Option<String>,
}
struct PostInput {
    attempt_id: String,
    input_tokens: Option<u64>,
}
/// The first model request of the Thread that started after the replacement was
/// adopted, read from saved attempts (no stored link). Turns are walked in
/// `ended_at` order so the first match ends the scan; an open Turn is the
/// fallback. Input tokens are the provider-reported value or None, never an
/// estimate.
fn post_adoption_input(
    db: &Connection,
    thread: &str,
    adopted_at: &str,
) -> Result<Option<PostInput>, crate::storage::StorageError> {
    let projection = crate::usage::sql_projection("a.usage_json");
    let ended = format!(
        "SELECT a.id,{projection} FROM turn u INDEXED BY turns_by_thread_end JOIN model_attempt a ON a.turn_id=u.id WHERE u.thread_id=?1 AND u.ended_at IS NOT NULL AND u.ended_at>=?2 AND a.started_at>?2 AND octet_length(a.id)<=512 ORDER BY u.ended_at,a.started_at,a.rowid LIMIT 1"
    );
    let open = format!(
        "SELECT a.id,{projection} FROM turn u JOIN model_attempt a ON a.turn_id=u.id WHERE u.thread_id=?1 AND u.ended_at IS NULL AND u.state IN ('preparing','running','waiting','recovery_wait') AND a.started_at>?2 AND octet_length(a.id)<=512 ORDER BY a.started_at,a.rowid LIMIT 1"
    );
    for sql in [ended, open] {
        let found = db
            .query_row(&sql, params![thread, adopted_at], |row| {
                Ok(PostInput {
                    attempt_id: row.get(0)?,
                    input_tokens: crate::usage::StandardUsage::read(row, 1)?.input_tokens,
                })
            })
            .optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}
fn post_inputs(
    db: &Connection,
    thread: &str,
    replacements: &[ReplacementRow],
    limit: usize,
) -> Result<std::collections::HashMap<String, Option<PostInput>>, crate::storage::StorageError> {
    // None marks a replacement a fork inherited from its parent: its first
    // request was not caused by it, so the item omits the linkage fields.
    let mut found = std::collections::HashMap::new();
    // Replacements are newest first and the page keeps the newest `limit` events,
    // so a summary that survives is among the first `limit` replacement rows.
    for row in replacements.iter().take(limit) {
        if row.inherited {
            found.insert(row.id.clone(), None);
        } else if row.format != "spill-v1"
            && let Some(input) = post_adoption_input(db, thread, &row.at)?
        {
            found.insert(row.id.clone(), Some(input));
        }
    }
    Ok(found)
}
struct ReplacementRow {
    id: String,
    at: String,
    format: String,
    raw: String,
    inherited: bool,
}
pub(crate) fn list(
    db: &Connection,
    thread: &str,
    args: &CompactListArgs,
) -> Result<Value, ApiError> {
    prepare(read_tx(db, thread, args)?)
}
pub(super) struct CopyBudget {
    remaining: Option<usize>,
}
impl CopyBudget {
    pub(super) fn display() -> Self {
        Self {
            remaining: Some(256 * 1024),
        }
    }
    fn text(
        &mut self,
        row: &rusqlite::Row<'_>,
        column: usize,
        metrics: bool,
    ) -> Result<String, crate::storage::StorageError> {
        if let (Some(remaining), rusqlite::types::ValueRef::Text(bytes)) =
            (self.remaining, row.get_ref(column)?)
        {
            if bytes.len() > remaining || (!metrics && bytes.len() > 512) {
                return Err(ApiError::new(
                    ErrorCode::InputLimit,
                    "Compaction display text exceeds its copy budget",
                )
                .into());
            }
            self.remaining = Some(remaining - bytes.len());
        }
        Ok(row.get(column)?)
    }
    fn optional(
        &mut self,
        row: &rusqlite::Row<'_>,
        column: usize,
    ) -> Result<Option<String>, crate::storage::StorageError> {
        if matches!(row.get_ref(column)?, rusqlite::types::ValueRef::Null) {
            Ok(None)
        } else {
            self.text(row, column, false).map(Some)
        }
    }
}
pub(super) fn capture_error(error: crate::storage::StorageError) -> ApiError {
    match error {
        crate::storage::StorageError::Sqlite(sql) => super::error(sql),
        other => other.into_api_error(),
    }
}
pub(crate) fn read_tx(
    db: &Connection,
    thread: &str,
    a: &CompactListArgs,
) -> Result<RawCompactions, ApiError> {
    collect_tx(db, thread, a, &mut CopyBudget { remaining: None }).map_err(capture_error)
}
pub(super) fn collect_tx(
    db: &Connection,
    thread: &str,
    a: &CompactListArgs,
    budget: &mut CopyBudget,
) -> Result<RawCompactions, crate::storage::StorageError> {
    a.validate()?;
    super::register(db)?;
    let window = Window::new(a.since.as_deref(), a.until.as_deref())?;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [thread],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(ApiError::new(ErrorCode::TargetUnavailable, "Thread is unavailable").into());
    }
    let cap = (a.limit as usize * 4 + 1).min(801);
    let mut receipts = Vec::new();
    let mut spills = Vec::new();
    let mut included_receipts = 0usize;
    let mut statement = db.prepare(&receipt_query(&window))?;
    let mut rows = statement.query(params![thread, cap as i64])?;
    while let Some(row) = rows.next()? {
        let id = budget.text(row, 0, false)?;
        let start = budget.text(row, 1, false)?;
        let at = budget.optional(row, 2)?;
        let kind = budget.text(row, 3, false)?;
        let raw = budget.text(row, 4, true)?;
        let state = budget.optional(row, 5)?;
        let operation_count = row.get(6)?;
        let spilled = row.get::<_, i64>(7)?;
        let in_window = window.includes(at.as_deref());
        if in_window {
            included_receipts += 1;
            if spilled > 0 {
                spills.push((id.clone(), start, at.clone(), spilled as u64));
            }
        }
        receipts.push(ReceiptRow {
            id,
            at,
            kind,
            raw,
            state,
            operation_count,
            in_window,
        });
    }
    drop(rows);
    drop(statement);
    let mut covered_spills = std::collections::HashSet::new();
    for (op, start, end, n) in spills {
        let Some(end) = end else {
            continue;
        };
        let mut statement = db.prepare(SPILL_COVERAGE_SQL)?;
        let mut rows = statement.query(params![
            thread,
            start,
            end,
            format!("compact.result.{op}"),
            n.min(cap as u64) as i64
        ])?;
        while let Some(row) = rows.next()? {
            covered_spills.insert(budget.text(row, 0, false)?);
        }
    }
    let mut automatic = Vec::new();
    let mut statement = db.prepare(&automatic_query(&window))?;
    let mut rows = statement.query(params![thread, cap as i64])?;
    while let Some(row) = rows.next()? {
        automatic.push(AutomaticRow {
            id: budget.text(row, 0, false)?,
            at: budget.optional(row, 1)?,
            raw: budget.text(row, 2, true)?,
            failure: budget.optional(row, 3)?,
            retry: row.get(4)?,
            turn: budget.text(row, 5, false)?,
            outcome: budget.text(row, 6, false)?,
            failures: row.get(7)?,
            reason: budget.optional(row, 8)?,
            trigger: budget.optional(row, 9)?,
        });
    }
    drop(rows);
    drop(statement);
    let mut replacements = Vec::new();
    let mut statement = db.prepare(&replacement_query(&window))?;
    let mut rows = statement.query(params![thread, cap as i64])?;
    while let Some(row) = rows.next()? {
        replacements.push(ReplacementRow {
            id: budget.text(row, 0, false)?,
            at: budget.text(row, 1, false)?,
            format: budget.text(row, 2, false)?,
            raw: budget.text(row, 3, true)?,
            inherited: row.get(4)?,
        });
    }
    let post_input = post_inputs(db, thread, &replacements, a.limit as usize)?;
    let saturated = included_receipts == cap || automatic.len() == cap || replacements.len() == cap;
    Ok(RawCompactions {
        thread: thread.into(),
        limit: a.limit as usize,
        window,
        cap,
        saturated,
        receipts,
        automatic,
        replacements,
        covered_spills,
        post_input,
        display_latest_complete: None,
    })
}
pub(crate) fn prepare(raw: RawCompactions) -> Result<Value, ApiError> {
    let RawCompactions {
        thread,
        limit,
        window,
        cap,
        saturated,
        receipts,
        automatic,
        replacements,
        covered_spills,
        post_input,
        display_latest_complete,
    } = raw;
    let mut events = BTreeMap::new();
    for row in receipts {
        let stats = parse(row.raw)?;
        if !row.in_window {
            continue;
        }
        let key = if row.kind == "restore" {
            format!("operation:{}", row.id)
        } else {
            event_id(&stats, &format!("operation:{}", row.id))
        };
        insert(
            &mut events,
            key.clone(),
            json!({"event_id":key,"thread_id":thread,"kind":row.kind,"at":row.at,"timestamp_basis":"replacement_created_at_or_operation_completed_at","operation_ids":[row.id],"operation_count":row.operation_count,"operation_ids_truncated":row.operation_count>1,"state":row.state,"metrics":stats}),
        );
    }
    for row in automatic {
        let mut stats = parse(row.raw)?;
        if !window.includes(row.at.as_deref()) {
            continue;
        }
        let key = event_id(&stats, &format!("attempt:{}", row.id));
        let mut trigger = json!({"failure_kind":row.failure,"retry_number":row.retry,"retained_failures_in_turn_through_attempt":row.failures});
        // Proactive records (#125) add their saved threshold and the provider-
        // reported input tokens; only unsigned integers are copied.
        if let Some(saved) = row
            .trigger
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        {
            for field in ["auto_at_tokens", "input_tokens"] {
                let value = &saved[field];
                if value.as_u64().is_some() {
                    trigger[field] = value.clone();
                }
            }
        }
        let reason = row
            .reason
            .filter(|reason| reason.len() <= 64)
            .unwrap_or_else(|| "failure".into());
        stats["reason"] = json!(reason);
        insert(
            &mut events,
            key.clone(),
            json!({"event_id":key,"thread_id":thread,"kind":"automatic_reduction","reason":reason,"at":row.at,"timestamp_basis":"replacement_created_at_or_attempt_ended_or_started_at","attempt_id":row.id,"turn_id":row.turn,"trigger":trigger,"state":row.outcome,"metrics":stats}),
        );
    }
    for row in replacements {
        if covered_spills.contains(&row.id) {
            continue;
        }
        let stats = parse(row.raw)?;
        if !window.includes(Some(&row.at)) {
            continue;
        }
        let kind = if stats["reason"].is_string() {
            "range_omission"
        } else if row.format == "spill-v1" {
            "spill"
        } else {
            "summary"
        };
        insert(
            &mut events,
            row.id.clone(),
            json!({"event_id":row.id,"thread_id":thread,"kind":kind,"at":row.at,"timestamp_basis":"replacement_created_at","metrics":stats,"record_kind":"retained_replacement"}),
        );
    }
    let mut items = events.into_values().collect::<Vec<_>>();
    // #230: adopted summaries link to the provider-reported input of the next
    // request. Null means not yet known or not reported, never an estimate.
    for item in &mut items {
        if !matches!(
            item["kind"].as_str(),
            Some("summary" | "automatic_reduction" | "compact")
        ) {
            continue;
        }
        let node = item["metrics"]["summary_node_id"]
            .as_str()
            .or(item["event_id"].as_str());
        let found = node.and_then(|id| post_input.get(id));
        if found.is_some_and(Option::is_none) {
            continue;
        }
        let found = found.and_then(Option::as_ref);
        item["post_compaction_attempt_id"] = json!(found.map(|f| f.attempt_id.as_str()));
        item["post_compaction_input_tokens"] = json!(found.and_then(|f| f.input_tokens));
    }
    items.sort_by(|a, b| {
        let ak = crate::usage_totals::start_key(a["at"].as_str());
        let bk = crate::usage_totals::start_key(b["at"].as_str());
        bk.cmp(&ak)
            .then_with(|| b["event_id"].as_str().cmp(&a["event_id"].as_str()))
    });
    let more = items.len() > limit;
    items.truncate(limit);
    let mut value = json!({"items":items,"more":more,"period":window.json(),"coverage":{"source_candidate_limit":cap,"candidate_limit_reached":saturated,"retention":"retained facts only; missing legacy metrics are unknown; expired operation receipts can hide restores and operation associations","failure_count":"retained Turn failures, not an invented consecutive-failure counter"}});
    if let Some(complete) = display_latest_complete {
        value["coverage"]["latest_complete"] = json!(complete);
    }
    Ok(value)
}
