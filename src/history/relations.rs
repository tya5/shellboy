//! Direct conversation edges and replacement witnesses are separate relations.
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::protocol::{
    ApiError, ErrorCode, HistoryRelation, HistoryRelationsArgs, MAX_LIST_LIMIT, NodeProvenance,
    Page, PageCursor, RelationItem,
};

use super::sql_error;

const ATTEMPT_OWNER_SQL: &str = "SELECT COUNT(DISTINCT attempt_id),MIN(CASE WHEN length(CAST(attempt_id AS BLOB))<=36 THEN attempt_id END),MAX(length(CAST(attempt_id AS BLOB))) FROM output_item INDEXED BY output_item_node WHERE node_id=?1";

fn saved_attempt_id(connection: &Connection, node_id: &str) -> Result<Option<String>, ApiError> {
    let (owners, candidate, max_bytes): (i64, Option<String>, Option<i64>) = connection
        .query_row(ATTEMPT_OWNER_SQL, [node_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(sql_error)?;
    if owners == 0 || owners > 1 {
        return Ok(None);
    }
    let malformed = || {
        ApiError::new(
            ErrorCode::InvalidState,
            "saved assistant response has malformed attempt provenance",
        )
    };
    let Some(candidate) = candidate else {
        return Err(malformed());
    };
    if max_bytes != Some(36) {
        return Err(malformed());
    }
    let parsed = Uuid::parse_str(&candidate).map_err(|_| malformed())?;
    if parsed.to_string() != candidate {
        return Err(malformed());
    }
    Ok(Some(candidate))
}

/// Metadata describes creation, not the potentially many Threads selecting a node.
pub fn provenance(connection: &Connection, node_id: &str) -> Result<NodeProvenance, ApiError> {
    let mut result=connection.query_row(
        "SELECT n.kind,n.created_at,n.payload_revision,n.turn_id,t.run_id,COALESCE(t.thread_id,r.thread_id) FROM node n LEFT JOIN turn t ON t.id=n.turn_id LEFT JOIN run r ON r.id=t.run_id WHERE n.id=?1",[node_id],|row|Ok(NodeProvenance{ notification: None,message:None,sender:None,attempt_id:None,kind:row.get(0)?,created_at:row.get(1)?,source_revision:row.get(2)?,turn_id:row.get(3)?,run_id:row.get(4)?,origin_thread_id:row.get(5)?})
    ).optional().map_err(sql_error)?.ok_or_else(||ApiError::new(ErrorCode::TargetUnavailable,"history node not found"))?;
    if result.kind == "assistant_response" {
        result.attempt_id = saved_attempt_id(connection, node_id)?;
    }
    if result.kind == "user" {
        let own:Option<Option<String>>=connection.query_row("SELECT CASE WHEN length(CAST(sender_json AS BLOB))<=8192 THEN sender_json END FROM receipt WHERE message_node_id=?1",[node_id],|r|r.get(0)).optional().map_err(sql_error)?;
        let raw = if let Some(own) = own {
            own
        } else if let Some(thread) = result.origin_thread_id.as_deref() {
            super::message_provenance::unique_reverse(connection, thread, node_id)
                .map_err(sql_error)?
                .and_then(|(_, raw)| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|v| v.pointer("/reply/by").cloned())
                .and_then(|v| serde_json::to_string(&v).ok())
        } else {
            None
        };
        result.sender = Some(crate::protocol::MessageSender::from_saved(raw.as_deref()));
    }
    Ok(result)
}

/// Only history get/list/window publish the separately budgeted optional relation.
pub(super) fn projected_provenance(
    connection: &Connection,
    node_id: &str,
) -> Result<NodeProvenance, ApiError> {
    let mut result = provenance(connection, node_id)?;
    if result.kind == "user"
        && let Some(thread) = result.origin_thread_id.as_deref()
    {
        result.message =
            super::message_provenance::project(connection, node_id, thread).map_err(sql_error)?;
    }
    if result.kind == "event" {
        result.notification = super::notification::descriptor(connection, node_id)
            .map_err(sql_error)?
            .map(|value| value.into_descriptor());
    }
    Ok(result)
}

pub fn relations(
    connection: &Connection,
    args: &HistoryRelationsArgs,
) -> Result<Page<RelationItem>, ApiError> {
    if args.limit == 0 || args.limit > MAX_LIST_LIMIT {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "history relation limit must be in 1..=200",
        ));
    }
    let metadata = provenance(connection, &args.node_id)?;
    let revision = metadata.source_revision as u64;
    let relation = match args.relation {
        HistoryRelation::Previous => "previous",
        HistoryRelation::Next => "next",
        HistoryRelation::Sources => "sources",
        HistoryRelation::Replacements => "replacements",
    };
    let filter = serde_json::to_string(&(&args.node_id, relation)).map_err(|_| invalid_cursor())?;
    // Both SQL branches expose one order: group rowid, source ordinal, witness rowid.
    // Source witnesses are ordered by their original range ordinal, not node IDs.
    let (from, constraint, columns, order) = match args.relation {
        HistoryRelation::Previous => (
            "edge e",
            "e.dst=?1",
            "e.src,NULL,NULL,NULL,e.rowid,0,e.rowid",
            "e.rowid,CAST(0 AS INTEGER),e.rowid",
        ),
        HistoryRelation::Next => (
            "edge e",
            "e.src=?1",
            "e.dst,NULL,NULL,NULL,e.rowid,0,e.rowid",
            "e.rowid,CAST(0 AS INTEGER),e.rowid",
        ),
        HistoryRelation::Sources => (
            "replacement_source s JOIN replacement r ON r.id=s.replacement_id",
            "r.output_node_id=?1 AND r.state='completed'",
            "s.source_node_id,r.id,s.source_revision,s.source_hash,r.rowid,s.ordinal,s.rowid",
            "r.rowid,s.ordinal,s.rowid",
        ),
        HistoryRelation::Replacements => (
            "replacement_source s JOIN replacement r ON r.id=s.replacement_id",
            "s.source_node_id=?1 AND r.state='completed'",
            "r.output_node_id,r.id,s.source_revision,s.source_hash,r.rowid,s.ordinal,s.rowid",
            "r.rowid,s.ordinal,s.rowid",
        ),
    };
    let is_edge = matches!(
        args.relation,
        HistoryRelation::Previous | HistoryRelation::Next
    );
    let row_id = if is_edge { "e.rowid" } else { "s.rowid" };
    let current_max: i64 = connection
        .query_row(
            &format!("SELECT COALESCE(MAX({row_id}),0) FROM {from} WHERE {constraint}"),
            [&args.node_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let (last, watermark) = if let Some(token) = &args.cursor {
        let cursor = PageCursor::decode(token, "history.relations", &filter, revision, None)?;
        let last: (i64, i64, i64) =
            serde_json::from_str(&cursor.last_sort_key).map_err(|_| invalid_cursor())?;
        if last.0 <= 0
            || last.1 < 0
            || last.2 <= 0
            || last.2 as u64 > cursor.watermark
            || cursor.watermark > i64::MAX as u64
            || cursor.watermark > current_max as u64
        {
            return Err(invalid_cursor());
        }
        (last, cursor.watermark)
    } else {
        ((0, 0, 0), current_max as u64)
    };
    let tuple = format!("({order})");
    let sql = format!(
        "SELECT {columns} FROM {from} WHERE {constraint} AND {row_id}<=?2 AND {tuple}>(?3,?4,?5) ORDER BY {order} LIMIT ?6"
    );
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            args.node_id,
            watermark as i64,
            last.0,
            last.1,
            last.2,
            args.limit + 1
        ])
        .map_err(sql_error)?;
    let mut items = Vec::new();
    let mut position = last;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(sql_error)? {
        if items.len() == args.limit as usize {
            has_more = true;
            break;
        }
        let node_id: String = row.get(0).map_err(sql_error)?;
        items.push(RelationItem {
            provenance: provenance(connection, &node_id)?,
            node_id,
            replacement_id: row.get(1).map_err(sql_error)?,
            source_revision: row.get(2).map_err(sql_error)?,
            source_hash: row.get(3).map_err(sql_error)?,
        });
        position = (
            row.get(4).map_err(sql_error)?,
            row.get(5).map_err(sql_error)?,
            row.get(6).map_err(sql_error)?,
        );
    }
    let next_cursor = if has_more {
        Some(
            PageCursor {
                version: 1,
                scope: "history.relations".into(),
                filter,
                last_sort_key: serde_json::to_string(&position).map_err(|_| invalid_cursor())?,
                watermark,
                revision,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

fn invalid_cursor() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "history relation position is unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt_provenance_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE node(id TEXT PRIMARY KEY,kind TEXT,created_at TEXT,payload_revision INTEGER,turn_id TEXT);
             CREATE TABLE turn(id TEXT PRIMARY KEY,run_id TEXT,thread_id TEXT);
             CREATE TABLE run(id TEXT PRIMARY KEY,thread_id TEXT);
             CREATE TABLE receipt(message_node_id TEXT,sender_json TEXT);
             CREATE TABLE model_attempt(id TEXT PRIMARY KEY);
             CREATE TABLE output_item(attempt_id TEXT NOT NULL REFERENCES model_attempt(id),ordinal INTEGER NOT NULL,item_id TEXT,item_type TEXT NOT NULL,phase TEXT,status TEXT NOT NULL,payload_json TEXT NOT NULL,node_id TEXT REFERENCES node(id),PRIMARY KEY(attempt_id,ordinal));
             CREATE INDEX output_item_node ON output_item(node_id);
             INSERT INTO node VALUES('assistant','assistant_response','now',0,NULL),('missing','assistant_response','now',0,NULL),('ambiguous','assistant_response','now',0,NULL),('other-kind','user','now',0,NULL);",
        )
        .unwrap();
        db
    }

    fn add_output_owner(db: &Connection, node: &str, attempt: &str, ordinal: i64) {
        db.execute(
            "INSERT OR IGNORE INTO model_attempt(id) VALUES(?1)",
            [attempt],
        )
        .unwrap();
        db.execute(
            "INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id) VALUES(?1,?2,'message','completed','{\"private\":\"payload\"}',?3)",
            params![attempt, ordinal, node],
        )
        .unwrap();
    }

    #[test]
    fn attempt_provenance_requires_one_exact_owner_and_skips_other_kinds() {
        let db = attempt_provenance_db();
        let owner = Uuid::new_v4().to_string();
        add_output_owner(&db, "assistant", &owner, 0);
        add_output_owner(&db, "assistant", &owner, 1);
        add_output_owner(&db, "other-kind", &Uuid::new_v4().to_string(), 0);
        add_output_owner(&db, "ambiguous", &Uuid::new_v4().to_string(), 0);
        add_output_owner(&db, "ambiguous", &Uuid::new_v4().to_string(), 0);

        assert_eq!(
            provenance(&db, "assistant").unwrap().attempt_id.as_deref(),
            Some(owner.as_str())
        );
        assert_eq!(provenance(&db, "missing").unwrap().attempt_id, None);
        assert_eq!(provenance(&db, "ambiguous").unwrap().attempt_id, None);
        assert_eq!(provenance(&db, "other-kind").unwrap().attempt_id, None);
        assert_eq!(
            provenance(&db, "assistant").unwrap().kind,
            "assistant_response"
        );
    }

    #[test]
    fn unique_malformed_attempt_owner_is_strict_not_absent() {
        let db = attempt_provenance_db();
        db.execute(
            "INSERT INTO node VALUES('malformed','assistant_response','now',0,NULL),('oversized','assistant_response','now',0,NULL)",
            [],
        )
        .unwrap();
        add_output_owner(&db, "malformed", "not-a-uuid", 0);
        add_output_owner(&db, "oversized", &"x".repeat(4096), 0);

        for node in ["malformed", "oversized"] {
            let error = provenance(&db, node).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidState);
            assert_eq!(
                error.message,
                "saved assistant response has malformed attempt provenance"
            );
        }
    }

    #[test]
    fn attempt_owner_lookup_uses_node_index_and_never_reads_output_payload() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

        let db = attempt_provenance_db();
        let owner = Uuid::new_v4().to_string();
        add_output_owner(&db, "assistant", &owner, 0);
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {ATTEMPT_OWNER_SQL}"))
            .unwrap()
            .query_map(["assistant"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|detail| detail.contains("SEARCH output_item USING INDEX output_item_node")),
            "{plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|detail| detail.contains("SCAN output_item")),
            "{plan:?}"
        );

        db.authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read {
                table_name: "output_item",
                column_name: "payload_json",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
        assert_eq!(
            provenance(&db, "assistant").unwrap().attempt_id.as_deref(),
            Some(owner.as_str())
        );
        db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .unwrap();
    }

    #[test]
    fn reply_node_provenance_follows_original_reply_by_sender() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE node(id TEXT PRIMARY KEY,kind TEXT,created_at TEXT,payload_revision INTEGER,turn_id TEXT); \
             CREATE TABLE turn(id TEXT PRIMARY KEY,run_id TEXT,thread_id TEXT); \
             CREATE TABLE run(id TEXT PRIMARY KEY,thread_id TEXT); \
             CREATE TABLE schema_migration(version INTEGER PRIMARY KEY,name TEXT); CREATE TABLE receipt(rowid INTEGER PRIMARY KEY,id TEXT,message_node_id TEXT,result_json TEXT,sender_json TEXT,target_thread_id TEXT); \
             INSERT INTO turn VALUES('turn','run','recipient'); \
             INSERT INTO run VALUES('run','recipient'); \
             INSERT INTO node VALUES('reply','user','now',0,'turn'); \
             INSERT INTO receipt(id,message_node_id,result_json,sender_json,target_thread_id) VALUES('receipt','original','{\"reply\":{\"state\":\"replied\",\"node_id\":\"reply\",\"by\":{\"kind\":\"external_cli\",\"label\":\"reviewer\"}}}',NULL,'recipient');",
        )
        .unwrap();

        db.execute_batch(include_str!(
            "../../migrations/0041_receipt_reply_node_lookup.sql"
        ))
        .unwrap();
        let provenance = provenance(&db, "reply").unwrap();
        let sender = provenance.sender.unwrap();
        assert_eq!(sender.kind, crate::protocol::SenderKind::ExternalCli);
        assert_eq!(sender.label.as_deref(), Some("reviewer"));
        assert_eq!(provenance.kind, "user");
    }
}
