//! Current roots shared by GC and history visibility.
// One current-root rule for mark scanning and destructive-time validation.
pub(crate) const ROOT_PREDICATE: &str = "EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=n.id AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=n.turn_id AND t.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM replacement_source ps INDEXED BY replacement_source_reverse JOIN replacement pr ON pr.id=ps.replacement_id AND pr.state='pruned' JOIN thread_replacement ptr ON ptr.replacement_id=pr.id AND ptr.thread_id=t.id WHERE ps.source_node_id=n.id)) OR EXISTS(SELECT 1 FROM receipt r JOIN thread t ON t.id=r.target_thread_id WHERE r.message_node_id IN (n.id,CASE WHEN n.id GLOB 'reply-*' THEN substr(n.id,7) ELSE n.id END) AND (r.message_node_id=n.id OR json_extract(r.result_json,'$.reply.node_id')=n.id) AND (t.deleted_at IS NULL OR json_extract(r.result_json,'$.reply.state')='pending' OR EXISTS(SELECT 1 FROM thread sender WHERE sender.id=json_extract(r.sender_json,'$.thread_id') AND json_extract(r.sender_json,'$.kind')='model' AND sender.deleted_at IS NULL))) OR EXISTS(SELECT 1 FROM attachment_delivery d JOIN thread t ON t.id=d.thread_id WHERE d.node_id=n.id AND d.status IN ('accepted','held') AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind IN ('node','history_node','source_node') AND resource_id=n.id) OR EXISTS(SELECT 1 FROM replacement r JOIN thread_replacement tr ON tr.replacement_id=r.id JOIN thread t ON t.id=tr.thread_id WHERE r.output_node_id=n.id AND t.deleted_at IS NULL)";

// Negative ownership evidence is intentionally narrower than GC eligibility:
// ownerless imported/legacy sources remain searchable until ordinary GC removes
// their payload. A live GC root always takes precedence over a deleted owner.
const DELETED_OWNER: &str = "EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=n.id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=n.turn_id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM receipt r JOIN thread t ON t.id=r.target_thread_id WHERE r.message_node_id=n.id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM attachment_delivery d JOIN thread t ON t.id=d.thread_id WHERE d.node_id=n.id AND t.deleted_at IS NOT NULL) OR EXISTS(SELECT 1 FROM replacement r JOIN thread_replacement tr ON tr.replacement_id=r.id JOIN thread t ON t.id=tr.thread_id WHERE r.output_node_id=n.id AND t.deleted_at IS NOT NULL)";

// The node is a source of a replacement that `thread prune` marked (#235).
const PRUNE_SOURCE: &str = "EXISTS(SELECT 1 FROM replacement_source ps INDEXED BY replacement_source_reverse JOIN replacement pr ON pr.id=ps.replacement_id WHERE ps.source_node_id=n.id AND pr.state='pruned')";

/// The node was released by `thread prune` and nothing else currently holds it
/// (path, another Thread's Turn, receipt, pin, adopted unpruned consumer). Its
/// body may still be present until the history GC runs; readers treat it as
/// not retained so a fetch fails the same way before and after collection.
pub(crate) fn released_by_prune(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<bool> {
    let sql = format!(
        "WITH RECURSIVE consumers(id) AS(SELECT ?1 UNION SELECT r.output_node_id FROM consumers c JOIN replacement_source s ON s.source_node_id=c.id JOIN replacement r ON r.id=s.replacement_id AND r.state<>'pruned') SELECT EXISTS(SELECT 1 FROM node n WHERE n.id=?1 AND {PRUNE_SOURCE}) AND NOT EXISTS(SELECT 1 FROM consumers c JOIN node n ON n.id=c.id WHERE {ROOT_PREDICATE})"
    );
    db.prepare_cached(&sql)?.query_row([id], |row| row.get(0))
}

const CONSUMERS: &str = "WITH RECURSIVE consumers(id) AS(SELECT ?1 UNION SELECT r.output_node_id FROM consumers c JOIN replacement_source s ON s.source_node_id=c.id JOIN replacement r ON r.id=s.replacement_id AND r.state<>'pruned')";

/// Read-only current visibility. Reverse replacement traversal preserves live
/// summary/spill provenance even when the original node left every selected path.
/// UNION terminates cycles; source reverse indexes bound each adjacency lookup.
///
/// Two stages (#240): a live root on any consumer decides `true` by itself, and
/// the root-only statement lets SQLite stop at the first root instead of
/// materializing the whole closure (the combined statement references the CTE
/// twice and always builds it). Only a false root falls back to the full rule.
pub(crate) fn searchable(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<bool> {
    let root = format!(
        "{CONSUMERS} SELECT EXISTS(SELECT 1 FROM consumers c JOIN node n ON n.id=c.id WHERE {ROOT_PREDICATE})"
    );
    if db
        .prepare_cached(&root)?
        .query_row([id], |row| row.get(0))?
    {
        return Ok(true);
    }
    searchable_full(db, id)
}

/// The complete rule in one statement; the fallback of `searchable` and its
/// equivalence reference.
fn searchable_full(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<bool> {
    let sql = format!(
        "{CONSUMERS} SELECT EXISTS(SELECT 1 FROM consumers c JOIN node n ON n.id=c.id WHERE {ROOT_PREDICATE}) OR (NOT EXISTS(SELECT 1 FROM consumers c JOIN node n ON n.id=c.id WHERE {DELETED_OWNER}) AND NOT EXISTS(SELECT 1 FROM node n WHERE n.id=?1 AND {PRUNE_SOURCE}))"
    );
    db.prepare_cached(&sql)?.query_row([id], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    // Random replacement graphs (chains, fan-in, cycles) with live/deleted
    // owners, pins and pruned replacements. The two-stage rule must equal the
    // single-statement rule for every node.
    #[test]
    fn two_stage_searchable_equals_full_rule_on_synthetic_graphs() {
        let (mut yes, mut no) = (0, 0);
        for seed in 0..40u64 {
            let mut rng = Lcg(seed + 1);
            let db = rusqlite::Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            let nodes = 24;
            for i in 0..nodes {
                db.execute(
                    "INSERT INTO node(id,kind,created_at) VALUES(?1,'message','t')",
                    [format!("n{i}")],
                )
                .unwrap();
            }
            for t in 0..4 {
                let deleted = if rng.next(2) == 0 { None } else { Some("d") };
                db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,deleted_at,updated_at) VALUES(?1,'','','','','',?2,'t')", rusqlite::params![format!("t{t}"), deleted]).unwrap();
            }
            for i in 0..nodes {
                match rng.next(6) {
                    0 => {
                        db.execute(
                            "INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,?2,?3)",
                            rusqlite::params![
                                format!("t{}", rng.next(4)),
                                i as i64,
                                format!("n{i}")
                            ],
                        )
                        .unwrap();
                    }
                    1 => {
                        let turn = format!("u{i}");
                        let thread = format!("t{}", rng.next(4));
                        db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?2,'completed','t')", rusqlite::params![format!("r{i}"), thread]).unwrap();
                        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES(?1,?2,?3,'completed','t')", rusqlite::params![turn, thread, format!("r{i}")]).unwrap();
                        db.execute(
                            "UPDATE node SET turn_id=?1 WHERE id=?2",
                            rusqlite::params![turn, format!("n{i}")],
                        )
                        .unwrap();
                    }
                    2 => {
                        db.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at) VALUES(?1,'i','node',?2,0,'t')", rusqlite::params![format!("p{i}"), format!("n{i}")]).unwrap();
                    }
                    _ => {}
                }
            }
            for r in 0..16 {
                let state = ["completed", "completed", "pruned"][rng.next(3) as usize];
                db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES(?1,?2,'k','f','p',?3,'t')", rusqlite::params![format!("x{r}"), format!("n{}", rng.next(nodes)), state]).unwrap();
                for o in 0..=rng.next(3) {
                    db.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision) VALUES(?1,?2,?3,0)", rusqlite::params![format!("x{r}"), o as i64, format!("n{}", rng.next(nodes))]).unwrap();
                }
                if rng.next(3) > 0 {
                    db.execute(
                        "INSERT INTO thread_replacement(thread_id,replacement_id) VALUES(?1,?2)",
                        rusqlite::params![format!("t{}", rng.next(4)), format!("x{r}")],
                    )
                    .unwrap();
                }
            }
            for i in 0..nodes {
                let id = format!("n{i}");
                let (new, old) = (
                    searchable(&db, &id).unwrap(),
                    searchable_full(&db, &id).unwrap(),
                );
                assert_eq!(new, old, "seed {seed} node {id}");
                if new { yes += 1 } else { no += 1 }
            }
        }
        assert!(yes > 50 && no > 50, "both outcomes covered: {yes}/{no}");
    }
}
