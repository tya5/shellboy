//! Snapshot facts for deletion review; no independent GC reachability engine.
pub(crate) mod observation;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use crate::protocol::{ApiError, ErrorCode, ThreadDeleteArgs};

pub(crate) const MAX_PREVIEW_THREADS: u32 = observation::THREAD_LIMIT as u32 + 1;

/// Canonical digest of a complete, bounded, sorted distinct target set.
pub(crate) fn target_marker<'a>(
    ids: impl IntoIterator<Item = &'a str>,
) -> crate::protocol::DeletionPreviewMarker {
    use sha2::{Digest, Sha256};
    let ids = ids.into_iter().collect::<std::collections::BTreeSet<_>>();
    let mut hash = Sha256::new();
    hash.update(b"shellboy.thread.delete.v1\0");
    for id in &ids {
        hash.update((id.len() as u32).to_be_bytes());
        hash.update(id.as_bytes());
    }
    crate::protocol::DeletionPreviewMarker {
        thread_count: ids.len() as u32,
        ids_hash: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// Reuses the observation's physical candidate/depth limits. A partial scope
/// cannot be attested; it must never fall back to unbounded guarded execution.
pub(crate) fn guarded_targets(
    db: &Connection,
    args: &ThreadDeleteArgs,
) -> Result<Option<Vec<String>>, ApiError> {
    match observation::selected_targets(db, args) {
        Ok(targets) => Ok(targets),
        Err(error) => {
            let error = error.into_api_error();
            if error.code == ErrorCode::InputLimit {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

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
        "SELECT count(*) FROM path_nodes n WHERE EXISTS(SELECT 1 FROM replacement_source rs INDEXED BY replacement_source_reverse JOIN replacement r ON r.id=rs.replacement_id WHERE rs.source_node_id=n.id AND r.state='completed')",
    )?;
    let surviving_sources = count(
        "SELECT count(*) FROM path_nodes n WHERE EXISTS(SELECT 1 FROM replacement_source rs INDEXED BY replacement_source_reverse JOIN replacement r ON r.id=rs.replacement_id WHERE rs.source_node_id=n.id AND r.state='completed' AND (EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=r.output_node_id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected s WHERE s.id=t.id)) OR EXISTS(SELECT 1 FROM thread_replacement tr JOIN thread t ON t.id=tr.thread_id WHERE tr.replacement_id=r.id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected s WHERE s.id=t.id))))",
    )?;
    let attachments =
        count("SELECT count(*) FROM attachment_ref a JOIN path_nodes n ON n.id=a.source_node_id")?;
    let external = count(
        "SELECT count(*) FROM attachment_ref a JOIN path_nodes n ON n.id=a.source_node_id WHERE a.ownership='external'",
    )?;
    let targets = guarded_targets(db, args)?;
    let marker = targets
        .as_ref()
        .map(|ids| target_marker(ids.iter().map(String::as_str)));
    Ok(json!({
        "preview_marker":marker,"preview_marker_unavailable_reason":if targets.is_some(){None}else{Some("input_limit")},
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

#[cfg(test)]
mod marker_tests {
    use super::*;

    const OLD_SURVIVING_SOURCES: &str = "WITH path_nodes(id) AS (SELECT DISTINCT p.node_id FROM thread_path p JOIN selected_thread s ON s.id=p.thread_id) SELECT count(DISTINCT n.id) FROM path_nodes n JOIN replacement_source rs ON rs.source_node_id=n.id JOIN replacement r ON r.id=rs.replacement_id WHERE r.state='completed' AND (EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=r.output_node_id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected_thread s WHERE s.id=t.id)) OR EXISTS(SELECT 1 FROM thread_replacement tr JOIN thread t ON t.id=tr.thread_id WHERE tr.replacement_id=r.id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected_thread s WHERE s.id=t.id)))";
    const OLD_COMPLETED_SOURCES: &str = "WITH path_nodes(id) AS (SELECT DISTINCT p.node_id FROM thread_path p JOIN selected_thread s ON s.id=p.thread_id) SELECT count(DISTINCT n.id) FROM path_nodes n JOIN replacement_source rs ON rs.source_node_id=n.id JOIN replacement r ON r.id=rs.replacement_id WHERE r.state='completed'";
    const SET_DRIVEN_COMPLETED_SOURCES: &str = "WITH path_nodes(id) AS MATERIALIZED (SELECT DISTINCT p.node_id FROM thread_path p JOIN selected_thread s ON s.id=p.thread_id) SELECT count(*) FROM path_nodes n WHERE EXISTS(SELECT 1 FROM replacement_source rs INDEXED BY replacement_source_reverse JOIN replacement r ON r.id=rs.replacement_id WHERE rs.source_node_id=n.id AND r.state='completed')";
    const SET_DRIVEN_SURVIVING_SOURCES: &str = "WITH path_nodes(id) AS MATERIALIZED (SELECT DISTINCT p.node_id FROM thread_path p JOIN selected_thread s ON s.id=p.thread_id) SELECT count(*) FROM path_nodes n WHERE EXISTS(SELECT 1 FROM replacement_source rs INDEXED BY replacement_source_reverse JOIN replacement r ON r.id=rs.replacement_id WHERE rs.source_node_id=n.id AND r.state='completed' AND (EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=r.output_node_id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected_thread s WHERE s.id=t.id)) OR EXISTS(SELECT 1 FROM thread_replacement tr JOIN thread t ON t.id=tr.thread_id WHERE tr.replacement_id=r.id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM selected_thread s WHERE s.id=t.id))))";

    fn reference_fixture() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE thread(id TEXT PRIMARY KEY,deleted_at TEXT); CREATE TABLE selected_thread(id TEXT PRIMARY KEY); CREATE TABLE thread_path(thread_id TEXT,node_id TEXT); CREATE INDEX thread_path_node ON thread_path(node_id,thread_id); CREATE TABLE replacement(id TEXT PRIMARY KEY,state TEXT,output_node_id TEXT); CREATE TABLE replacement_source(replacement_id TEXT,source_node_id TEXT); CREATE INDEX replacement_source_reverse ON replacement_source(source_node_id); CREATE TABLE thread_replacement(thread_id TEXT,replacement_id TEXT,PRIMARY KEY(thread_id,replacement_id)); INSERT INTO thread VALUES ('selected-a',NULL),('selected-b',NULL),('survivor',NULL),('survivor-b',NULL),('deleted','gone'); INSERT INTO selected_thread VALUES ('selected-a'),('selected-b'); INSERT INTO thread_path VALUES ('selected-a','shared'),('selected-b','shared'),('selected-a','single-a'),('selected-a','single-b'),('selected-a','empty-output'),('survivor','shared-output'),('deleted','deleted-output'); INSERT INTO replacement VALUES ('shared-output-ref','completed','shared-output'),('thread-replacement-ref','completed','unreferenced-output'),('selected-output-ref','completed','empty-output'),('deleted-output-ref','completed','deleted-output'),('pending-ref','pending','shared-output'),('multi-source-ref','completed','shared-output'),('survivor-b-ref','completed','unreferenced-output'); INSERT INTO replacement_source VALUES ('shared-output-ref','shared'),('thread-replacement-ref','shared'),('selected-output-ref','single-a'),('deleted-output-ref','single-b'),('pending-ref','shared'),('multi-source-ref','shared'),('multi-source-ref','single-a'),('survivor-b-ref','single-b'); INSERT INTO thread_replacement VALUES ('survivor','thread-replacement-ref'),('deleted','deleted-output-ref'),('survivor-b','survivor-b-ref');")
            .unwrap();
        db
    }

    #[test]
    fn set_driven_surviving_sources_matches_global_scan_on_synthetic_reference_graphs() {
        let db = reference_fixture();
        let compare = |label: &str| {
            let old_completed: i64 = db
                .query_row(OLD_COMPLETED_SOURCES, [], |row| row.get(0))
                .unwrap();
            let new_completed: i64 = db
                .query_row(SET_DRIVEN_COMPLETED_SOURCES, [], |row| row.get(0))
                .unwrap();
            assert_eq!(new_completed, old_completed, "completed sources: {label}");
            let old_surviving: i64 = db
                .query_row(OLD_SURVIVING_SOURCES, [], |row| row.get(0))
                .unwrap();
            let new_surviving: i64 = db
                .query_row(SET_DRIVEN_SURVIVING_SOURCES, [], |row| row.get(0))
                .unwrap();
            assert_eq!(new_surviving, old_surviving, "surviving sources: {label}");
        };
        compare(
            "shared nodes, multiple sources/replacements, shared outputs, and live/deleted references",
        );
        db.execute("DELETE FROM selected_thread", []).unwrap();
        db.execute(
            "DELETE FROM thread_path WHERE thread_id LIKE 'selected-%'",
            [],
        )
        .unwrap();
        compare("empty selected path-node set");
    }

    #[test]
    fn marker_sorts_deduplicates_and_length_prefixes_ids() {
        assert_eq!(target_marker(["b", "a", "a"]), target_marker(["a", "b"]));
        assert_ne!(target_marker(["ab", "c"]), target_marker(["a", "bc"]));
        assert_eq!(target_marker(["a", "b"]).thread_count, 2);
        assert_eq!(
            target_marker(["a", "b"]).ids_hash,
            "78469718155562294aa2f7c0e0278a4e86c9bf642dc2bbb0db48349c64bd326c"
        );
    }
}
