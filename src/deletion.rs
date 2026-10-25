//! Snapshot facts for deletion review; no independent GC reachability engine.
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use crate::protocol::{ApiError, ErrorCode, ThreadDeleteArgs};

// Match delete_thread's subtree: deleted Threads do not bridge traversal.
const SCOPE: &str = "WITH RECURSIVE descendants(id) AS (
 SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL
 UNION SELECT t.id FROM thread t JOIN descendants d ON t.parent_thread_id=d.id WHERE t.deleted_at IS NULL
), selected(id) AS (SELECT id FROM descendants WHERE ?2 OR id=?1),
path_nodes(id) AS (SELECT DISTINCT p.node_id FROM thread_path p JOIN selected s ON s.id=p.thread_id) ";

/// When called through Control, its transaction supplies the snapshot. Standalone
/// callers receive a read transaction too; no temporary tables or writes occur.
pub fn preview(connection: &Connection, args: &ThreadDeleteArgs) -> Result<Value, ApiError> {
    if connection.is_autocommit() {
        let transaction = connection.unchecked_transaction().map_err(sql_error)?;
        inspect(&transaction, args)
    } else {
        inspect(connection, args)
    }
}

fn inspect(db: &Connection, args: &ThreadDeleteArgs) -> Result<Value, ApiError> {
    let updated_at: String = db
        .query_row(
            "SELECT updated_at FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [&args.thread_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"))?;
    let root_revision: String = db
        .query_row(
            "SELECT value FROM app_meta WHERE key='root_revision'",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let root_revision: i64 = root_revision
        .parse()
        .map_err(|_| crate::storage::save_error("invalid root revision metadata"))?;
    let count = |query: &str| -> Result<i64, ApiError> {
        db.query_row(
            &format!("{SCOPE}{query}"),
            rusqlite::params![args.thread_id, args.descendants],
            |row| row.get(0),
        )
        .map_err(sql_error)
    };
    let thread_count = count("SELECT count(*) FROM selected")?;
    let descendant_count = count("SELECT count(*)-1 FROM descendants")?;
    let latest_updated_at: String = db
        .query_row(
            &format!("{SCOPE} SELECT max(t.updated_at) FROM thread t JOIN selected s ON s.id=t.id"),
            rusqlite::params![args.thread_id, args.descendants],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let live = count(
        "SELECT count(*) FROM run r JOIN selected s ON s.id=r.thread_id WHERE r.state='live'",
    )?;
    let stopping = count(
        "SELECT count(*) FROM run r JOIN selected s ON s.id=r.thread_id WHERE r.state='stopping'",
    )?;
    let runs = db.prepare(&format!("{SCOPE} SELECT r.id,r.thread_id,r.state FROM run r JOIN selected s ON s.id=r.thread_id WHERE r.state IN ('live','stopping') ORDER BY r.id LIMIT 50")).map_err(sql_error)?.query_map(rusqlite::params![args.thread_id,args.descendants],|row|Ok(json!({"id":row.get::<_,String>(0)?,"thread_id":row.get::<_,String>(1)?,"state":row.get::<_,String>(2)?}))).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    let selected_path_nodes = count("SELECT count(*) FROM path_nodes")?;
    let shared_path_nodes = count(
        "SELECT count(*) FROM path_nodes n WHERE EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=n.id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected s WHERE s.id=t.id))",
    )?;
    let surviving_path_threads = count(
        "SELECT count(DISTINCT p.thread_id) FROM thread_path p JOIN thread t ON t.id=p.thread_id JOIN path_nodes n ON n.id=p.node_id WHERE t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected s WHERE s.id=t.id)",
    )?;
    let sources = count(
        "SELECT count(DISTINCT n.id) FROM path_nodes n JOIN replacement_source rs ON rs.source_node_id=n.id JOIN replacement r ON r.id=rs.replacement_id WHERE r.state='completed'",
    )?;
    let surviving_sources = count(
        "SELECT count(DISTINCT n.id) FROM path_nodes n JOIN replacement_source rs ON rs.source_node_id=n.id JOIN replacement r ON r.id=rs.replacement_id WHERE r.state='completed' AND (EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=r.output_node_id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected s WHERE s.id=t.id)) OR EXISTS(SELECT 1 FROM thread_replacement tr JOIN thread t ON t.id=tr.thread_id WHERE tr.replacement_id=r.id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected s WHERE s.id=t.id)))",
    )?;
    let attachments =
        count("SELECT count(*) FROM attachment_ref a JOIN path_nodes n ON n.id=a.source_node_id")?;
    let external = count(
        "SELECT count(*) FROM attachment_ref a JOIN path_nodes n ON n.id=a.source_node_id WHERE a.ownership='external'",
    )?;
    Ok(json!({
        "thread_id":args.thread_id,"descendants":args.descendants,"updated_at":updated_at,
        "root_revision":root_revision,"snapshot_only":true,"deletable_now":live+stopping==0,
        "selection":{"thread_count":thread_count,"descendant_thread_count":descendant_count,"surviving_descendant_thread_count":descendant_count-(thread_count-1),"latest_updated_at":latest_updated_at},
        "blockers":{"live_run_count":live,"stopping_run_count":stopping,"runs":runs,"truncated":live+stopping>50},
        "reference_scope":"direct_references_not_gc_reachability",
        "references":{"selected_path_nodes":selected_path_nodes,"shared_path_nodes":shared_path_nodes,"surviving_path_thread_count":surviving_path_threads,"completed_replacement_source_nodes":sources,"surviving_replacement_source_nodes":surviving_sources,"attachment_reference_count":attachments,"external_attachment_reference_count":external,"managed_attachment_reference_count":attachments-external},
        "reclaimable_bytes":Value::Null,"reclaimable_reason":"requires_gc_reachability_and_storage_accounting"
    }))
}

fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("deletion preview database error: {error}"))
}
