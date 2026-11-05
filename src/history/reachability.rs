//! Current roots shared by GC and history visibility.
// One current-root rule for mark scanning and destructive-time validation.
pub(crate) const ROOT_PREDICATE: &str = "EXISTS(SELECT 1 FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=n.id AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=n.turn_id AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM receipt r JOIN thread t ON t.id=r.target_thread_id WHERE r.message_node_id IN (n.id,CASE WHEN n.id GLOB 'reply-*' THEN substr(n.id,7) ELSE n.id END) AND (r.message_node_id=n.id OR json_extract(r.result_json,'$.reply.node_id')=n.id) AND (t.deleted_at IS NULL OR json_extract(r.result_json,'$.reply.state')='pending' OR EXISTS(SELECT 1 FROM thread sender WHERE sender.id=json_extract(r.sender_json,'$.thread_id') AND json_extract(r.sender_json,'$.kind')='model' AND sender.deleted_at IS NULL))) OR EXISTS(SELECT 1 FROM attachment_delivery d JOIN thread t ON t.id=d.thread_id WHERE d.node_id=n.id AND d.status IN ('accepted','held') AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind IN ('node','history_node','source_node') AND resource_id=n.id) OR EXISTS(SELECT 1 FROM replacement r JOIN thread_replacement tr ON tr.replacement_id=r.id JOIN thread t ON t.id=tr.thread_id WHERE r.output_node_id=n.id AND t.deleted_at IS NULL)";

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

/// Evaluate searchable for a bounded set with shared reverse-closure queries.
/// Per-source membership preserves each scalar result without per-node queries.
pub(crate) fn searchable_many(
    db: &rusqlite::Connection,
    ids: &[String],
) -> rusqlite::Result<Vec<bool>> {
    let input = serde_json::to_string(ids)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let sql = format!(
        "WITH RECURSIVE requested(k,id) AS (SELECT CAST(key AS INTEGER),value FROM json_each(?1)), live_sources(id) AS (SELECT n.id FROM node n WHERE ({ROOT_PREDICATE}) UNION SELECT rs.source_node_id FROM live_sources l JOIN replacement r ON r.output_node_id=l.id JOIN replacement_source rs ON rs.replacement_id=r.id), deleted_sources(id) AS (SELECT n.id FROM node n WHERE ({DELETED_OWNER}) UNION SELECT rs.source_node_id FROM deleted_sources d JOIN replacement r ON r.output_node_id=d.id JOIN replacement_source rs ON rs.replacement_id=r.id) SELECT q.k,EXISTS(SELECT 1 FROM live_sources l WHERE l.id=q.id) OR NOT EXISTS(SELECT 1 FROM deleted_sources d WHERE d.id=q.id) FROM requested q ORDER BY q.k"
    );
    let mut statement = db.prepare(&sql)?;
    let rows = statement.query_map([input], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))
    })?;
    let mut result = vec![false; ids.len()];
    for row in rows {
        let (ordinal, visible) = row?;
        if let Some(slot) = result.get_mut(ordinal as usize) {
            *slot = visible;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod batch_equivalence_tests {
    use super::*;

    #[test]
    fn batched_visibility_matches_single_candidate_rules_across_forks_and_replacements() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,deleted_at) VALUES('live-a','/','/a','p','m','now',NULL),('live-b','/','/a','p','m','now',NULL),('deleted','/','/a','p','m','now','now');
            INSERT INTO node(id,kind,created_at,payload_json,body_hash,deleted_reason) VALUES('root-a','user','now','{}','h',NULL),('root-b','user','now','{}','h',NULL),('shared','user','now','{}','h',NULL),('hidden','user','now','{}','h',NULL),('ownerless','user','now','{}','h',NULL),('pending-only','user','now','{}','h',NULL),('summary','summary','now','{}','h',NULL),('dead-summary','summary','now','{}','h',NULL),('cycle-a','user','now','{}','h',NULL),('cycle-b','user','now','{}','h',NULL),('tombstone','user','now',NULL,'h','collected');
            INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('live-a',0,'root-a'),('live-a',1,'summary'),('live-b',0,'root-b'),('live-b',1,'shared'),('deleted',0,'hidden'),('deleted',1,'dead-summary'),('deleted',2,'shared');
            INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES('replace-live','summary','k','1','1','completed','now'),('replace-dead','dead-summary','k2','1','1','completed','now'),('unfinished','root-a','k3','1','1','pending','now'),('cycle-ab','cycle-b','k4','1','1','completed','now'),('cycle-ba','cycle-a','k5','1','1','completed','now');
            INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision) VALUES('replace-live',0,'shared',0),('replace-dead',0,'hidden',0),('unfinished',0,'pending-only',0),('cycle-ab',0,'cycle-a',0),('cycle-ba',0,'cycle-b',0);
            INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at) VALUES('pin','instance','node','ownerless',0,'now');").unwrap();
        let ids = [
            "root-a",
            "root-b",
            "shared",
            "hidden",
            "ownerless",
            "pending-only",
            "summary",
            "dead-summary",
            "tombstone",
            "missing",
            "cycle-a",
            "cycle-b",
        ]
        .map(str::to_owned);
        let hidden_owned = db
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM node n WHERE n.id='hidden' AND {DELETED_OWNER})"
                ),
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        assert!(hidden_owned);
        let expected = ids
            .iter()
            .map(|id| searchable(&db, id).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(searchable_many(&db, &ids).unwrap(), expected);
        assert_eq!(
            expected,
            [
                true, true, true, false, true, true, true, false, true, true, true, true
            ]
        );
        let batched = searchable_many(&db, &ids).unwrap();
        let rank = |visible: &[bool]| {
            let mut selected = ids
                .iter()
                .enumerate()
                .filter(|(index, _)| visible[*index])
                .map(|(index, id)| (index as f64 / 7.0, id.clone()))
                .collect::<Vec<_>>();
            selected.sort_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
            });
            selected.truncate(4);
            selected
        };
        assert_eq!(rank(&expected), rank(&batched));
    }
}
