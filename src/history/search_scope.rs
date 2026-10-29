//! Shared Thread search membership: selected path plus completed replacement sources.
use rusqlite::{Connection, params};

/// Parameter is a fixed SQL placeholder supplied by our query, never user input.
pub(crate) fn cte(parameter: &str) -> String {
    format!(
        "WITH RECURSIVE scope(id) AS (SELECT node_id FROM thread_path WHERE thread_id={parameter} UNION SELECT rs.source_node_id FROM scope s JOIN replacement r ON r.output_node_id=s.id AND r.state='completed' JOIN replacement_source rs ON rs.replacement_id=r.id)"
    )
}

pub(crate) fn exists(db: &Connection, thread: &str) -> rusqlite::Result<bool> {
    db.prepare_cached("SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)")?
        .query_row([thread], |row| row.get(0))
}

pub(crate) fn on_current_path(db: &Connection, thread: &str, node: &str) -> rusqlite::Result<bool> {
    db.prepare_cached("SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2)")?
        .query_row(params![thread, node], |row| row.get(0))
}

const CANDIDATE_SCOPE_SQL: &str = "WITH RECURSIVE consumers(id) AS (
        SELECT ?2
        UNION SELECT r.output_node_id FROM consumers c
          JOIN replacement_source rs ON rs.source_node_id=c.id
          JOIN replacement r ON r.id=rs.replacement_id AND r.state='completed'
    ) SELECT EXISTS(SELECT 1 FROM consumers c WHERE EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=c.id))";

/// Recheck one candidate inside its current read snapshot, following the
/// inverse of cte(): sources -> completed replacement outputs -> selected path.
/// The source reverse index and Thread-path unique index bound work to this
/// candidate's consumers; no whole-scope materialization is repeated per hit.
pub(crate) fn contains(db: &Connection, thread: &str, node: &str) -> rusqlite::Result<bool> {
    db.prepare_cached(CANDIDATE_SCOPE_SQL)?
        .query_row(params![thread, node], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn reverse_membership_equals_forward_scope_for_nested_pending_and_cyclic_replacements() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('one','/','/a','p','m','now'),('fork','/','/a','p','m','now');") .unwrap();
        for node in ["root", "a", "b", "nested", "foreign", "pending"] {
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')",
                [node],
            )
            .unwrap();
        }
        db.execute_batch("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('one',0,'root'),('fork',0,'a'),('fork',1,'foreign');
            INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('outer','root','outer','1','1','completed','now'),('inner','nested','inner','1','1','completed','now'),('cycle','b','cycle','1','1','completed','now'),('unfinished','root','unfinished','1','1','pending','now');
            INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('outer',0,'a',0),('outer',1,'nested',0),('inner',0,'b',0),('cycle',0,'root',0),('unfinished',0,'pending',0);").unwrap();
        for thread in ["one", "fork"] {
            let sql = format!("{} SELECT id FROM scope", cte("?1"));
            let forward = db
                .prepare(&sql)
                .unwrap()
                .query_map([thread], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<HashSet<_>>>()
                .unwrap();
            for node in ["root", "a", "b", "nested", "foreign", "pending", "missing"] {
                assert_eq!(
                    contains(&db, thread, node).unwrap(),
                    forward.contains(node),
                    "thread={thread}, node={node}"
                );
            }
        }
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {CANDIDATE_SCOPE_SQL}"))
            .unwrap()
            .query_map(params!["one", "a"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("replacement_source_reverse"), "{plan}");
        assert!(
            plan.contains("thread_id=? AND node_id=?"),
            "candidate validation must probe one indexed Thread-path pair rather than scan every path node: {plan}"
        );
        assert!(on_current_path(&db, "one", "root").unwrap());
        assert!(!on_current_path(&db, "one", "b").unwrap());
    }
}
