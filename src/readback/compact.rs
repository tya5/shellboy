use super::*;
use std::collections::BTreeMap;
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
        "created_parts",
        "reused_parts",
        "summary_characters",
        "summary_limit",
        "source_characters",
        "model_requests",
        "map_requests",
        "reduce_requests",
        "spilled_groups",
        "summary_node_id",
        "no_target",
    ] {
        fields.push(format!("'{field}',COALESCE(json_extract({root},'$.compact.{field}'),json_extract({root},'$.{field}'))"));
    }
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
        "json_set({base},'$.after_current_request_nodes',CASE WHEN json_extract({root},'$.omitted_nodes')=0 THEN 0 ELSE json_extract(n.payload_json,'$.context_trim.after_current_request_nodes') END,'$.summary_characters',COALESCE(json_extract({root},'$.compact.summary_characters'),json_extract({root},'$.summary_characters'),CASE WHEN n.kind='summary' THEN readback_chars(json_extract(n.payload_json,'$.item.content[0].text')) END),'$.summary_parts',COALESCE(json_array_length(n.payload_json,'$.summary_part_keys'),json_extract({root},'$.compact.created_parts')+json_extract({root},'$.compact.reused_parts'),json_extract({root},'$.created_parts')+json_extract({root},'$.reused_parts')))"
    )
}
pub(super) fn receipt_query(w: &Window) -> String {
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
    format!(
        "SELECT min(o.operation_id),o.created_at,{at},{kind},{},json_extract({meta},'$.state'),count(*) FROM operation_receipt o INDEXED BY compact_operation_thread JOIN app_meta m ON m.key='compact.result.'||o.operation_id LEFT JOIN node n ON n.id={node} AND {kind}<>'restore' WHERE CASE WHEN json_valid(o.result_ref_json) THEN json_extract(o.result_ref_json,'$.result.status')='queued' ELSE 0 END AND json_extract(o.result_ref_json,'$.result.thread_id')=?1 AND json_valid(m.value) AND {} GROUP BY {group} ORDER BY usage_saved_start({at}) DESC,min(o.operation_id) DESC LIMIT ?2",
        enriched(&root),
        w.sql(&at)
    )
}
pub(super) fn automatic_query(w: &Window) -> String {
    let root = "COALESCE(json_extract(a.error_json,'$.automatic_trim'),json_extract(a.error_json,'$.automatic_trim_attempt.result'),json_extract(a.error_json,'$.automatic_trim_attempt.summary'),json_extract(a.error_json,'$.automatic_trim_attempt.error.details'),'{}')";
    let node = output_node(root);
    let at = "COALESCE(n.created_at,a.ended_at,a.started_at)";
    format!(
        "SELECT a.id,{at},{},json_extract(a.error_json,'$.error.kind'),json_extract(a.error_json,'$.automatic_retry.number'),a.turn_id,COALESCE(json_extract(a.error_json,'$.automatic_trim_attempt.outcome'),'recorded'),(SELECT count(*) FROM model_attempt earlier INDEXED BY attempts_by_turn WHERE earlier.turn_id=a.turn_id AND earlier.rowid<=a.rowid AND earlier.state IN ('failed','incomplete')) FROM turn u INDEXED BY turns_by_thread JOIN model_attempt a INDEXED BY automatic_compaction_turn ON a.turn_id=u.id LEFT JOIN node n ON n.id={node} WHERE u.thread_id=?1 AND {AUTO} AND {} ORDER BY usage_saved_start({at}) DESC,a.id DESC LIMIT ?2",
        enriched(root),
        w.sql(at)
    )
}
pub(super) fn replacement_query(w: &Window) -> String {
    format!(
        "SELECT r.output_node_id,r.created_at,r.format_version,json_object('source_nodes',(SELECT count(*) FROM replacement_source q WHERE q.replacement_id=r.id),'first_node_id',(SELECT source_node_id FROM replacement_source q WHERE q.replacement_id=r.id ORDER BY ordinal LIMIT 1),'last_node_id',(SELECT source_node_id FROM replacement_source q WHERE q.replacement_id=r.id ORDER BY ordinal DESC LIMIT 1),'before_characters',json_extract(n.payload_json,'$.context_trim.history_characters'),'omitted_nodes',json_extract(n.payload_json,'$.context_trim.omitted_nodes'),'after_current_request_nodes',json_extract(n.payload_json,'$.context_trim.after_current_request_nodes'),'summary_characters',CASE WHEN n.kind='summary' THEN readback_chars(json_extract(n.payload_json,'$.item.content[0].text')) END,'summary_parts',json_array_length(n.payload_json,'$.summary_part_keys'),'reason',json_extract(n.payload_json,'$.context_trim.reason')) FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND {} ORDER BY usage_saved_start(r.created_at) DESC,r.id DESC LIMIT ?2",
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
            } else if key == "no_target" {
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
pub(crate) fn list(db: &Connection, thread: &str, a: &CompactListArgs) -> Result<Value, ApiError> {
    a.validate()?;
    super::register(db)?;
    let w = Window::new(a.since.as_deref(), a.until.as_deref())?;
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
            [thread],
            |r| r.get(0),
        )
        .map_err(error)?;
    if !exists {
        return Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            "Thread is unavailable",
        ));
    }
    let cap = (a.limit as usize * 4 + 1).min(801);
    let mut events = BTreeMap::new();
    let mut saturated = false;
    let mut st = db.prepare(&receipt_query(&w)).map_err(error)?;
    let rows = st
        .query_map(params![thread, cap as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(error)?;
    let mut count = 0;
    let mut spills = Vec::new();
    for row in rows {
        let (id, start, at, kind, raw, state, operation_count) = row.map_err(error)?;
        let stats = parse(raw)?;
        if !w.includes(at.as_deref()) {
            continue;
        }
        count += 1;
        if stats["spilled_groups"].as_u64().is_some_and(|n| n > 0) {
            spills.push((
                id.clone(),
                start,
                at.clone(),
                stats["spilled_groups"].as_u64().unwrap(),
            ));
        }
        let key = if kind == "restore" {
            format!("operation:{id}")
        } else {
            event_id(&stats, &format!("operation:{id}"))
        };
        insert(
            &mut events,
            key.clone(),
            json!({"event_id":key,"thread_id":thread,"kind":kind,"at":at,"timestamp_basis":"replacement_created_at_or_operation_completed_at","operation_ids":[id],"operation_count":operation_count,"operation_ids_truncated":operation_count>1,"state":state,"metrics":stats}),
        );
    }
    saturated |= count == cap;
    let mut covered_spills = std::collections::HashSet::new();
    for (op, start, end, n) in spills {
        let Some(end) = end else {
            continue;
        };
        // Per-thread compaction is serialized. The latest matching spill rows before
        // this result belong to its reported spill count; earlier restored rows do not.
        let sql = SPILL_COVERAGE_SQL;
        let mut q = db.prepare(sql).map_err(error)?;
        for row in q
            .query_map(
                params![
                    thread,
                    start,
                    end,
                    format!("compact.result.{op}"),
                    n.min(cap as u64) as i64
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(error)?
        {
            covered_spills.insert(row.map_err(error)?);
        }
    }
    let mut st = db.prepare(&automatic_query(&w)).map_err(error)?;
    let mut count = 0;
    for row in st
        .query_map(params![thread, cap as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
            ))
        })
        .map_err(error)?
    {
        let (id, at, raw, failure, retry, turn, outcome, failures) = row.map_err(error)?;
        count += 1;
        let stats = parse(raw)?;
        if !w.includes(at.as_deref()) {
            continue;
        }
        let key = event_id(&stats, &format!("attempt:{id}"));
        insert(
            &mut events,
            key.clone(),
            json!({"event_id":key,"thread_id":thread,"kind":"automatic_reduction","at":at,"timestamp_basis":"replacement_created_at_or_attempt_ended_or_started_at","attempt_id":id,"turn_id":turn,"trigger":{"failure_kind":failure,"retry_number":retry,"retained_failures_in_turn_through_attempt":failures},"state":outcome,"metrics":stats}),
        );
    }
    saturated |= count == cap;
    let mut st = db.prepare(&replacement_query(&w)).map_err(error)?;
    let mut count = 0;
    for row in st
        .query_map(params![thread, cap as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(error)?
    {
        let (id, at, format, raw) = row.map_err(error)?;
        count += 1;
        if covered_spills.contains(&id) {
            continue;
        }
        let stats = parse(raw)?;
        if !w.includes(Some(&at)) {
            continue;
        }
        let kind = if stats["reason"].is_string() {
            "range_omission"
        } else if format == "spill-v1" {
            "spill"
        } else {
            "summary"
        };
        insert(
            &mut events,
            id.clone(),
            json!({"event_id":id,"thread_id":thread,"kind":kind,"at":at,"timestamp_basis":"replacement_created_at","metrics":stats,"record_kind":"retained_replacement"}),
        );
    }
    saturated |= count == cap;
    let mut items = events.into_values().collect::<Vec<_>>();
    items.sort_by(|a, b| {
        let ak = crate::usage_totals::start_key(a["at"].as_str());
        let bk = crate::usage_totals::start_key(b["at"].as_str());
        bk.cmp(&ak)
            .then_with(|| b["event_id"].as_str().cmp(&a["event_id"].as_str()))
    });
    let more = items.len() > a.limit as usize;
    items.truncate(a.limit as usize);
    Ok(
        json!({"items":items,"more":more,"period":w.json(),"coverage":{"source_candidate_limit":cap,"candidate_limit_reached":saturated,"retention":"retained facts only; missing legacy metrics are unknown; expired operation receipts can hide restores and operation associations","failure_count":"retained Turn failures, not an invented consecutive-failure counter"}}),
    )
}
