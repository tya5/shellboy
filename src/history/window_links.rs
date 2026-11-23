//! Indexed candidate scans for a window row; no validation or sort of the full ledger.
use super::*;
use std::collections::BTreeSet;

const CANDIDATES: usize = 64;
const LINK_LIMIT: usize = 8;

pub(super) struct Links {
    pub(super) rows: Vec<HistoryExecutionLink>,
    pub(super) count: u64,
    pub(super) exact: bool,
}

fn candidates<T>(
    db: &Connection,
    sql: &str,
    key: &str,
    remaining: &mut usize,
    exact: &mut bool,
    row: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, ApiError> {
    let mut stmt = db.prepare_cached(sql).map_err(sql_error)?;
    let mut rows = stmt
        .query_map(params![key, *remaining as i64 + 1], row)
        .map_err(sql_error)?;
    let mut batch = Vec::new();
    for row in rows.by_ref().take(*remaining) {
        batch.push(row.map_err(sql_error)?);
    }
    *remaining -= batch.len();
    if rows.next().transpose().map_err(sql_error)?.is_some() {
        *exact = false;
    }
    Ok(batch)
}

pub(super) fn scan(db: &Connection, node: &str) -> Result<Links, ApiError> {
    let mut exact = true;
    // Each stage has a separate budget, so duplicate output rows cannot starve calls
    // for an admitted attempt. Result candidates are independent of request candidates.
    let (mut outputs, mut groups, mut calls, mut results) =
        (CANDIDATES, CANDIDATES, CANDIDATES, CANDIDATES);
    let attempts: BTreeSet<String> = candidates(db,
        "SELECT attempt_id FROM output_item INDEXED BY output_item_node WHERE node_id=?1 ORDER BY rowid LIMIT ?2",
        node, &mut outputs, &mut exact, |r| r.get(0))?.into_iter().collect();
    let mut keys: BTreeSet<(String, String)> = candidates(db,
        "SELECT group_id,call_id FROM tool_result INDEXED BY tool_result_node WHERE node_id=?1 ORDER BY rowid LIMIT ?2",
        node, &mut results, &mut exact, |r| Ok((r.get(0)?, r.get(1)?)))?.into_iter().collect();
    for attempt in attempts {
        for group in candidates::<String>(
            db,
            "SELECT id FROM tool_group INDEXED BY history_tool_group_attempt WHERE attempt_id=?1 ORDER BY rowid LIMIT ?2",
            &attempt,
            &mut groups,
            &mut exact,
            |r| r.get(0),
        )? {
            for call in candidates(
                db,
                "SELECT call_id FROM tool_call WHERE group_id=?1 ORDER BY ordinal LIMIT ?2",
                &group,
                &mut calls,
                &mut exact,
                |r| r.get(0),
            )? {
                keys.insert((group.clone(), call));
            }
        }
    }
    // Probe only sampled keys; sort only the validated, bounded sample. A request
    // node is reported only when its uniqueness can be proved from the sample.
    let mut stmt = db.prepare_cached("SELECT c.group_id,c.call_id,c.ordinal,c.state,s.id,s.state,s.cwd,substr(COALESCE(json_extract(s.shell_json,'$.command'),''),1,81),tr.node_id,NULL,CASE WHEN json_valid(s.shell_json) THEN CASE WHEN json_type(s.shell_json,'$.description')='text' THEN substr(json_extract(s.shell_json,'$.description'),1,80) END END,g.rowid,g.attempt_id FROM tool_call c JOIN tool_group g ON g.id=c.group_id JOIN turn u ON u.id=g.turn_id JOIN shell_execution s ON s.id=c.operation_id AND s.turn_id=g.turn_id AND s.owner_run_id=u.run_id LEFT JOIN tool_result tr ON tr.group_id=c.group_id AND tr.call_id=c.call_id WHERE c.group_id=?1 AND c.call_id=?2").map_err(sql_error)?;
    let mut found = Vec::new();
    for (group, call) in keys {
        if let Some(link) = stmt
            .query_row(params![group, call], |r| {
                Ok((
                    r.get::<_, i64>(11)?,
                    r.get::<_, String>(12)?,
                    execution_link_row(r)?,
                ))
            })
            .optional()
            .map_err(sql_error)?
        {
            found.push(link);
        }
    }
    found.sort_unstable_by_key(|(group_order, _, link)| (*group_order, link.call_ordinal));
    let count = found.len().min(LINK_LIMIT + 1);
    exact &= found.len() <= LINK_LIMIT;
    let mut rows = Vec::new();
    for (_, attempt, mut link) in found.into_iter().take(LINK_LIMIT) {
        let mut complete = true;
        let mut remaining = CANDIDATES;
        let nodes: BTreeSet<String> = candidates::<Option<String>>(
            db,
            "SELECT node_id FROM output_item WHERE attempt_id=?1 ORDER BY ordinal LIMIT ?2",
            &attempt,
            &mut remaining,
            &mut complete,
            |r| r.get(0),
        )?
        .into_iter()
        .flatten()
        .collect();
        if complete && nodes.len() == 1 {
            link.request_node_id = nodes.into_iter().next();
        }
        rows.push(link);
    }
    Ok(Links {
        rows,
        count: count as u64,
        exact,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn window_bounds_candidate_rows_even_when_candidates_have_no_shell() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/a','','','t');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','t');
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','t');
            INSERT INTO node(id,kind,created_at) VALUES('n','assistant_response','t');
            INSERT INTO model_attempt(id,turn_id,kind,state,thread_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,input_manifest_json,started_at) VALUES('a','u','conversation','completed','t',0,0,'','',0,'{}','t');
            INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id) VALUES('a',0,'function_call','completed','{}','n');
            INSERT INTO tool_group VALUES('g','a','u','completed','t');
            WITH RECURSIVE seq(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM seq WHERE i<9999)
            INSERT INTO tool_call(group_id, call_id, ordinal, tool_name, arguments_json, state) SELECT 'g','c'||i,i,'shell','{}','completed' FROM seq;").unwrap();
        let ticks = Arc::new(AtomicUsize::new(0));
        let observed = ticks.clone();
        db.progress_handler(
            100,
            Some(move || {
                observed.fetch_add(100, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
        let found = scan(&db, "n").unwrap();
        assert_eq!((found.rows.len(), found.count, found.exact), (0, 0, false));
        let cost = ticks.load(Ordering::Relaxed);
        assert!(cost > 0 && cost < 12_000, "bounded VM work: {cost}");
        assert!(scan(&db, "unrelated").unwrap().exact);
    }
}
