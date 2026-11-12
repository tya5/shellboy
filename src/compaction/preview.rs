//! One bounded read-only snapshot; canonical policy without adoption or model calls.
use super::*;
use crate::protocol::{CompactPreviewArgs, ContextEditAction};
use rusqlite::types::ValueRef;
use std::collections::BTreeMap;

const ROWS: usize = 4096;
const TURNS: usize = 512;
const DEPTH: usize = 16;
const TEXT: usize = 8 * 1024 * 1024;
const PAYLOAD: usize = 1024 * 1024;
const ID: usize = 1024;
const SQL_MAX: usize = 28;

fn limit() -> StorageError {
    ApiError::new(
        ErrorCode::InputLimit,
        "Compaction forecast exceeds bounded read coverage; no prediction is available",
    )
    .hint("Narrow or inspect the source range; a coverage limit does not mean compaction will fail")
    .details(json!({"coverage_complete":false}))
    .into()
}
fn text(
    row: &rusqlite::Row<'_>,
    col: usize,
    max: usize,
    budget: &mut usize,
) -> Result<Option<String>, StorageError> {
    if let ValueRef::Text(bytes) = row.get_ref(col)? {
        if bytes.len() > max || bytes.len() > *budget {
            return Err(limit());
        }
        *budget -= bytes.len();
    }
    Ok(row.get(col)?)
}
fn required(
    row: &rusqlite::Row<'_>,
    col: usize,
    max: usize,
    budget: &mut usize,
) -> Result<String, StorageError> {
    text(row, col, max, budget)?
        .ok_or_else(|| StorageError::Schema("Missing compaction forecast scalar".into()))
}
fn json_ids(ids: impl IntoIterator<Item = String>) -> String {
    serde_json::to_string(&ids.into_iter().collect::<Vec<_>>()).expect("IDs")
}

const PATH_SQL: &str =
    "SELECT n.id,n.turn_id,n.payload_revision,n.payload_json,n.deleted_reason,n.kind
 FROM (SELECT node_id,ordinal FROM thread_path WHERE thread_id=?1 ORDER BY ordinal LIMIT 4097) p
 JOIN node n ON n.id=p.node_id ORDER BY p.ordinal";
const HIDDEN_SQL: &str = "SELECT n.id,n.turn_id,n.payload_revision,n.payload_json,NULL,n.kind,s.source_revision,s.source_hash
 FROM replacement r JOIN replacement_source s ON s.replacement_id=r.id JOIN node n ON n.id=s.source_node_id
 WHERE r.output_node_id=?1 AND r.state='completed' ORDER BY s.ordinal LIMIT 4097";
const GROUP_SQL: &str = "SELECT g.id,g.state,c.state,r.node_id FROM json_each(?1) q
 LEFT JOIN tool_group g ON g.id=q.value LEFT JOIN tool_call c ON c.group_id=g.id
 LEFT JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id LIMIT 4097";
const ANCESTRY_SQL: &str = "SELECT json_extract(q.value,'$.root'),n.id,n.turn_id,r.state,n.kind
 FROM json_each(?1) q JOIN replacement r ON r.output_node_id=json_extract(q.value,'$.id')
 JOIN replacement_source s ON s.replacement_id=r.id JOIN node n ON n.id=s.source_node_id LIMIT ?2";

struct Loaded {
    snapshot: Snapshot,
    hidden: Option<(usize, Vec<Group>)>,
    kinds: HashMap<String, String>,
    stopping: bool,
    /// The Thread's own overrides; the connection and `[compaction]` fill in the rest.
    thread_summary_chars: Option<u64>,
}
pub(crate) struct RawPreview {
    plan: Plan,
    kinds: HashMap<String, String>,
    sql: usize,
    at: String,
}
struct Summary {
    source_characters: u64,
    limit: u64,
}

struct Plan {
    original: Snapshot,
    projected: Snapshot,
    targets: HashSet<String>,
    summary: Option<Summary>,
    failure: Option<ApiError>,
    after: Option<u64>,
    mode: &'static str,
    spilled: usize,
    source_nodes: usize,
    removed: (usize, usize),
    intermediate_after: u64,
}

fn read_sources(
    db: &Connection,
    sql: &str,
    parameter: &str,
    budget: &mut usize,
    hidden: bool,
    kinds: &mut HashMap<String, String>,
) -> Result<Vec<Source>, StorageError> {
    let lengths = sql.replace("n.payload_json", "octet_length(n.payload_json)");
    let mut statement = db.prepare(&lengths)?;
    let mut rows = statement.query([parameter])?;
    let mut count = 0;
    let mut total = 0usize;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > ROWS {
            return Err(limit());
        }
        let bytes = row.get::<_, Option<i64>>(3)?.unwrap_or(0);
        let bytes = usize::try_from(bytes).map_err(|_| limit())?;
        total = total.checked_add(bytes).ok_or_else(limit)?;
        if bytes > PAYLOAD || total > *budget {
            return Err(limit());
        }
    }
    drop(rows);
    drop(statement);
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query([parameter])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        if result.len() >= ROWS {
            return Err(limit());
        }
        let id = required(row, 0, ID, budget)?;
        let turn = text(row, 1, ID, budget)?;
        let revision = row.get(2)?;
        let body = text(row, 3, PAYLOAD, budget)?;
        let deleted = text(row, 4, 512, budget)?;
        let kind = required(row, 5, 64, budget)?;
        let payload: Value = if let Some(body) = body {
            serde_json::from_str(&body).map_err(|e| StorageError::Schema(e.to_string()))?
        } else if hidden {
            return Err(
                invalid("A context marker source is unavailable; selected path retained").into(),
            );
        } else {
            json!({"role":"developer","content":[{"type":"input_text","text":format!("Original node {id} is unavailable: {}",deleted.unwrap_or_else(||"missing payload".into()))}]})
        };
        let hash = key(&(revision, &payload));
        if hidden
            && (revision != row.get::<_, i64>(6)?
                || Some(hash.clone()) != text(row, 7, 128, budget)?)
        {
            return Err(invalid("A context marker source changed; selected path retained").into());
        }
        if let Some(group) = payload.get("group_id").and_then(Value::as_str)
            && group.len() > ID
        {
            return Err(limit());
        }
        kinds.insert(id.clone(), kind);
        result.push(Source {
            id,
            turn,
            revision,
            hash,
            payload,
            attachments: Vec::new(),
        });
    }
    Ok(result)
}

fn load(
    db: &Connection,
    thread: &str,
    args: &CompactPreviewArgs,
    budget: &mut usize,
    sql: &mut usize,
) -> Result<Loaded, StorageError> {
    *sql += 1;
    let mut statement=db.prepare("SELECT selection_revision,profile_revision,reasoning_epoch,profile_id,model_id,effort,summary_max_chars FROM thread WHERE id=?1 AND deleted_at IS NULL")?;
    let mut rows = statement.query([thread])?;
    let row = rows.next()?.ok_or_else(|| {
        StorageError::from(ApiError::new(
            ErrorCode::ThreadNotFound,
            "Context target Thread is unavailable",
        ))
    })?;
    let data = (
        row.get::<_, i64>(0)?,
        row.get::<_, i64>(1)?,
        row.get::<_, i64>(2)?,
        row.get::<_, Option<i64>>(6)?,
    );
    let profile = required(row, 3, 4096, budget)?;
    let model = required(row, 4, 4096, budget)?;
    let effort = text(row, 5, 64, budget)?;
    let thread_summary_chars = data
        .3
        .map(|n| u64::try_from(n).map_err(|_| limit()))
        .transpose()?;
    drop(rows);
    drop(statement);
    *sql += 1;
    let mut statement=db.prepare("SELECT id,state,rowid,generation FROM turn INDEXED BY turns_by_thread WHERE thread_id=?1 LIMIT 513")?;
    let mut rows = statement.query([thread])?;
    let mut turns = Vec::new();
    while let Some(row) = rows.next()? {
        if turns.len() >= TURNS {
            return Err(limit());
        }
        turns.push((
            required(row, 0, ID, budget)?,
            required(row, 1, 64, budget)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ));
    }
    let current = turns
        .iter()
        .filter(|t| {
            matches!(
                t.1.as_str(),
                "preparing" | "running" | "waiting" | "recovery_wait"
            )
        })
        .max_by_key(|t| t.2);
    let latest = turns
        .iter()
        .filter(|t| t.1 == "completed")
        .max_by_key(|t| t.2)
        .map(|t| t.0.clone());
    let current_id = current.map(|t| t.0.clone());
    let turn_fence = current.map(|t| (t.0.clone(), t.3));
    *sql += 1;
    let mut statement = db.prepare(
        "SELECT id,state FROM run WHERE thread_id=?1 AND state IN ('live','stopping') LIMIT 2",
    )?;
    let mut rows = statement.query([thread])?;
    let mut run = None;
    let mut stopping = false;
    while let Some(row) = rows.next()? {
        let id = required(row, 0, ID, budget)?;
        let state = required(row, 1, 64, budget)?;
        if state == "stopping" {
            stopping = true
        } else {
            run = Some(id);
        }
    }
    *sql += 2;
    let mut kinds = HashMap::new();
    let mut sources = read_sources(db, PATH_SQL, thread, budget, false, &mut kinds)?;
    let mut hidden = None;
    if args.action == Some(ContextEditAction::Summary) {
        let from = args.from_node_id.as_deref().unwrap_or("");
        let to = args.to_node_id.as_deref().unwrap_or("");
        let marker = sources.iter().enumerate().find(|(_, s)| {
            s.payload.get("context_trim").is_some_and(Value::is_object)
                && ((s.id == from && from == to)
                    || s.payload
                        .get("source_node_ids")
                        .and_then(Value::as_array)
                        .is_some_and(|ids| {
                            ids.first().and_then(Value::as_str) == Some(from)
                                && ids.last().and_then(Value::as_str) == Some(to)
                        }))
        });
        let selected_endpoints =
            sources.iter().any(|s| s.id == from) && sources.iter().any(|s| s.id == to);
        if let Some((index, marker)) = marker
            && (!selected_endpoints || (marker.id == from && from == to))
        {
            *sql += 2;
            let original = read_sources(db, HIDDEN_SQL, &marker.id, budget, true, &mut kinds)?;
            if original.is_empty() {
                return Err(invalid("Context marker source list is unavailable").into());
            }
            if (marker.id != from || from != to)
                && (original.first().is_none_or(|s| s.id != from)
                    || original.last().is_none_or(|s| s.id != to))
            {
                return Err(invalid(
                    "Context marker source boundaries changed; no forecast is available",
                )
                .into());
            }
            if original.len() + sources.len() > ROWS {
                return Err(limit());
            }
            hidden = Some((index, original));
        }
    }
    let ids = json_ids(
        sources
            .iter()
            .chain(hidden.iter().flat_map(|(_, s)| s.iter()))
            .map(|s| s.id.clone()),
    );
    *sql += 1;
    let mut statement=db.prepare("SELECT a.source_node_id,a.id,a.absolute_path,a.input_kind,COALESCE(s.state,'not_selected'),s.reason,a.created_at FROM json_each(?1) q CROSS JOIN attachment_ref a INDEXED BY attachment_source ON a.source_node_id=q.value LEFT JOIN attachment_selection s ON s.attachment_id=a.id AND s.thread_id=?2 LIMIT 4097")?;
    let mut rows = statement.query(params![ids, thread])?;
    let mut attachments: HashMap<String, Vec<(String, String, Value)>> = HashMap::new();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > ROWS {
            return Err(limit());
        }
        let source = required(row, 0, ID, budget)?;
        let value = json!({"id":required(row,1,ID,budget)?,"path":required(row,2,128*1024,budget)?,"kind":required(row,3,64,budget)?,"state":required(row,4,64,budget)?,"reason":text(row,5,512,budget)?});
        let created = required(row, 6, 512, budget)?;
        let id = value["id"].as_str().unwrap().to_owned();
        attachments
            .entry(source)
            .or_default()
            .push((created, id, value));
    }
    for rows in attachments.values_mut() {
        rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    }
    for source in sources
        .iter_mut()
        .chain(hidden.iter_mut().flat_map(|(_, s)| s.iter_mut()))
    {
        source.attachments = attachments
            .get(&source.id)
            .map(|rows| rows.iter().map(|(_, _, v)| v.clone()).collect())
            .unwrap_or_default();
    }
    let groups = sources
        .iter()
        .chain(hidden.iter().flat_map(|(_, s)| s.iter()))
        .filter_map(|s| s.payload.get("group_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    *sql += 1;
    let mut statement = db.prepare(GROUP_SQL)?;
    let mut rows = statement.query([json_ids(groups)])?;
    let mut complete = HashSet::new();
    let mut incomplete = HashSet::new();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > ROWS {
            return Err(limit());
        }
        if let Some(id) = text(row, 0, ID, budget)? {
            let state = text(row, 1, 64, budget)?;
            let call = text(row, 2, 64, budget)?;
            let result = text(row, 3, ID, budget)?;
            complete.insert(id.clone());
            if state.as_deref() != Some("completed")
                || (call.is_some() && (call.as_deref() != Some("completed") || result.is_none()))
            {
                incomplete.insert(id);
            }
        }
    }
    complete.retain(|id| !incomplete.contains(id));
    *sql += 1;
    let mut statement=db.prepare("SELECT message_node_id,state FROM receipt INDEXED BY receipt_thread_order WHERE target_thread_id=?1 LIMIT 4097")?;
    let mut rows = statement.query([thread])?;
    let mut protected = HashSet::new();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > ROWS {
            return Err(limit());
        }
        let id = text(row, 0, ID, budget)?;
        let state = required(row, 1, 64, budget)?;
        if matches!(state.as_str(), "accepted" | "held")
            && let Some(id) = id
        {
            protected.insert(id);
        }
    }
    let mut snapshot = Snapshot {
        thread: thread.into(),
        revision: data.0 as u64,
        profile_revision: data.1,
        epoch: data.2,
        profile,
        model,
        effort,
        groups: source_groups(sources, &complete),
        extra_sources: Vec::new(),
        append_witness: None,
        protected,
        latest_complete_turn: latest,
        current_turn: current_id,
        turn_boundaries: HashMap::new(),
        turn_fence,
        run_fence: run,
        summary_max_chars: data.3,
    };
    // Derive the earliest selected replacement for each protected Turn by going
    // backward through indexed provenance, never scanning lifetime Turn seeds.
    boundaries(db, &mut snapshot, &kinds, budget, sql)?;
    let hidden = hidden
        .map(|(index, original)| {
            let marker_id = ids_for_hidden_marker(&snapshot, args)?;
            let group_index = snapshot
                .groups
                .iter()
                .position(|g| g.sources.len() == 1 && g.sources[0].id == marker_id)
                .ok_or_else(|| {
                    StorageError::from(invalid("Context marker is not a whole selected group"))
                })?;
            let _ = index;
            Ok::<_, StorageError>((group_index, source_groups(original, &complete)))
        })
        .transpose()?;
    Ok(Loaded {
        snapshot,
        hidden,
        kinds,
        stopping,
        thread_summary_chars,
    })
}

fn ids_for_hidden_marker(
    snapshot: &Snapshot,
    args: &CompactPreviewArgs,
) -> Result<String, StorageError> {
    let from = args.from_node_id.as_deref().unwrap_or("");
    let to = args.to_node_id.as_deref().unwrap_or("");
    snapshot
        .groups
        .iter()
        .flat_map(|g| &g.sources)
        .find(|s| {
            s.payload.get("context_trim").is_some_and(Value::is_object)
                && ((s.id == from && from == to)
                    || s.payload
                        .get("source_node_ids")
                        .and_then(Value::as_array)
                        .is_some_and(|ids| {
                            ids.first().and_then(Value::as_str) == Some(from)
                                && ids.last().and_then(Value::as_str) == Some(to)
                        }))
        })
        .map(|s| s.id.clone())
        .ok_or_else(|| invalid("Context marker is unavailable").into())
}

fn boundaries(
    db: &Connection,
    snapshot: &mut Snapshot,
    kinds: &HashMap<String, String>,
    budget: &mut usize,
    sql: &mut usize,
) -> Result<(), StorageError> {
    let turns = [
        snapshot.current_turn.clone(),
        snapshot.latest_complete_turn.clone(),
    ];
    if turns.iter().all(Option::is_none) {
        return Ok(());
    }
    let sources = snapshot
        .groups
        .iter()
        .flat_map(|g| &g.sources)
        .collect::<Vec<_>>();
    let mut first: HashMap<String, (usize, String)> = HashMap::new();
    for (index, s) in sources.iter().enumerate() {
        if let Some(turn) = &s.turn
            && turns.iter().any(|t| t.as_ref() == Some(turn))
        {
            first.entry(turn.clone()).or_insert((index, s.id.clone()));
        }
    }
    let mut frontier = sources
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            kinds
                .get(&s.id)
                .is_some_and(|k| matches!(k.as_str(), "spill" | "summary"))
        })
        .map(|(i, s)| (i, s.id.clone()))
        .collect::<Vec<_>>();
    let mut seen = frontier.iter().cloned().collect::<HashSet<_>>();
    let mut work = 0;
    for _ in 0..DEPTH {
        if frontier.is_empty() {
            break;
        }
        *sql += 1;
        let parameter = serde_json::to_string(
            &frontier
                .iter()
                .map(|(root, id)| json!({"root":root,"id":id}))
                .collect::<Vec<_>>(),
        )
        .expect("frontier");
        let mut statement = db.prepare(ANCESTRY_SQL)?;
        let mut rows = statement.query(params![parameter, (ROWS - work + 1) as i64])?;
        let mut next = Vec::new();
        while let Some(row) = rows.next()? {
            work += 1;
            if work > ROWS {
                return Err(limit());
            }
            let root = row.get::<_, u32>(0)? as usize;
            let source = sources.get(root).ok_or_else(limit)?;
            let id = required(row, 1, ID, budget)?;
            let turn = text(row, 2, ID, budget)?;
            let state = required(row, 3, 64, budget)?;
            let kind = required(row, 4, 64, budget)?;
            if state != "completed" {
                continue;
            }
            if let Some(turn) = turn
                && turns.iter().any(|t| t.as_ref() == Some(&turn))
                && first.get(&turn).is_none_or(|(old, _)| root < *old)
            {
                first.insert(turn, (root, source.id.clone()));
            }
            if matches!(kind.as_str(), "spill" | "summary") && seen.insert((root, id.clone())) {
                next.push((root, id));
            }
        }
        frontier = next;
    }
    if !frontier.is_empty() {
        return Err(limit());
    }
    snapshot.turn_boundaries = first
        .into_iter()
        .map(|(turn, (_, id))| (turn, id))
        .collect();
    Ok(())
}

fn message_counts(snapshot: &Snapshot) -> (usize, usize) {
    let mut counts = (0, 0);
    for source in snapshot.groups.iter().flat_map(|g| &g.sources) {
        for item in crate::control::replay::convert(source.payload.clone(), snapshot.epoch) {
            match item.get("role").and_then(Value::as_str) {
                Some("user") => counts.0 += 1,
                Some("assistant") => counts.1 += 1,
                _ => {}
            }
        }
    }
    counts
}
type SpillProjection = (Snapshot, HashMap<String, Vec<String>>, usize);
fn spill(snapshot: &Snapshot, ranges: Vec<Range<usize>>) -> Result<SpillProjection, ApiError> {
    range::validate(snapshot, &ranges, range::RangeMode::Spill)?;
    let mut projected = snapshot.clone();
    let mut original = HashMap::new();
    let mut count = 0;
    for index in ranges.into_iter().flatten() {
        let group = &snapshot.groups[index];
        if !group.tool
            || group
                .sources
                .iter()
                .all(|s| s.payload.get("spill_version").is_some())
        {
            continue;
        }
        if let Some(payload) = spill_payload(group) {
            let virtual_id = format!("00000000-0000-0000-0000-{index:012}");
            let ids = group
                .sources
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>();
            for boundary in projected.turn_boundaries.values_mut() {
                if ids.contains(boundary) {
                    *boundary = virtual_id.clone();
                }
            }
            original.insert(virtual_id.clone(), ids);
            count += 1;
            let source = Source {
                id: virtual_id,
                turn: None,
                revision: 0,
                hash: key(&(0i64, &payload)),
                payload,
                attachments: Vec::new(),
            };
            projected.groups[index] = Group {
                sources: vec![source],
                complete: true,
                tool: false,
            };
        }
    }
    Ok((projected, original, count))
}
fn make_plan(loaded: &Loaded, args: &CompactPreviewArgs, config: &Config) -> Plan {
    let snapshot = &loaded.snapshot;
    let mode = match args.action {
        None => "default",
        Some(ContextEditAction::Spill) => "spill",
        Some(ContextEditAction::Summary) => "summary",
        Some(ContextEditAction::Trim) => "trim",
        Some(ContextEditAction::Omit) => "omit",
        Some(ContextEditAction::Maximal) => "maximal",
        _ => "unsupported",
    };
    let mut plan = Plan {
        original: snapshot.clone(),
        projected: snapshot.clone(),
        targets: HashSet::new(),
        summary: None,
        failure: None,
        after: Some(edit::history_characters(snapshot)),
        mode,
        spilled: 0,
        source_nodes: 0,
        removed: (0, 0),
        intermediate_after: edit::history_characters(snapshot),
    };
    let outcome = (|| -> Result<(), ApiError> {
        if loaded.stopping {
            return Err(invalid("Compaction cancelled: Run is stopping"));
        }
        if matches!(
            args.action,
            Some(ContextEditAction::Trim | ContextEditAction::Omit | ContextEditAction::Maximal)
        ) {
            let (projected, ids) = edit::preview_trim(snapshot, args, config)?;
            plan.projected = projected;
            plan.source_nodes = ids.len();
            plan.targets.extend(ids);
            plan.after = Some(edit::history_characters(&plan.projected));
            return Ok(());
        }
        let mut virtual_sources = HashMap::new();
        if args.action.is_none() || args.action == Some(ContextEditAction::Spill) {
            let ranges = if args.action.is_none() {
                policy::spill_ranges(snapshot, false)
            } else {
                vec![range::from_nodes(
                    snapshot,
                    args.from_node_id.as_deref().unwrap_or(""),
                    args.to_node_id.as_deref().unwrap_or(""),
                )?]
            };
            let (projected, original, count) = spill(snapshot, ranges)?;
            plan.projected = projected;
            plan.spilled = count;
            plan.source_nodes = original.values().map(Vec::len).sum();
            plan.targets.extend(original.values().flatten().cloned());
            virtual_sources = original;
            plan.intermediate_after = edit::history_characters(&plan.projected);
            plan.after = Some(plan.intermediate_after);
            if args.action == Some(ContextEditAction::Spill) {
                return Ok(());
            }
        }
        let selected = if args.action.is_none() {
            policy::summary_range(&plan.projected, false)
        } else if let Some((index, groups)) = &loaded.hidden {
            // Canonical explicit summaries reintroduce exactly one marker's originals.
            let index = *index;
            let marker_range = index..index + 1;
            range::validate(
                &plan.projected,
                std::slice::from_ref(&marker_range),
                range::RangeMode::Summary,
            )?;
            let end = index + groups.len();
            plan.projected
                .groups
                .splice(index..index + 1, groups.clone());
            Some(index..end)
        } else {
            Some(range::from_nodes(
                &plan.projected,
                args.from_node_id.as_deref().unwrap_or(""),
                args.to_node_id.as_deref().unwrap_or(""),
            )?)
        };
        let Some(selected) = selected else {
            return Ok(());
        };
        range::validate(
            &plan.projected,
            std::slice::from_ref(&selected),
            range::RangeMode::Summary,
        )?;
        let groups = &plan.projected.groups[selected.clone()];
        plan.source_nodes += groups.iter().map(|g| g.sources.len()).sum::<usize>();
        for source in groups.iter().flat_map(|g| &g.sources) {
            if let Some(ids) = virtual_sources.get(&source.id) {
                plan.targets.extend(ids.iter().cloned());
            } else {
                plan.targets.insert(source.id.clone());
            }
        }
        let source_characters = groups
            .iter()
            .flat_map(|g| &g.sources)
            .map(|s| edit::projection_characters(&s.payload, snapshot.epoch))
            .sum();
        if !config.connections.contains_key(&snapshot.profile) {
            return Err(ApiError::new(
                ErrorCode::ConfigInvalid,
                "Summary connection is not configured",
            ));
        }
        let connection = config.connections.get(&snapshot.profile);
        let cap = crate::config::SummarySettings::resolve(
            None,
            loaded.thread_summary_chars,
            connection,
            args.max_chars,
            &config.compaction,
        )
        .output_characters;
        plan.summary = Some(Summary {
            source_characters,
            limit: cap,
        });
        plan.after = None;
        // Counts describe original messages removed, not unknown generated text.
        plan.projected.groups.drain(selected);
        Ok(())
    })();
    if let Err(error) = outcome {
        plan.failure = Some(error);
    }
    let before = message_counts(&plan.original);
    let after = message_counts(&plan.projected);
    plan.removed = (
        before.0.saturating_sub(after.0),
        before.1.saturating_sub(after.1),
    );
    plan
}

/// A bounded, payload-free witness of the sources that automatic compaction
/// can currently consider. Source hashes, rather than text, are persisted.
#[derive(Debug, Clone)]
#[cfg(test)]
pub(crate) struct AutomaticCandidates {
    pub revision: u64,
    pub marks: Vec<String>,
    pub mutable_protection: bool,
}

/// A metadata-only proof for an oversized protected current Turn. It never
/// concludes "no target" unless the ordinary summary fence begins at the
/// first selected source and replay proves that no older tool span can shrink.
#[cfg(test)]
pub(crate) fn automatic_structural_no_target_tx(
    db: &Connection,
    thread: &str,
    no_reducing_older_tools: bool,
) -> Result<Option<u64>, StorageError> {
    if !no_reducing_older_tools {
        return Ok(None);
    }
    let revision: i64 = db.query_row(
        "SELECT selection_revision FROM thread WHERE id=?1 AND deleted_at IS NULL",
        [thread],
        |row| row.get(0),
    )?;
    // A direct first-node Turn is a complete proof even when the selected path
    // is too large for ancestry expansion. The connection's VM budget still
    // bounds the latest-completed lookup.
    let first_turn: Option<String> = db.query_row(
        "SELECT n.turn_id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal LIMIT 1",
        [thread], |row| row.get(0),
    ).optional()?.flatten();
    if let Some(first_turn) = first_turn {
        let turn: Option<(String, i64)> = db
            .query_row(
                "SELECT state,rowid FROM turn WHERE id=?1 AND thread_id=?2",
                params![first_turn, thread],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((state, rowid)) = turn {
            if !matches!(state.as_str(), "completed" | "failed" | "stopped") {
                return Ok(Some(revision as u64));
            }
            if state == "completed" {
                let newer: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM turn WHERE thread_id=?1 AND state='completed' AND rowid>?2)",
                    params![thread, rowid], |row| row.get(0),
                )?;
                if !newer {
                    return Ok(Some(revision as u64));
                }
            }
        }
    }
    let mut paths = db.prepare(
        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal LIMIT 4097",
    )?;
    let mut rows = paths.query([thread])?;
    let mut first = None;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > ROWS {
            return Ok(None);
        }
        if first.is_none() {
            first = Some(row.get::<_, String>(0)?);
        }
    }
    let Some(first) = first else {
        return Ok(None);
    };
    let mut turns =
        db.prepare("SELECT id,state FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 513")?;
    let mut rows = turns.query([thread])?;
    let mut current = None;
    let mut latest = None;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > TURNS {
            return Ok(None);
        }
        let id: String = row.get(0)?;
        let state: String = row.get(1)?;
        if state == "completed" && latest.is_none() {
            latest = Some(id.clone());
        }
        if !matches!(state.as_str(), "completed" | "failed" | "stopped") && current.is_none() {
            current = Some(id);
        }
    }
    for turn in [current.as_deref(), latest.as_deref()]
        .into_iter()
        .flatten()
    {
        match super::resolve_turn_boundary(db, thread, turn, ROWS) {
            Ok(Some(boundary)) if boundary == first => return Ok(Some(revision as u64)),
            Ok(_) => {}
            // The bounded ancestry budget cannot prove the boundary.
            Err(StorageError::InvalidInput(_)) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) fn automatic_candidates_tx(
    db: &Connection,
    thread: &str,
) -> Result<AutomaticCandidates, StorageError> {
    if thread.is_empty() || thread.len() > ID {
        return Err(limit());
    }
    let args = CompactPreviewArgs::default();
    let mut budget = TEXT;
    let mut sql = 0;
    let loaded = load(db, thread, &args, &mut budget, &mut sql)?;
    if sql > SQL_MAX {
        return Err(limit());
    }
    let snapshot = loaded.snapshot;
    let mut marks = BTreeMap::<String, u64>::new();
    let mut add = |kind: &str, group: &Group| {
        for source in &group.sources {
            let identity = format!("{kind}:{}", crate::digest::sha256_hex(&source.id));
            let chars = edit::projection_characters(&source.payload, snapshot.epoch);
            marks
                .entry(identity)
                .and_modify(|previous| *previous = (*previous).max(chars))
                .or_insert(chars);
        }
    };
    for range in policy::spill_ranges(&snapshot, false) {
        let group = &snapshot.groups[range.start];
        let before = group
            .sources
            .iter()
            .map(|source| edit::projection_characters(&source.payload, snapshot.epoch))
            .fold(0u64, u64::saturating_add);
        if spill_payload(group)
            .is_some_and(|payload| edit::projection_characters(&payload, snapshot.epoch) < before)
        {
            add("spill", group);
        }
    }
    if let Some(plan) = policy::active_turn_plan(&snapshot) {
        for group in &snapshot.groups[plan.start..plan.end()] {
            add("middle", group);
        }
    }
    if let Some(range) = policy::summary_range(&snapshot, false) {
        for group in &snapshot.groups[range] {
            add("summary", group);
        }
    }
    if marks.len() > ROWS * 2 {
        return Err(limit());
    }
    let marks = marks
        .into_iter()
        .map(|(identity, chars)| format!("{identity}:{chars:016x}"))
        .collect();
    let mutable_protection = snapshot.groups.iter().any(|group| {
        group
            .sources
            .iter()
            .any(|source| snapshot.protected.contains(&source.id))
    });
    Ok(AutomaticCandidates {
        revision: snapshot.revision,
        marks,
        mutable_protection,
    })
}

pub(crate) fn read_tx(
    db: &Connection,
    thread: &str,
    args: &CompactPreviewArgs,
    config: &Config,
) -> Result<RawPreview, StorageError> {
    let config = &config.current();
    args.validate().map_err(StorageError::from)?;
    if thread.is_empty() || thread.len() > ID {
        return Err(limit());
    }
    let mut budget = TEXT;
    let mut sql = 0;
    let loaded = load(db, thread, args, &mut budget, &mut sql)?;
    let plan = make_plan(&loaded, args, config);
    if sql > SQL_MAX {
        return Err(limit());
    }
    Ok(RawPreview {
        plan,
        kinds: loaded.kinds,
        sql,
        at: timestamp(),
    })
}

pub(crate) fn prepare(raw: RawPreview) -> Value {
    let plan = raw.plan;
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for id in &plan.targets {
        *by_kind
            .entry(
                raw.kinds
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
            )
            .or_default() += 1;
    }
    let mut ids = plan.targets.iter().collect::<Vec<_>>();
    ids.sort();
    let summary=plan.summary.as_ref().map(|s|json!({"source_characters":s.source_characters,"summary_character_limit":s.limit,"single_request":true,"generated_characters":null,"provider_requests":0,"model_output_and_provider_errors_unknown":true}));
    json!({"dry_run":true,"thread_id":plan.original.thread,"mode":plan.mode,"coverage_complete":true,"observed_at":raw.at,
        "selection_revision":plan.original.revision,"profile_revision":plan.original.profile_revision,"reasoning_epoch":plan.original.epoch,
        "target_nodes":{"count":plan.targets.len(),"by_kind":by_kind,"sample_node_ids":ids.into_iter().take(16).collect::<Vec<_>>(),"more":plan.targets.len()>16},
        "before_characters":edit::history_characters(&plan.original),"after_characters":plan.after,
        "removed_original_messages":{"user":plan.removed.0,"assistant":plan.removed.1},"spilled_groups":plan.spilled,"source_nodes":plan.source_nodes,
        "no_target":plan.targets.is_empty() && plan.failure.is_none(),"predicted_failure":plan.failure,"summary":summary,
        "sql_statements":raw.sql,"limits":{"nodes":ROWS,"turns":TURNS,"rows":ROWS,"ancestry_layers":DEPTH,"copied_text_bytes":TEXT,"payload_bytes":PAYLOAD,"sql_statements":SQL_MAX},
        "reservation":false,"provider_requests":0,"notice":"Snapshot forecast only; actual execution revalidates. Characters count replay JSON, messages are not wrapped terminal lines. Summary after-size is unknown. Default spill may commit before a later summary failure."})
}

#[cfg(test)]
mod tests;
