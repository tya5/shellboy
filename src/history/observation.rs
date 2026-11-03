//! Body-free current facts for retained history results; no selection inference.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params, types::ValueRef};
use serde_json::{Value, json};
const CHANNEL: usize = 256 * 1024;
const NODE_SQL:&str="SELECT CAST(w.key AS INTEGER),n.rowid IS NOT NULL,n.payload_revision,n.payload_json IS NOT NULL,
 CASE WHEN octet_length(n.kind)<=64 THEN n.kind END,
 CASE WHEN octet_length(n.created_at)<=512 THEN n.created_at END,
 CASE WHEN octet_length(n.body_hash)<=128 THEN n.body_hash END,
 CASE WHEN octet_length(n.deleted_reason)<=512 THEN n.deleted_reason END,
 EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?2 AND p.node_id=n.id),
 COALESCE(octet_length(n.kind)>64,0),COALESCE(octet_length(n.created_at)>512,0),
 COALESCE(octet_length(n.body_hash)>128,0),COALESCE(octet_length(n.deleted_reason)>512,0)
 FROM json_each(?1) w LEFT JOIN node n ON n.id=w.value ORDER BY CAST(w.key AS INTEGER)";
pub(crate) struct Raw {
    selection: Option<Selection>,
    nodes: Vec<Node>,
}
struct Selection {
    id: String,
    facts: Option<(bool, i64)>,
}
struct Node {
    id: String,
    exists: bool,
    revision: Option<i64>,
    body: bool,
    kind: Option<String>,
    created: Option<String>,
    hash: Option<String>,
    reason: Option<String>,
    selected: bool,
    limited: Vec<&'static str>,
}
fn limit() -> StorageError {
    ApiError::new(
        ErrorCode::InputLimit,
        "History source observation exceeds its finite scope",
    )
    .into()
}
fn text(
    row: &rusqlite::Row<'_>,
    column: usize,
    remaining: &mut usize,
    limited: &mut Vec<&'static str>,
    name: &'static str,
) -> Result<Option<String>, StorageError> {
    if let ValueRef::Text(bytes) = row.get_ref(column)? {
        let charge = bytes
            .iter()
            .map(|b| match b {
                b'"' | b'\\' => 2,
                0..=31 => 6,
                _ => 1,
            })
            .sum::<usize>();
        if charge > *remaining {
            limited.push(name);
            return Ok(None);
        }
        *remaining -= charge;
    }
    Ok(row.get(column)?)
}
pub(crate) fn read_tx(
    db: &Connection,
    ids: &[String],
    thread: Option<&str>,
) -> Result<Raw, StorageError> {
    read_bounded_tx(db, ids, thread, CHANNEL)
}
/// Share the caller's aggregate metadata channel with other retained sources.
/// Existing callers retain the exact default budget through read_tx.
pub(crate) fn read_bounded_tx(
    db: &Connection,
    ids: &[String],
    thread: Option<&str>,
    channel: usize,
) -> Result<Raw, StorageError> {
    if channel > CHANNEL {
        return Err(limit());
    }
    if ids.len() > 200
        || ids.iter().any(|id| id.is_empty() || id.len() > 512)
        || thread.is_some_and(|id| id.is_empty() || id.len() > 512)
    {
        return Err(limit());
    }
    let encoded = serde_json::to_string(ids).map_err(|_| limit())?;
    if encoded.len() > 128 * 1024 {
        return Err(limit());
    }
    let overhead = encoded.len() + ids.len() * 512 + 4096 + thread.map_or(0, |id| id.len() * 6);
    let mut remaining = channel.checked_sub(overhead).ok_or_else(limit)?;
    let selection = thread
        .map(|id| {
            let facts = db
                .query_row(
                    "SELECT deleted_at IS NULL,selection_revision FROM thread WHERE id=?1",
                    [id],
                    |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            Ok::<_, StorageError>(Selection {
                id: id.to_owned(),
                facts,
            })
        })
        .transpose()?;
    let mut nodes = Vec::with_capacity(ids.len());
    if !ids.is_empty() {
        let mut statement = db.prepare(NODE_SQL)?;
        let mut rows = statement.query(params![encoded, thread])?;
        while let Some(row) = rows.next()? {
            let index = usize::try_from(row.get::<_, i64>(0)?).map_err(|_| limit())?;
            let id = ids.get(index).ok_or_else(limit)?.clone();
            let mut limited = Vec::new();
            for (column, name) in [
                (9, "kind"),
                (10, "created_at"),
                (11, "body_hash"),
                (12, "tombstone_reason"),
            ] {
                if row.get::<_, bool>(column)? {
                    limited.push(name);
                }
            }
            nodes.push(Node {
                id,
                exists: row.get(1)?,
                revision: row.get(2)?,
                body: row.get(3)?,
                kind: text(row, 4, &mut remaining, &mut limited, "kind")?,
                created: text(row, 5, &mut remaining, &mut limited, "created_at")?,
                hash: text(row, 6, &mut remaining, &mut limited, "body_hash")?,
                reason: text(row, 7, &mut remaining, &mut limited, "tombstone_reason")?,
                selected: row.get(8)?,
                limited,
            });
        }
    }
    Ok(Raw { selection, nodes })
}
pub(crate) fn prepare(raw: Raw) -> Value {
    let live = raw
        .selection
        .as_ref()
        .and_then(|selection| selection.facts.as_ref())
        .is_some_and(|(live, _)| *live);
    let selection=raw.selection.map(|s|json!({"thread_id":s.id,"thread_exists":s.facts.is_some(),"thread_live":s.facts.map(|f|f.0),"selection_revision":s.facts.map(|f|f.1)}));
    let nodes=raw.nodes.into_iter().map(|n|json!({"node_id":n.id,"node_exists":n.exists,"source_revision":n.revision,
  "body_available":n.exists.then_some(n.body),"kind":n.kind,"created_at":n.created,"body_hash":n.hash,
  "tombstone_reason":if n.exists&&!n.body {n.reason}else{None},
  "selected":(live&&n.exists).then_some(n.selected),"limited_fields":n.limited})).collect::<Vec<_>>();
    json!({"selection_scope":selection,"nodes":nodes,"notice":"Current saved metadata only; retained text, cursor and search results are unchanged. An absent row is unavailable, not proof of deletion."})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use rusqlite::StatementStatus;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'),('other','/','','p','m','now');
   INSERT INTO node(id,kind,created_at,payload_json,payload_revision,body_hash)VALUES('n','assistant_response','now','{\"content\":\"retained original\"}',7,'hash');
   INSERT INTO thread_path VALUES('t',0,'n');").unwrap();
        db
    }
    #[test]
    fn unscoped_selection_missing_rows_and_retained_original_are_distinct_from_current_gc_facts() {
        let db = fixture();
        let ids = vec!["n".into(), "missing".into()];
        let original: String = db
            .query_row("SELECT payload_json FROM node WHERE id='n'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let first = prepare(read_tx(&db, &ids, None).unwrap());
        assert!(first["selection_scope"].is_null());
        assert!(first["nodes"][0]["selected"].is_null());
        assert_eq!(first["nodes"][0]["body_available"], true);
        assert_eq!(first["nodes"][1]["node_exists"], false);
        assert!(first["nodes"][1]["body_available"].is_null());
        let scoped = prepare(read_tx(&db, &ids, Some("t")).unwrap());
        assert_eq!(scoped["nodes"][0]["selected"], true);
        db.execute_batch("DELETE FROM thread_path WHERE thread_id='t';UPDATE thread SET selection_revision=1 WHERE id='t';UPDATE node SET payload_json=NULL,payload_revision=8,deleted_reason='gc' WHERE id='n';").unwrap();
        let raw = read_tx(&db, &ids, Some("t")).unwrap();
        drop(db);
        let changed = prepare(raw);
        assert_eq!(changed["selection_scope"]["selection_revision"], 1);
        assert_eq!(changed["nodes"][0]["selected"], false);
        assert_eq!(changed["nodes"][0]["body_available"], false);
        assert_eq!(changed["nodes"][0]["source_revision"], 8);
        assert_eq!(changed["nodes"][0]["tombstone_reason"], "gc");
        assert_eq!(original, r#"{"content":"retained original"}"#);
    }
    #[test]
    fn invalid_or_escaped_oversized_scope_does_no_sql_and_empty_scope_does_not_scan() {
        let db = fixture();
        let trace = Statements::new(&db);
        for ids in [
            vec!["n".into(); 201],
            vec!["".into()],
            vec!["x".repeat(513)],
            vec!["\0".repeat(512); 200],
        ] {
            assert!(
                matches!(read_tx(&db,&ids,None),Err(StorageError::Operation(e))if e.code==ErrorCode::InputLimit)
            );
        }
        assert!(read_tx(&db, &[], Some("")).is_err());
        assert_eq!(trace.count(), 0);
        assert!(
            prepare(read_tx(&db, &[], None).unwrap())["nodes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(trace.count(), 0);
    }
    #[test]
    fn scalar_and_aggregate_guards_keep_metadata_finite_without_hiding_availability() {
        let mut db = fixture();
        let huge = "x".repeat(1024 * 1024);
        db.execute(
            "UPDATE node SET kind=?1,created_at=?1,body_hash=?1,deleted_reason=?1 WHERE id='n'",
            [&huge],
        )
        .unwrap();
        let value = prepare(read_tx(&db, &["n".into()], Some("t")).unwrap());
        assert_eq!(value["nodes"][0]["body_available"], true);
        assert_eq!(
            value["nodes"][0]["limited_fields"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let tx = db.transaction().unwrap();
        let mut ids = Vec::new();
        for i in 0..200 {
            let id = format!("{i:04}{}", "i".repeat(508));
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json,body_hash,deleted_reason)VALUES(?1,?2,?3,NULL,?4,?5)",params![id,"k".repeat(64),"\u{1}".repeat(512),"h".repeat(128),"\u{1}".repeat(512)]).unwrap();
            ids.push(id);
        }
        tx.commit().unwrap();
        let trace = Statements::new(&db);
        let raw = read_tx(&db, &ids, Some("t")).unwrap();
        assert_eq!(trace.count(), 2);
        drop(trace);
        let value = prepare(raw);
        assert!(serde_json::to_vec(&value).unwrap().len() <= CHANNEL);
        assert!(
            value["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|n| n["body_available"] == false)
        );
        assert!(
            value["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| !n["limited_fields"].as_array().unwrap().is_empty())
        );
    }
    #[test]
    fn hot_history_reads_do_not_scan_unrelated_growing_nodes() {
        let mut db = fixture();
        db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO node(id,kind,created_at,payload_json) SELECT 'foreign-'||x,'user','now','{}' FROM n; WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) SELECT 'r-'||x,'foreign-'||x,'key','v','v','completed','now' FROM n; WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision) SELECT 'r-'||x,0,'foreign-'||x,0 FROM n; WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<5000) INSERT INTO edge(src,dst) SELECT 'foreign-'||x,'foreign-'||(x+5000) FROM n;").unwrap();
        let list: crate::protocol::HistoryListArgs =
            serde_json::from_value(json!({"thread_id":"t"})).unwrap();
        let get: crate::protocol::HistoryGetArgs =
            serde_json::from_value(json!({"node_id":"n"})).unwrap();
        for name in ["list", "get", "previous", "next", "sources", "replacements"] {
            let mut observation =
                crate::storage::statement_observation::StatementObservation::install(&mut db);
            match name {
                "list" => {
                    super::super::list_nodes(observation.connection(), &list).unwrap();
                }
                "get" => {
                    super::super::get_node(observation.connection(), &get).unwrap();
                }
                relation => {
                    let args: crate::protocol::HistoryRelationsArgs =
                        serde_json::from_value(json!({"node_id":"n","relation":relation})).unwrap();
                    super::super::relations(observation.connection(), &args).unwrap();
                }
            }
            let counters = observation.counters();
            assert_eq!(counters.fullscan_steps, 0, "{name}: {counters:?}");
            assert!(counters.vm_steps > 0, "{name}");
        }
    }

    #[test]
    fn node_and_path_seeks_do_not_scale_with_unrelated_rows_or_materialize_payloads() {
        let mut db = fixture();
        let encoded = serde_json::to_string(&vec!["n", "missing"]).unwrap();
        let measure = |db: &Connection| {
            let mut statement = db.prepare(NODE_SQL).unwrap();
            let rows = statement
                .query_map(params![encoded, "t"], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows, vec![0, 1]);
            (
                statement.get_status(StatementStatus::VmStep),
                statement.get_status(StatementStatus::FullscanStep),
            )
        };
        let before = measure(&db);
        let tx = db.transaction().unwrap();
        for i in 0..10000 {
            let id = format!("foreign-{i}");
            tx.execute(
                "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')",
                [&id],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO thread_path VALUES('other',?1,?2)",
                params![i, id],
            )
            .unwrap();
        }
        tx.execute(
            "UPDATE node SET payload_json=?1 WHERE id='n'",
            [json!({"content":"x".repeat(1024*1024)}).to_string()],
        )
        .unwrap();
        tx.commit().unwrap();
        {
            let mut observation =
                crate::storage::statement_observation::StatementObservation::install(&mut db);
            assert!(observation.available());
            let _ = read_tx(observation.connection(), &["n".into()], Some("t")).unwrap();
            let counters = observation.counters();
            assert_eq!(
                counters.fullscan_steps, 0,
                "single-node history lookup must seek growing tables"
            );
            assert!(counters.vm_steps > 0);
        }
        assert_eq!(measure(&db), before);
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {NODE_SQL}"))
            .unwrap()
            .query_map(params![encoded, "t"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter().any(|p| p.contains("SEARCH n USING INDEX")),
            "{plan:?}"
        );
        assert!(
            plan.iter()
                .any(|p| p.contains("SEARCH p USING COVERING INDEX")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|p| p == "SCAN n" || p == "SCAN p"),
            "{plan:?}"
        );
    }
}
