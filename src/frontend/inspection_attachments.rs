//! Finite saved selection observation; canonical CLI listing remains exhaustive.
//! State prefixes use the existing physical index before any joins. At most128
//! rows plus one body-free sentinel cross this seam; no source JSON/file reads.
use crate::storage::StorageError;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
const ROWS: usize = 128;
//512 bytes per path/reason bounds each aggregate to64KiB independently.
const TEXT: i64 = 512;
const PREFIX: &str = "SELECT rowid FROM attachment_selection INDEXED BY active_attachments WHERE thread_id=?1 AND state=?2 ORDER BY selected_order,rowid LIMIT ?3";
const DETAIL: &str = "WITH cohort AS MATERIALIZED (SELECT s.rowid,s.selected_order,s.state,CASE WHEN octet_length(s.reason)<=?4 THEN s.reason END AS reason,COALESCE(octet_length(s.reason)>?4,0) AS reason_limited FROM attachment_selection s INDEXED BY active_attachments WHERE s.thread_id=?1 AND s.state=?2 ORDER BY s.selected_order,s.rowid LIMIT ?3)
 SELECT c.selected_order,
 CASE WHEN octet_length(s.attachment_id)<=256 THEN s.attachment_id END,
 CASE WHEN octet_length(a.source_node_id)<=256 THEN a.source_node_id END,
 CASE WHEN octet_length(a.absolute_path)<=?4 THEN a.absolute_path END,
 CASE WHEN octet_length(a.input_kind)<=32 THEN a.input_kind END,
 a.ownership='managed_clipboard',c.state='active',
 CASE WHEN octet_length(c.reason)<=?4 THEN c.reason END,
 octet_length(a.absolute_path)>?4,c.reason_limited,
 CASE WHEN a.source_node_id IS NULL THEN NULL WHEN p.node_id IS NOT NULL THEN 'applied' ELSE COALESCE(d.status,'accepted') END,
 p.node_id IS NOT NULL,n.id IS NOT NULL,n.payload_json IS NOT NULL,
 CASE WHEN octet_length(n.deleted_reason)<=?4 THEN n.deleted_reason END,
 CASE WHEN octet_length(m.state)<=32 THEN m.state END,
 CASE WHEN octet_length(m.deleted_reason)<=?4 THEN m.deleted_reason END,
 COALESCE(octet_length(n.deleted_reason)>?4,0),COALESCE(octet_length(m.deleted_reason)>?4,0)
 FROM cohort c JOIN attachment_selection s ON s.rowid=c.rowid LEFT JOIN attachment_ref a ON a.id=s.attachment_id
 LEFT JOIN thread_path p ON p.thread_id=?1 AND p.node_id=a.source_node_id
 LEFT JOIN attachment_delivery d ON d.thread_id=?1 AND d.node_id=a.source_node_id
 LEFT JOIN node n ON n.id=a.source_node_id
 LEFT JOIN managed_file m ON m.absolute_path=a.absolute_path";
const POINT_DETAIL: &str = "WITH cohort AS MATERIALIZED (SELECT s.rowid,s.selected_order,s.state,CASE WHEN octet_length(s.reason)<=?3 THEN s.reason END AS reason,COALESCE(octet_length(s.reason)>?3,0) AS reason_limited FROM attachment_selection s WHERE s.thread_id=?1 AND s.attachment_id=?2) SELECT c.selected_order,
 CASE WHEN octet_length(s.attachment_id)<=256 THEN s.attachment_id END,
 CASE WHEN octet_length(a.source_node_id)<=256 THEN a.source_node_id END,
 CASE WHEN octet_length(a.absolute_path)<=?3 THEN a.absolute_path END,
 CASE WHEN octet_length(a.input_kind)<=32 THEN a.input_kind END,
 a.ownership='managed_clipboard',c.state='active',
 CASE WHEN octet_length(c.reason)<=?3 THEN c.reason END,
 octet_length(a.absolute_path)>?3,c.reason_limited,
 CASE WHEN a.source_node_id IS NULL THEN NULL WHEN p.node_id IS NOT NULL THEN 'applied' ELSE COALESCE(d.status,'accepted') END,
 p.node_id IS NOT NULL,n.id IS NOT NULL,n.payload_json IS NOT NULL,
 CASE WHEN octet_length(n.deleted_reason)<=?3 THEN n.deleted_reason END,
 CASE WHEN octet_length(m.state)<=32 THEN m.state END,
 CASE WHEN octet_length(m.deleted_reason)<=?3 THEN m.deleted_reason END,
 COALESCE(octet_length(n.deleted_reason)>?3,0),COALESCE(octet_length(m.deleted_reason)>?3,0)
 FROM cohort c JOIN attachment_selection s ON s.rowid=c.rowid LEFT JOIN attachment_ref a ON a.id=s.attachment_id
 LEFT JOIN thread_path p ON p.thread_id=?1 AND p.node_id=a.source_node_id
 LEFT JOIN attachment_delivery d ON d.thread_id=?1 AND d.node_id=a.source_node_id
 LEFT JOIN node n ON n.id=a.source_node_id
 LEFT JOIN managed_file m ON m.absolute_path=a.absolute_path";
pub(super) struct Raw {
    thread: String,
    attachment: Option<String>,
    target: Option<(Option<String>, i64, bool)>,
    rows: Vec<Row>,
    complete: bool,
}
struct Row {
    order: i64,
    id: Option<String>,
    source: Option<String>,
    path: Option<String>,
    kind: Option<String>,
    managed: Option<bool>,
    selected: bool,
    reason: Option<String>,
    path_limited: Option<bool>,
    reason_limited: bool,
    delivery: Option<String>,
    on_path: bool,
    source_exists: bool,
    source_available: bool,
    source_reason: Option<String>,
    managed_state: Option<String>,
    managed_reason: Option<String>,
    source_reason_limited: bool,
    managed_reason_limited: bool,
}
fn decode_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        order: r.get(0)?,
        id: r.get(1)?,
        source: r.get(2)?,
        path: r.get(3)?,
        kind: r.get(4)?,
        managed: r.get(5)?,
        selected: r.get(6)?,
        reason: r.get(7)?,
        path_limited: r.get(8)?,
        reason_limited: r.get(9)?,
        delivery: r.get(10)?,
        on_path: r.get(11)?,
        source_exists: r.get(12)?,
        source_available: r.get(13)?,
        source_reason: r.get(14)?,
        managed_state: r.get(15)?,
        managed_reason: r.get(16)?,
        source_reason_limited: r.get(17)?,
        managed_reason_limited: r.get(18)?,
    })
}
fn validate_id(id: &str) -> Result<(), StorageError> {
    if id.is_empty() || id.len() > 4096 {
        return Err(crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::InvalidArgument,
            "Attachment inspection requires explicit1..4096-byte identities",
        )
        .into());
    }
    Ok(())
}
pub(super) fn supports(command: &crate::protocol::Command) -> bool {
    matches!(command,crate::protocol::Command::AttachmentList(a) if a.thread_id.is_some())
}
pub(super) fn read_tx(db: &Connection, thread: &str) -> Result<Raw, StorageError> {
    validate_id(thread)?;
    let target = db.query_row("SELECT CASE WHEN octet_length(deleted_at)<=512 THEN deleted_at END,selection_revision,deleted_at IS NOT NULL FROM thread WHERE id=?1", [thread], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if target.is_none() {
        return Err(crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::ThreadNotFound,
            "Saved attachment Thread is unavailable",
        )
        .into());
    }
    let mut rows = Vec::new();
    let mut complete = true;
    for state in ["active", "excluded"] {
        let remaining = ROWS - rows.len();
        // Discover only keys, with a sentinel, before copying retained bodies.
        let count = db
            .prepare(PREFIX)?
            .query_map(params![thread, state, (remaining + 1) as i64], |_| Ok(()))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .len();
        let retained = count.min(remaining);
        if retained > 0 {
            let mut stmt = db.prepare(DETAIL)?;
            rows.extend(
                stmt.query_map(params![thread, state, retained as i64, TEXT], decode_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        if count > remaining {
            complete = false;
            break;
        }
    }
    Ok(Raw {
        thread: thread.into(),
        attachment: None,
        target,
        rows,
        complete,
    })
}
pub(super) fn read_selected_tx(
    db: &Connection,
    thread: &str,
    attachment: &str,
) -> Result<Raw, StorageError> {
    validate_id(thread)?;
    validate_id(attachment)?;
    let target=db.query_row("SELECT CASE WHEN octet_length(deleted_at)<=512 THEN deleted_at END,selection_revision,deleted_at IS NOT NULL FROM thread WHERE id=?1",[thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(||crate::protocol::ApiError::new(crate::protocol::ErrorCode::ThreadNotFound,"Saved attachment Thread is unavailable"))?;
    let rows = db
        .prepare(POINT_DETAIL)?
        .query_map(params![thread, attachment, TEXT], decode_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Raw {
        thread: thread.into(),
        attachment: Some(attachment.into()),
        target: Some(target),
        rows,
        complete: true,
    })
}
pub(super) fn prepare(mut raw: Raw) -> (Value, bool) {
    let scope = match &raw.attachment {
        Some(id) => {
            json!({"kind":"attachment","attachment_id":id,"selection_found":!raw.rows.is_empty()})
        }
        None => json!({"kind":"thread"}),
    };
    let exact = raw.attachment.is_some();
    raw.rows
        .sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
    let identity_complete = raw.rows.iter().all(|row| row.id.is_some());
    let mut limited = !raw.complete;
    let attachments = raw.rows.into_iter().map(|r| {
        let row_limited = r.id.is_none() || r.source.is_none() || r.path.is_none() || r.kind.is_none() || r.reason_limited || r.source_reason_limited || r.managed_reason_limited;
        limited |= row_limited;
        let unavailable=if r.managed_reason_limited && r.managed_state.as_deref().is_some_and(|state|state!="ready") {
            Some("Saved managed attachment reason exceeds the display limit; inspect with attachment list".into())
        } else {crate::attachments::saved_unavailable_reason(r.managed_state.as_deref(),r.managed_reason.as_deref(),r.reason.as_deref())};
        let has_reason = unavailable.is_some();
        let mut value = json!({"attachment_id":r.id,"source_node_id":r.source,"thread_id":raw.thread,"path":r.path,"kind":r.kind,"managed":r.managed,"selected":r.selected,"unavailable_reason":unavailable,"delivery_status":r.delivery,
            "saved_observation":{"selected_order":r.order,"on_selected_path":r.on_path,"source_exists":r.source_exists,"source_payload_available":r.source_available,"source_deleted_reason":r.source_reason,"managed_state":r.managed_state,"managed_deleted_reason":r.managed_reason,"path_limited":r.path_limited,"reason_limited":r.reason_limited,"source_reason_limited":r.source_reason_limited,"managed_reason_limited":r.managed_reason_limited,"complete":!row_limited}});
        if !has_reason { value.as_object_mut().expect("attachment object").remove("unavailable_reason"); }
        value
    }).collect::<Vec<_>>();
    let (exists, deleted_at, revision, deleted) = match raw.target {
        Some((at, rev, deleted)) => (true, at, Some(rev), Some(deleted)),
        None => (false, None, None, None),
    };
    (
        json!({"thread_id":raw.thread,"scope":scope,"target_exists":exists,"target_deleted_at":deleted_at,"target_deleted":deleted,"selection_revision":revision,"complete":!limited,"selection_exhausted":raw.complete,"global_order_complete":!exact&&raw.complete&&identity_complete,"count":{"value":attachments.len(),"complete":raw.complete,"relation":if raw.complete {"exact"} else {"lower_bound"}},"notice":if exact {"Exact saved attachment selection only; external file availability has not been checked."} else if raw.complete {"Saved references only; external file availability has not been checked."} else {"Bounded physical state-prefix cohort; additional saved references and global order are unknown. Use attachment list for the exhaustive saved list."},"attachments":attachments}),
        limited,
    )
}

pub(super) async fn request(
    control: &std::sync::Arc<crate::control::Control>,
    thread: String,
    attachment: Option<String>,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Value, crate::protocol::ApiError> {
    let observed_thread = thread.clone();
    let (raw, at) = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop,
        move |db| {
            let raw = match attachment {
                Some(id) => read_selected_tx(db, &thread, &id),
                None => read_tx(db, &thread),
            }?;
            Ok((raw, chrono::Utc::now().to_rfc3339()))
        },
    )
    .await?;
    let (mut value, limited) = super::presentation_work(move || prepare(raw)).await?;
    value["view_observation"] = json!({"thread_id":observed_thread,"observed_at":at});
    if limited {
        value["view_limits"] = json!({"limited_fields":["attachments"]});
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;
    #[test]
    fn explicit_attachment_scope_does_not_fall_back_to_the_viewer() {
        use crate::protocol::{Command, ThreadTarget};
        assert!(!supports(&Command::AttachmentList(ThreadTarget {
            thread_id: None
        })));
        assert!(supports(&Command::AttachmentList(ThreadTarget {
            thread_id: Some("foreign".into())
        })));
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('target','','/','','p','m','now'),('viewer','','/','','p','m','now');
        INSERT INTO node(id,kind,created_at,payload_json)VALUES('source','user','now','{}');
        INSERT INTO attachment_ref VALUES('a','source','/same','file','external','now'),('b','source','/same','image','managed_clipboard','now');
        INSERT INTO attachment_selection(thread_id,attachment_id,state,reason,selected_at,selected_order)VALUES('target','a','excluded','explicitly removed','now',2),('target','b','active',NULL,'now',1);
        INSERT INTO managed_file(id,absolute_path,state,created_at,deleted_reason)VALUES('managed','/same','deleted','now','saved GC');
        INSERT INTO attachment_delivery VALUES('source','target',NULL,'held','op','{}','now');").unwrap();
        db
    }
    fn canonical_rows(value: &Value) -> Value {
        Value::Array(
            value["attachments"]
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .map(|mut r| {
                    r.as_object_mut().unwrap().remove("saved_observation");
                    r
                })
                .collect(),
        )
    }
    #[test]
    fn complete_saved_rows_match_canonical_with_shared_source_delivery_and_tombstones() {
        let db = fixture();
        for applied in [false, true] {
            if applied {
                db.execute("INSERT INTO thread_path VALUES('target',0,'source')", [])
                    .unwrap();
            }
            let (value, limited) = prepare(read_tx(&db, "target").unwrap());
            assert!(!limited);
            assert_eq!(
                canonical_rows(&value),
                serde_json::to_value(crate::attachments::list_tx(&db, "target").unwrap()).unwrap()
            );
            assert_eq!(
                value["attachments"][0]["saved_observation"]["managed_state"],
                "deleted"
            );
            assert_eq!(value["attachments"][1]["selected"], false);
        }
        db.execute_batch("UPDATE node SET payload_json=NULL,deleted_reason='saved tombstone';UPDATE thread SET deleted_at='now' WHERE id='target';").unwrap();
        let (value, _) = prepare(read_tx(&db, "target").unwrap());
        assert_eq!(value["target_deleted_at"], "now");
        assert_eq!(
            value["attachments"][0]["saved_observation"]["source_payload_available"],
            false
        );
        assert_eq!(
            canonical_rows(&value),
            serde_json::to_value(crate::attachments::list_tx(&db, "target").unwrap()).unwrap()
        );
        assert!(
            matches!(read_tx(&db, "missing"), Err(StorageError::Operation(error)) if error.code == crate::protocol::ErrorCode::ThreadNotFound)
        );
        assert!(
            crate::attachments::list_tx(&db, "missing")
                .unwrap()
                .is_empty(),
            "canonical explicit CLI behavior remains unchanged"
        );
    }
    fn seed(db: &Connection, start: usize, end: usize) {
        for n in start..end {
            db.execute(
                "INSERT INTO attachment_ref VALUES(?1,'source','/same','file','external','now')",
                [format!("extra-{n:05}")],
            )
            .unwrap();
            db.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('target',?1,'active','now',0)",[format!("extra-{n:05}")]).unwrap();
        }
    }
    fn work(db: &Connection, sql: &str, limit: i64) -> (i32, i32) {
        let mut statement = db.prepare(sql).unwrap();
        let mut rows = statement
            .query(params!["target", "active", limit, TEXT])
            .unwrap();
        while rows.next().unwrap().is_some() {}
        drop(rows);
        (
            statement.get_status(StatementStatus::VmStep),
            statement.get_status(StatementStatus::FullscanStep),
        )
    }
    #[test]
    fn overflow_bounds_actual_join_work_and_marks_global_order_unknown() {
        let db = fixture();
        seed(&db, 0, 1000);
        let before = work(&db, DETAIL, 128);
        let raw = read_tx(&db, "target").unwrap();
        assert_eq!(raw.rows.len(), 128);
        let (value, limited) = prepare(raw);
        assert!(limited);
        assert_eq!(value["count"]["relation"], "lower_bound");
        assert_eq!(value["global_order_complete"], false);
        seed(&db, 1000, 10000);
        assert_eq!(
            work(&db, DETAIL, 128),
            before,
            "same physical prefix has identical VM work despite tied-order fanout"
        );
    }
    #[test]
    fn guarded_cells_are_unknown_not_rewritten_and_prepare_needs_no_database() {
        let db = fixture();
        db.execute(
            "UPDATE attachment_ref SET absolute_path=?1 WHERE id='a'",
            ["字".repeat(1000)],
        )
        .unwrap();
        db.execute(
            "UPDATE attachment_selection SET reason=?1 WHERE attachment_id='a'",
            ["x".repeat(10000)],
        )
        .unwrap();
        let raw = read_tx(&db, "target").unwrap();
        drop(db);
        let (value, limited) = prepare(raw);
        assert!(limited);
        assert!(value["attachments"][1]["path"].is_null());
        assert_eq!(
            value["attachments"][1]["saved_observation"]["reason_limited"],
            true
        );
        assert_eq!(value["attachments"][1]["attachment_id"], "a");
    }
    fn point_work(db: &Connection, id: &str) -> (i32, i32) {
        let mut statement = db.prepare(POINT_DETAIL).unwrap();
        statement
            .query_map(params!["target", id, TEXT], |_| Ok(()))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        (
            statement.get_status(StatementStatus::VmStep),
            statement.get_status(StatementStatus::FullscanStep),
        )
    }
    #[test]
    fn exact_selection_is_independent_of_list_cohort_and_grows_no_scan() {
        let db = fixture();
        seed(&db, 0, 1000);
        let (list, limited) = prepare(read_tx(&db, "target").unwrap());
        assert!(limited);
        assert!(
            !list["attachments"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["attachment_id"] == "a")
        );
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let raw = read_selected_tx(&db, "target", "a").unwrap();
        assert_eq!(trace.count(), 2);
        drop(trace);
        let (value, limited) = prepare(raw);
        assert!(!limited);
        assert_eq!(
            value["scope"],
            json!({"kind":"attachment","attachment_id":"a","selection_found":true})
        );
        assert_eq!(value["count"]["value"], 1);
        assert_eq!(
            value["attachments"][0]["unavailable_reason"],
            "saved GC (deleted)"
        );
        assert_eq!(value["attachments"][0]["selected"], false);
        assert_eq!(value["global_order_complete"], false);
        let before = point_work(&db, "a");
        seed(&db, 1000, 10000);
        assert_eq!(point_work(&db, "a"), before);
        assert_eq!(before.1, 0);
        let (missing, _) = prepare(read_selected_tx(&db, "viewer", "a").unwrap());
        assert_eq!(missing["thread_id"], "viewer");
        assert_eq!(missing["scope"]["selection_found"], false);
        assert_eq!(missing["count"]["value"], 0);
        assert!(missing["attachments"].as_array().unwrap().is_empty());
    }
    #[test]
    fn invalid_scope_does_no_sql_and_limited_managed_reason_is_not_defaulted() {
        let db = fixture();
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        for (thread, id) in [("", "a"), ("target", ""), ("target", &"x".repeat(4097))] {
            assert!(read_selected_tx(&db, thread, id).is_err());
        }
        assert_eq!(trace.count(), 0);
        drop(trace);
        db.execute(
            "UPDATE managed_file SET deleted_reason=?1",
            ["x".repeat(1000)],
        )
        .unwrap();
        let (value, limited) = prepare(read_selected_tx(&db, "target", "a").unwrap());
        assert!(limited);
        assert_eq!(
            value["attachments"][0]["saved_observation"]["managed_reason_limited"],
            true
        );
        assert!(
            value["attachments"][0]["unavailable_reason"]
                .as_str()
                .unwrap()
                .contains("exceeds the display limit")
        );
    }
}
