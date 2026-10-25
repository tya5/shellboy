//! Current roots shared by GC and history visibility.
// One current-root rule for mark scanning and destructive-time validation.
pub(crate) const ROOT_PREDICATE: &str = "EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=n.id AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=n.turn_id AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM receipt r LEFT JOIN thread t ON t.id=r.target_thread_id WHERE r.message_node_id=n.id AND (t.deleted_at IS NULL OR r.state IN ('accepted','held'))) OR EXISTS(SELECT 1 FROM attachment_delivery d JOIN thread t ON t.id=d.thread_id WHERE d.node_id=n.id AND d.status IN ('accepted','held') AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind IN ('node','history_node','source_node') AND resource_id=n.id) OR EXISTS(SELECT 1 FROM replacement r JOIN thread_replacement tr ON tr.replacement_id=r.id JOIN thread t ON t.id=tr.thread_id WHERE r.output_node_id=n.id AND t.deleted_at IS NULL)";

// Negative ownership evidence is intentionally narrower than GC eligibility:
// ownerless imported/legacy sources remain searchable until ordinary GC removes
// their payload. A live GC root always takes precedence over a deleted owner.
const DELETED_OWNER: &str = "EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=n.id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=n.turn_id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM receipt r JOIN thread t ON t.id=r.target_thread_id WHERE r.message_node_id=n.id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM attachment_delivery d JOIN thread t ON t.id=d.thread_id WHERE d.node_id=n.id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM replacement r JOIN thread_replacement tr ON tr.replacement_id=r.id JOIN thread t ON t.id=tr.thread_id WHERE r.output_node_id=n.id AND t.deleted_at IS NOT NULL)";

/// Read-only current visibility. Reverse replacement traversal preserves live
/// summary/spill provenance even when the original node left every selected path.
/// UNION terminates cycles; source reverse indexes bound each adjacency lookup.
pub(crate) fn searchable(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<bool> {
    let sql = format!(
        "WITH RECURSIVE consumers(id) AS(SELECT ?1 UNION SELECT r.output_node_id FROM consumers c JOIN replacement_source s ON s.source_node_id=c.id JOIN replacement r ON r.id=s.replacement_id) SELECT EXISTS(SELECT 1 FROM consumers c JOIN node n ON n.id=c.id WHERE {ROOT_PREDICATE}) OR NOT EXISTS(SELECT 1 FROM consumers c JOIN node n ON n.id=c.id WHERE {DELETED_OWNER})"
    );
    db.prepare_cached(&sql)?.query_row([id], |row| row.get(0))
}
