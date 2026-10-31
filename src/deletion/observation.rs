//! Finite UI observations of deletion facts. Exact CLI review and deletion CAS
//! remain in the canonical implementation; this module never grants permission.
//! A visible observer must invalidate on ANY committed Thread Changed, because
//! newly introduced foreign references are not confined to known target IDs.
use crate::{
    protocol::{ApiError, ErrorCode, ThreadDeleteArgs},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const THREAD_LIMIT: usize = 256;
const WITNESS_LIMIT: usize = 1024;
// Deleted physical children consume admission before exclusion, but unlike
// Watch lineage they do NOT bridge traversal into their live grandchildren.
const SCOPE: &str = r"
WITH RECURSIVE root AS (SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL),
walk(id,parent,depth,path,cycle,deleted) AS (
 SELECT c.id,c.parent_thread_id,1,json_array(?1,c.id),c.id=?1,c.deleted_at IS NOT NULL
 FROM root CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=root.id)
 UNION ALL
 SELECT c.id,c.parent_thread_id,w.depth+1,json_insert(w.path,'$[#]',c.id),
   EXISTS(SELECT 1 FROM json_each(w.path) WHERE value=c.id),c.deleted_at IS NOT NULL
 FROM walk w CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=w.id)
 WHERE w.depth<8 AND NOT w.cycle AND NOT w.deleted
 UNION ALL
 SELECT c.id,c.parent_thread_id,w.depth,json_insert(json_remove(w.path,'$[#-1]'),'$[#]',c.id),
   EXISTS(SELECT 1 FROM json_each(json_remove(w.path,'$[#-1]')) WHERE value=c.id),c.deleted_at IS NOT NULL
 FROM walk w CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=w.parent AND id>w.id)
 ORDER BY 3,1 LIMIT 257
)
SELECT w.id,w.deleted,w.cycle,t.updated_at,
 w.depth=8 AND NOT w.deleted AND NOT w.cycle AND EXISTS(SELECT 1 FROM thread child WHERE child.parent_thread_id=w.id)
FROM walk w JOIN thread t ON t.id=w.id";

// Ordinary finite successor walks over existing indexes. Every physical path,
// inverse path, replacement-source or attachment row consumes the common budget
// before deleted/state/selected filtering. UNION deduplicates repeated inverse
// lanes when several selected paths share one source; at most1025 rows enter
// the result, with a bounded frontier (<=257 initial path heads, <=4 successors
// per admitted row). No raw node/output/attachment body is selected.
const REFERENCES: &str = r"
WITH RECURSIVE selected(id) AS MATERIALIZED (SELECT value FROM json_each(?1)),
walk(kind,a,b,position,node,replacement,completed,ownership,deleted,output) AS (
 SELECT 1,p.thread_id,'',p.ordinal,p.node_id,NULL,0,NULL,0,NULL
 FROM selected s CROSS JOIN thread_path p ON p.thread_id=s.id
  AND p.ordinal=(SELECT ordinal FROM thread_path WHERE thread_id=s.id ORDER BY ordinal LIMIT 1)
 UNION
 SELECT 1,p.thread_id,'',p.ordinal,p.node_id,NULL,0,NULL,0,NULL
 FROM walk w CROSS JOIN thread_path p ON p.thread_id=w.a
  AND p.ordinal=(SELECT ordinal FROM thread_path WHERE thread_id=w.a AND ordinal>w.position ORDER BY ordinal LIMIT 1)
 WHERE w.kind=1
 UNION
 SELECT 2,w.node,p.thread_id,0,w.node,NULL,0,NULL,t.id IS NULL OR t.deleted_at IS NOT NULL,NULL
 FROM walk w CROSS JOIN thread_path p ON p.node_id=w.node
  AND p.thread_id=(SELECT thread_id FROM thread_path INDEXED BY thread_path_node WHERE node_id=w.node ORDER BY thread_id LIMIT 1)
 LEFT JOIN thread t ON t.id=p.thread_id WHERE w.kind=1
 UNION
 SELECT 2,w.a,p.thread_id,0,w.node,NULL,0,NULL,t.id IS NULL OR t.deleted_at IS NOT NULL,NULL
 FROM walk w CROSS JOIN thread_path p ON p.node_id=w.a
  AND p.thread_id=(SELECT thread_id FROM thread_path INDEXED BY thread_path_node WHERE node_id=w.a AND thread_id>w.b ORDER BY thread_id LIMIT 1)
 LEFT JOIN thread t ON t.id=p.thread_id WHERE w.kind=2
 UNION
 SELECT 3,w.node,'',s.rowid,w.node,s.replacement_id,COALESCE(r.state='completed',0),NULL,0,r.output_node_id
 FROM walk w CROSS JOIN replacement_source s ON s.rowid=(SELECT rowid FROM replacement_source INDEXED BY replacement_source_reverse WHERE source_node_id=w.node ORDER BY rowid LIMIT 1)
 LEFT JOIN replacement r ON r.id=s.replacement_id WHERE w.kind=1
 UNION
 SELECT 3,w.a,'',s.rowid,w.node,s.replacement_id,COALESCE(r.state='completed',0),NULL,0,r.output_node_id
 FROM walk w CROSS JOIN replacement_source s ON s.rowid=(SELECT rowid FROM replacement_source INDEXED BY replacement_source_reverse WHERE source_node_id=w.a AND rowid>w.position ORDER BY rowid LIMIT 1)
 LEFT JOIN replacement r ON r.id=s.replacement_id WHERE w.kind=3
 UNION
 SELECT 4,w.node,s.id,s.rowid,w.node,NULL,0,s.ownership,0,NULL
 FROM walk w CROSS JOIN attachment_ref s ON s.rowid=(SELECT rowid FROM attachment_ref INDEXED BY attachment_source WHERE source_node_id=w.node ORDER BY rowid LIMIT 1) WHERE w.kind=1
 UNION
 SELECT 4,w.a,s.id,s.rowid,w.node,NULL,0,s.ownership,0,NULL
 FROM walk w CROSS JOIN attachment_ref s ON s.rowid=(SELECT rowid FROM attachment_ref INDEXED BY attachment_source WHERE source_node_id=w.a AND rowid>w.position ORDER BY rowid LIMIT 1) WHERE w.kind=4
 UNION
 SELECT 5,w.output,p.thread_id,0,w.output,w.replacement,1,NULL,t.id IS NULL OR t.deleted_at IS NOT NULL,NULL
 FROM walk w CROSS JOIN thread_path p ON p.node_id=w.output
  AND p.thread_id=(SELECT thread_id FROM thread_path INDEXED BY thread_path_node WHERE node_id=w.output ORDER BY thread_id LIMIT 1)
 LEFT JOIN thread t ON t.id=p.thread_id WHERE w.kind=3 AND w.completed
 UNION
 SELECT 5,w.a,p.thread_id,0,w.node,w.replacement,1,NULL,t.id IS NULL OR t.deleted_at IS NOT NULL,NULL
 FROM walk w CROSS JOIN thread_path p ON p.node_id=w.a
  AND p.thread_id=(SELECT thread_id FROM thread_path INDEXED BY thread_path_node WHERE node_id=w.a AND thread_id>w.b ORDER BY thread_id LIMIT 1)
 LEFT JOIN thread t ON t.id=p.thread_id WHERE w.kind=5
 LIMIT 1025
)
SELECT kind,b,node,replacement,completed,ownership,deleted FROM walk";
// There is no reverse replacement_id index on thread_replacement. Inspect a
// finite physical rowid prefix rather than doing an unbounded inverse scan.
const REPLACEMENT_THREADS: &str = "SELECT tr.replacement_id,tr.thread_id,t.id IS NULL OR t.deleted_at IS NOT NULL FROM thread_replacement tr LEFT JOIN thread t ON t.id=tr.thread_id ORDER BY tr.rowid LIMIT ?1";
struct Member {
    id: String,
    deleted: bool,
    cycle: bool,
    updated: String,
    depth_limited: bool,
}
struct Witness {
    kind: i64,
    b: String,
    node: String,
    replacement: Option<String>,
    completed: bool,
    ownership: Option<String>,
    deleted: bool,
}
struct Run {
    id: String,
    thread: String,
    state: String,
}
struct ReplacementThread {
    replacement: String,
    thread: String,
    deleted: bool,
}
pub(crate) struct RawObservation {
    args: ThreadDeleteArgs,
    updated: String,
    root_revision: i64,
    members: Vec<Member>,
    scope_truncated: bool,
    runs: Vec<Run>,
    witnesses: Vec<Witness>,
    references_truncated: bool,
    replacement_threads: Vec<ReplacementThread>,
    replacement_threads_complete: bool,
}
fn invalid(message: &str) -> StorageError {
    ApiError::new(ErrorCode::InvalidArgument, message).into()
}
#[derive(Debug, thiserror::Error)]
#[error("Deletion observation scalar exceeds512 bytes; inspect with the exact CLI preview")]
struct ScalarLimit;
fn scalar(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<String> {
    let value = row.get_ref(index)?;
    if let rusqlite::types::ValueRef::Text(bytes) = value
        && bytes.len() > 512
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(ScalarLimit),
        ));
    }
    let text = value.as_str().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(text.to_owned())
}
fn optional_scalar(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<String>> {
    if matches!(row.get_ref(index)?, rusqlite::types::ValueRef::Null) {
        Ok(None)
    } else {
        scalar(row, index).map(Some)
    }
}
fn boundary_error(error: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::FromSqlConversionFailure(_, _, source) = &error
        && source.downcast_ref::<ScalarLimit>().is_some()
    {
        return ApiError::new(ErrorCode::InputLimit, ScalarLimit.to_string()).into();
    }
    error.into()
}
pub(crate) fn read_tx(
    db: &Connection,
    args: &ThreadDeleteArgs,
) -> Result<RawObservation, StorageError> {
    if args.thread_id.is_empty() || args.thread_id.len() > 256 {
        return Err(invalid("Invalid deletion observation Thread ID"));
    }
    let updated: Option<String> = db
        .query_row(
            "SELECT updated_at FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [&args.thread_id],
            |row| scalar(row, 0),
        )
        .optional()
        .map_err(boundary_error)?;
    let updated = updated.ok_or_else(|| {
        StorageError::from(ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"))
    })?;
    let revision: String = db
        .query_row(
            "SELECT value FROM app_meta WHERE key='root_revision'",
            [],
            |row| scalar(row, 0),
        )
        .map_err(boundary_error)?;
    let root_revision = revision.parse().map_err(|_| {
        StorageError::from(crate::storage::save_error("invalid root revision metadata"))
    })?;
    let mut members = db
        .prepare(SCOPE)?
        .query_map([&args.thread_id], |row| {
            Ok(Member {
                id: scalar(row, 0)?,
                deleted: row.get(1)?,
                cycle: row.get(2)?,
                updated: scalar(row, 3)?,
                depth_limited: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(boundary_error)?;
    let scope_truncated =
        members.len() > THREAD_LIMIT || members.iter().any(|row| row.cycle || row.depth_limited);
    members.truncate(THREAD_LIMIT);
    let mut selected = BTreeSet::from([args.thread_id.clone()]);
    if args.descendants {
        selected.extend(
            members
                .iter()
                .filter(|row| !row.deleted && !row.cycle)
                .map(|row| row.id.clone()),
        );
    }
    let encoded = serde_json::to_string(&selected).expect("bounded Thread IDs serialize");
    let runs=db.prepare("WITH selected AS (SELECT value AS id FROM json_each(?1)) SELECT r.id,r.thread_id,r.state FROM selected s CROSS JOIN run r INDEXED BY one_live_run ON r.thread_id=s.id WHERE r.state IN ('live','stopping') ORDER BY r.id")?.query_map([&encoded],|row|Ok(Run{id:scalar(row,0)?,thread:scalar(row,1)?,state:scalar(row,2)?}))?.collect::<rusqlite::Result<Vec<_>>>().map_err(boundary_error)?;
    let mut witnesses = db
        .prepare(REFERENCES)?
        .query_map([&encoded], |row| {
            Ok(Witness {
                kind: row.get(0)?,
                b: scalar(row, 1)?,
                node: scalar(row, 2)?,
                replacement: optional_scalar(row, 3)?,
                completed: row.get(4)?,
                ownership: optional_scalar(row, 5)?,
                deleted: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(boundary_error)?;
    let references_truncated = witnesses.len() > WITNESS_LIMIT;
    witnesses.truncate(WITNESS_LIMIT);
    let sources = witnesses
        .iter()
        .filter(|w| w.kind == 3 && w.completed)
        .map(|w| w.node.as_str())
        .collect::<BTreeSet<_>>();
    let mut by_replacement: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for row in witnesses.iter().filter(|w| w.kind == 3 && w.completed) {
        if let Some(replacement) = row.replacement.as_deref() {
            by_replacement
                .entry(replacement)
                .or_default()
                .push(row.node.as_str());
        }
    }
    let mut surviving = BTreeSet::new();
    for row in witnesses
        .iter()
        .filter(|w| w.kind == 5 && !w.deleted && !selected.contains(&w.b))
    {
        if let Some(nodes) = row
            .replacement
            .as_deref()
            .and_then(|id| by_replacement.get(id))
        {
            surviving.extend(nodes.iter().copied());
        }
    }
    let selected_complete = !args.descendants || !scope_truncated;
    let needs_global = selected_complete && !references_truncated && !sources.is_subset(&surviving);
    let mut replacement_threads = Vec::new();
    let mut replacement_threads_complete = !needs_global;
    if needs_global {
        let remaining = WITNESS_LIMIT - witnesses.len();
        replacement_threads = db
            .prepare(REPLACEMENT_THREADS)?
            .query_map([(remaining + 1) as i64], |row| {
                Ok(ReplacementThread {
                    replacement: scalar(row, 0)?,
                    thread: scalar(row, 1)?,
                    deleted: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(boundary_error)?;
        replacement_threads_complete = replacement_threads.len() <= remaining;
        replacement_threads.truncate(remaining);
    }
    Ok(RawObservation {
        args: args.clone(),
        updated,
        root_revision,
        members,
        scope_truncated,
        runs,
        witnesses,
        references_truncated,
        replacement_threads,
        replacement_threads_complete,
    })
}

/// Counts are exact only with complete relevant evidence. Incomplete negative
/// counts remain null; observed positive lower bounds are separate facts.
pub(crate) fn prepare(raw: RawObservation) -> Value {
    let descendants = raw
        .members
        .iter()
        .filter(|row| !row.deleted && !row.cycle && row.id != raw.args.thread_id)
        .map(|row| row.id.clone())
        .collect::<BTreeSet<_>>();
    let selected_complete = !raw.args.descendants || !raw.scope_truncated;
    let mut selected = BTreeSet::from([raw.args.thread_id.clone()]);
    if raw.args.descendants {
        selected.extend(descendants.iter().cloned());
    }
    let live = raw.runs.iter().filter(|run| run.state == "live").count();
    let stopping = raw
        .runs
        .iter()
        .filter(|run| run.state == "stopping")
        .count();
    let runs = raw
        .runs
        .iter()
        .take(50)
        .map(|run| json!({"id":run.id,"thread_id":run.thread,"state":run.state}))
        .collect::<Vec<_>>();
    let nodes = raw
        .witnesses
        .iter()
        .filter(|w| w.kind == 1)
        .map(|w| w.node.clone())
        .collect::<BTreeSet<_>>();
    let mut shared = BTreeSet::new();
    let mut surviving_threads = BTreeSet::new();
    let mut sources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut surviving_replacements = BTreeSet::new();
    let mut attachments = BTreeMap::new();
    for w in &raw.witnesses {
        match w.kind {
            2 if selected_complete && !w.deleted && !selected.contains(&w.b) => {
                shared.insert(w.node.clone());
                surviving_threads.insert(w.b.clone());
            }
            3 if w.completed => {
                if let Some(id) = &w.replacement {
                    sources
                        .entry(id.clone())
                        .or_default()
                        .insert(w.node.clone());
                }
            }
            4 => {
                attachments.insert(w.b.clone(), w.ownership.as_deref() == Some("external"));
            }
            5 if selected_complete && !w.deleted && !selected.contains(&w.b) => {
                if let Some(id) = &w.replacement {
                    surviving_replacements.insert(id.clone());
                }
            }
            _ => {}
        }
    }
    for row in &raw.replacement_threads {
        if selected_complete && !row.deleted && !selected.contains(&row.thread) {
            surviving_replacements.insert(row.replacement.clone());
        }
    }
    let completed_sources = sources.values().flatten().cloned().collect::<BTreeSet<_>>();
    let surviving_sources = sources
        .iter()
        .filter(|(id, _)| surviving_replacements.contains(*id))
        .flat_map(|(_, nodes)| nodes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let external = attachments.values().filter(|external| **external).count();
    let observed = json!({"selected_path_nodes":nodes.len(),"shared_path_nodes":shared.len(),"surviving_path_thread_count":surviving_threads.len(),"completed_replacement_source_nodes":completed_sources.len(),"surviving_replacement_source_nodes":surviving_sources.len(),"attachment_reference_count":attachments.len(),"external_attachment_reference_count":external,"managed_attachment_reference_count":attachments.len()-external});
    let references_complete =
        selected_complete && !raw.references_truncated && raw.replacement_threads_complete;
    let deletable = if live + stopping > 0 {
        Some(false)
    } else if selected_complete && references_complete {
        Some(true)
    } else {
        None
    };
    let references = if references_complete {
        observed.clone()
    } else {
        Value::Object(
            observed
                .as_object()
                .expect("fixed counts")
                .keys()
                .map(|key| (key.clone(), Value::Null))
                .collect(),
        )
    };
    let latest = if raw.args.descendants {
        raw.members
            .iter()
            .filter(|row| selected.contains(&row.id))
            .map(|row| row.updated.as_str())
            .chain(std::iter::once(raw.updated.as_str()))
            .max()
            .unwrap_or(&raw.updated)
            .to_owned()
    } else {
        raw.updated.clone()
    };
    json!({"thread_id":raw.args.thread_id,"descendants":raw.args.descendants,"updated_at":raw.updated,"root_revision":raw.root_revision,"snapshot_only":true,"deletable_now":deletable,
        "selection":{"thread_count":if selected_complete{Some(selected.len())}else{None},"descendant_thread_count":if raw.scope_truncated{None}else{Some(descendants.len())},"surviving_descendant_thread_count":if raw.args.descendants{Some(0)}else if raw.scope_truncated{None}else{Some(descendants.len())},"latest_updated_at":if selected_complete{Some(latest)}else{None}},
        "blockers":{"live_run_count":if selected_complete{Some(live)}else{None},"stopping_run_count":if selected_complete{Some(stopping)}else{None},"runs":runs,"truncated":raw.runs.len()>50||!selected_complete},
        "reference_scope":"direct_references_not_gc_reachability","references":references,
        "reclaimable_bytes":Value::Null,"reclaimable_reason":"requires_gc_reachability_and_storage_accounting",
        "observation":{"bounded":true,"selected_scope_complete":selected_complete,"descendant_scope_complete":!raw.scope_truncated,"references_complete":references_complete,"descendant_physical_candidates":raw.members.len(),"reference_witnesses":raw.witnesses.len()+raw.replacement_threads.len(),"depth_limit":8,"descendant_limit":THREAD_LIMIT,"witness_limit":WITNESS_LIMIT,"observed_lower_bounds":{"selected_thread_count":selected.len(),"descendant_thread_count":descendants.len(),"live_run_count":live,"stopping_run_count":stopping,"references":observed},"exact_review":"shellboy thread delete-preview --thread THREAD_ID"}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{StatementStatus, params};
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        thread(&db, "root", None, false);
        db
    }
    fn thread(db: &Connection, id: &str, parent: Option<&str>, deleted: bool) {
        db.execute("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at,deleted_at)VALUES(?1,?2,?1,'/','','p','m',?1,?3)",params![id,parent,deleted.then_some("deleted")]).unwrap();
    }
    fn node(db: &Connection, id: &str) {
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')",
            [id],
        )
        .unwrap();
    }
    fn path(db: &Connection, thread: &str, node: &str, ordinal: i64) {
        db.execute(
            "INSERT INTO thread_path VALUES(?1,?2,?3)",
            params![thread, ordinal, node],
        )
        .unwrap();
    }
    fn args(descendants: bool) -> ThreadDeleteArgs {
        ThreadDeleteArgs {
            thread_id: "root".into(),
            descendants,
        }
    }
    fn steps(db: &Connection, sql: &str, argument: &str) -> (i32, i32) {
        let mut statement = db.prepare(sql).unwrap();
        statement
            .query_map([argument], |_| Ok(()))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        (
            statement.get_status(StatementStatus::VmStep),
            statement.get_status(StatementStatus::FullscanStep),
        )
    }
    fn replacement(db: &Connection, id: &str, source: &str, state: &str) {
        let output = format!("output-{id}");
        node(db, &output);
        db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES(?1,?2,?1,'format','prompt',?3,'now')",params![id,output,state]).unwrap();
        db.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES(?1,0,?2,0)",params![id,source]).unwrap();
    }
    #[test]
    fn complete_small_observation_matches_exact_preview_including_deleted_bridge_scope_and_reference_types()
     {
        let db = fixture();
        thread(&db, "child", Some("root"), false);
        thread(&db, "deleted-bridge", Some("root"), true);
        thread(&db, "grandchild", Some("deleted-bridge"), false);
        thread(&db, "outside", None, false);
        for id in ["a", "b", "hidden"] {
            node(&db, id);
        }
        path(&db, "root", "a", 0);
        path(&db, "child", "a", 0);
        path(&db, "child", "b", 1);
        path(&db, "outside", "a", 0);
        path(&db, "grandchild", "hidden", 0);
        replacement(&db, "ra", "a", "completed");
        replacement(&db, "rb", "b", "completed");
        replacement(&db, "not-completed", "a", "pending");
        path(&db, "outside", "output-ra", 1);
        db.execute("INSERT INTO thread_replacement VALUES('outside','rb')", [])
            .unwrap();
        for (id, ownership) in [("external", "external"), ("managed", "managed_clipboard")] {
            db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES(?1,'a','/synthetic','file',?2,'now')",params![id,ownership]).unwrap();
        }
        for descendants in [false, true] {
            let canonical = crate::deletion::preview(&db, &args(descendants)).unwrap();
            let trace = crate::frontend::sql_trace::Statements::new(&db);
            let raw = read_tx(&db, &args(descendants)).unwrap();
            assert!(trace.count() <= 6);
            drop(trace);
            let mut value = prepare(raw);
            assert_eq!(value["observation"]["references_complete"], true);
            value.as_object_mut().unwrap().remove("observation");
            assert_eq!(value, canonical);
            assert_eq!(
                value["selection"]["descendant_thread_count"], 1,
                "deleted bridge must not reveal its live grandchild"
            );
        }
    }
    #[test]
    fn physical_deleted_fanout_is_bounded_before_exclusion_and_negative_evidence_stays_unknown() {
        let db = fixture();
        for i in 0..1500 {
            thread(&db, &format!("excluded-{i:05}"), Some("root"), true);
        }
        let before = steps(&db, SCOPE, "root");
        let value = prepare(read_tx(&db, &args(true)).unwrap());
        assert_eq!(value["observation"]["descendant_physical_candidates"], 256);
        assert_eq!(value["observation"]["selected_scope_complete"], false);
        assert!(value["selection"]["descendant_thread_count"].is_null());
        assert!(value["deletable_now"].is_null());
        assert!(value["blockers"]["live_run_count"].is_null());
        for i in 1500..6000 {
            thread(&db, &format!("excluded-{i:05}"), Some("root"), true);
        }
        assert_eq!(
            steps(&db, SCOPE, "root"),
            before,
            "adding excluded siblings cannot increase traversal VM work"
        );
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('blocked','root','live','now')",
            [],
        )
        .unwrap();
        let value = prepare(read_tx(&db, &args(true)).unwrap());
        assert_eq!(value["deletable_now"], false);
        assert_eq!(
            value["observation"]["observed_lower_bounds"]["live_run_count"],
            1
        );
        let noncascade = prepare(read_tx(&db, &args(false)).unwrap());
        assert_eq!(noncascade["selection"]["thread_count"], 1);
        assert_eq!(noncascade["blockers"]["live_run_count"], 1);
    }
    #[test]
    fn deleted_foreign_path_memberships_consume_witness_budget_before_survivor_filtering() {
        let db = fixture();
        node(&db, "source");
        path(&db, "root", "source", 0);
        for i in 0..2000 {
            let id = format!("excluded-{i:05}");
            thread(&db, &id, None, true);
            path(&db, &id, "source", 0);
        }
        let encoded = r#"["root"]"#;
        let before = steps(&db, REFERENCES, encoded);
        let value = prepare(read_tx(&db, &args(false)).unwrap());
        assert_eq!(value["observation"]["reference_witnesses"], 1024);
        assert_eq!(value["observation"]["references_complete"], false);
        assert!(value["references"]["shared_path_nodes"].is_null());
        assert!(value["deletable_now"].is_null());
        for i in 2000..6000 {
            let id = format!("excluded-{i:05}");
            thread(&db, &id, None, true);
            path(&db, &id, "source", 0);
        }
        assert_eq!(
            steps(&db, REFERENCES, encoded),
            before,
            "excluded inverse memberships must not be skipped by an unbounded filter"
        );
        thread(&db, "zz-survivor", None, false);
        path(&db, "zz-survivor", "source", 0);
        let value = prepare(read_tx(&db, &args(false)).unwrap());
        assert!(
            value["references"]["surviving_path_thread_count"].is_null(),
            "an uninspected live row cannot become a fabricated zero"
        );
    }
    #[test]
    fn noncompleted_replacements_are_physical_witnesses_and_never_an_unbounded_exclusion_scan() {
        let db = fixture();
        node(&db, "source");
        path(&db, "root", "source", 0);
        for i in 0..1500 {
            replacement(&db, &format!("pending-{i}"), "source", "pending");
        }
        let before = steps(&db, REFERENCES, r#"["root"]"#);
        let value = prepare(read_tx(&db, &args(false)).unwrap());
        assert!(value["references"]["completed_replacement_source_nodes"].is_null());
        assert_eq!(value["observation"]["reference_witnesses"], 1024);
        for i in 1500..3000 {
            replacement(&db, &format!("pending-{i}"), "source", "pending");
        }
        assert_eq!(steps(&db, REFERENCES, r#"["root"]"#), before);
    }
    #[test]
    fn unindexed_reverse_replacement_membership_uses_remaining_physical_prefix_and_explicit_unknown()
     {
        let db = fixture();
        node(&db, "source");
        path(&db, "root", "source", 0);
        replacement(&db, "needed", "source", "completed");
        replacement(&db, "unrelated", "source", "pending");
        for i in 0..1500 {
            let id = format!("foreign-{i}");
            thread(&db, &id, None, true);
            db.execute(
                "INSERT INTO thread_replacement VALUES(?1,'unrelated')",
                [id],
            )
            .unwrap();
        }
        let raw = read_tx(&db, &args(false)).unwrap();
        assert!(raw.witnesses.len() + raw.replacement_threads.len() <= 1024);
        assert!(!raw.replacement_threads_complete);
        let value = prepare(raw);
        assert!(value["references"]["surviving_replacement_source_nodes"].is_null());
        assert_eq!(value["observation"]["references_complete"], false);
        // The one missing inverse index does not justify a full-table scan.
        let mut statement = db.prepare(REPLACEMENT_THREADS).unwrap();
        let _ = statement
            .query_map([1020], |_| Ok(()))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let before = (
            statement.get_status(StatementStatus::VmStep),
            statement.get_status(StatementStatus::FullscanStep),
        );
        drop(statement);
        for i in 1500..3000 {
            let id = format!("foreign-{i}");
            thread(&db, &id, None, true);
            db.execute(
                "INSERT INTO thread_replacement VALUES(?1,'unrelated')",
                [id],
            )
            .unwrap();
        }
        let mut statement = db.prepare(REPLACEMENT_THREADS).unwrap();
        let _ = statement
            .query_map([1020], |_| Ok(()))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            (
                statement.get_status(StatementStatus::VmStep),
                statement.get_status(StatementStatus::FullscanStep)
            ),
            before
        );
    }
    #[test]
    fn depth_cycles_empty_scope_and_raw_after_close_are_explicit() {
        let db = fixture();
        let mut parent = "root".to_owned();
        for i in 0..10 {
            let id = format!("depth-{i}");
            thread(&db, &id, Some(&parent), false);
            parent = id;
        }
        let value = prepare(read_tx(&db, &args(true)).unwrap());
        assert_eq!(value["observation"]["selected_scope_complete"], false);
        assert_eq!(
            value["observation"]["observed_lower_bounds"]["descendant_thread_count"],
            8
        );
        assert!(value["deletable_now"].is_null());
        let db = fixture();
        db.execute(
            "UPDATE thread SET parent_thread_id='root' WHERE id='root'",
            [],
        )
        .unwrap();
        let value = prepare(read_tx(&db, &args(true)).unwrap());
        assert_eq!(value["observation"]["selected_scope_complete"], false);
        assert!(value["deletable_now"].is_null());
        let db = fixture();
        let raw = read_tx(&db, &args(true)).unwrap();
        drop(db);
        let value = prepare(raw);
        assert_eq!(value["deletable_now"], true);
        assert_eq!(value["references"]["selected_path_nodes"], 0);
        assert_eq!(value["selection"]["thread_count"], 1);
    }
    #[test]
    fn oversized_saved_scalar_fails_before_owned_copy_and_never_authorizes_partial_scope() {
        let db = fixture();
        let original = crate::deletion::preview(&db, &args(false)).unwrap();
        db.execute(
            "UPDATE thread SET updated_at=?1 WHERE id='root'",
            ["x".repeat(1024 * 1024)],
        )
        .unwrap();
        let error = read_tx(&db, &args(false)).err().unwrap().into_api_error();
        assert_eq!(error.code, ErrorCode::InputLimit);
        assert!(error.message.contains("512"));
        db.execute("UPDATE thread SET updated_at='root' WHERE id='root'", [])
            .unwrap();
        let mut after = prepare(read_tx(&db, &args(false)).unwrap());
        after.as_object_mut().unwrap().remove("observation");
        assert_eq!(after, original);
    }
}
