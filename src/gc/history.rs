//! Finite history mark/provenance/sweep steps in the caller transaction.
use super::*;
use crate::control::wait::wakes::{Key, Wakes};

pub(super) fn history_step(
    db: &mut Connection,
    job: &str,
    batch: usize,
    candidate: &std::sync::Mutex<Option<String>>,
    keep: Option<&str>,
    wakes: &Wakes,
) -> Result<HistoryStep> {
    work_tables(db)?;
    let tx = db.transaction().map_err(sql_error)?;
    let (root, body, state): (i64, Option<String>, String) = tx
        .query_row(
            "SELECT root_revision,cursor_json,state FROM gc_job WHERE id=?1 AND kind='history'",
            [job],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable, "history GC job not found"))?;
    let mut report = HistoryStep {
        stage: state.clone(),
        root_revision: root,
        scanned: 0,
        candidates: 0,
        nodes_tombstoned: 0,
        kept_over_budget: 0,
        kept_receipts: 0,
        kept_marks: 0,
        bytes_collected: 0,
        restarted: false,
        complete: state == "done",
        deferred: false,
        reply_wake_threads: Vec::new(),
    };
    if report.complete {
        return Ok(report);
    }
    let current = revision(&tx)?;
    let session: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM shellboy_gc_session WHERE job_id=?1)",
            [job],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let parsed = body.and_then(|body| serde_json::from_str::<Progress>(&body).ok());
    if !session || parsed.is_none() {
        reset(&tx, job, current)?;
        tx.commit().map_err(sql_error)?;
        report.stage = Stage::Reset.as_str().into();
        report.root_revision = current;
        report.restarted = true;
        return Ok(report);
    }
    let mut progress = parsed.unwrap();
    if current != root {
        // Keep conservative marks. Every still-unmarked destructive candidate
        // now needs a current reachability proof in the same write transaction.
        progress.revalidate_roots = true;
        report.root_revision = current;
    }
    // Keep the last candidate identified until this unit commits: a budget
    // interrupt may arrive while writing the cursor, after its work finished.
    let mut keys = Vec::new();
    match progress.stage {
        Stage::Reset => clean_marks(&tx, job, &mut progress, &mut report, batch, candidate, keep)?,
        Stage::Receipts => receipts_step(
            &tx,
            &mut progress,
            &mut report,
            batch,
            candidate,
            keep,
            &mut keys,
        )?,
        Stage::Instructions => {
            instructions_step(&tx, &mut progress, &mut report, batch, candidate, keep)?
        }
        Stage::Roots => roots_step(&tx, job, &mut progress, &mut report, batch, candidate, keep)?,

        Stage::Provenance => {
            provenance_step(&tx, job, &mut progress, &mut report, batch, candidate, keep)?
        }

        Stage::Sweep => sweep_step(&tx, job, &mut progress, &mut report, batch, candidate, keep)?,
        Stage::Cleanup => {
            clean_marks(&tx, job, &mut progress, &mut report, batch, candidate, keep)?
        }
        Stage::Finish => finish_step(&tx, job, &mut progress, &mut report, root, batch)?,
    }
    report.stage = progress.stage.as_str().into();
    report.kept_over_budget = progress.kept_count;
    report.kept_receipts = progress.kept_receipts;
    report.kept_marks = progress.kept_marks;
    if !report.complete {
        tx.execute(
            "UPDATE gc_job SET cursor_json=?2 WHERE id=?1",
            params![job, serde_json::to_string(&progress).unwrap()],
        )
        .map_err(sql_error)?;
    }
    tx.commit().map_err(sql_error)?;
    wakes.publish(keys);
    Ok(report)
}

// Keyset the entire finite receipt horizon: LIMIT bounds examination as well as
// deletion, even when most receipts remain owned. Recheck inside the DELETE.
const RECEIPT_ELIGIBLE: &str = "EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NOT NULL) AND COALESCE(json_extract(COALESCE(receipt.result_json,'{}'),'$.reply.state'),'') NOT IN ('pending','awaiting_own_ask') AND NOT EXISTS(SELECT 1 FROM thread sender WHERE sender.id=json_extract(receipt.sender_json,'$.thread_id') AND json_extract(receipt.sender_json,'$.kind')='model' AND sender.deleted_at IS NULL)";
fn receipts_step(
    tx: &Transaction<'_>,
    progress: &mut Progress,
    report: &mut HistoryStep,
    batch: usize,
    candidate: &std::sync::Mutex<Option<String>>,
    keep: Option<&str>,
    keys: &mut Vec<Key>,
) -> Result<()> {
    let rows=tx.prepare("SELECT rowid,id,octet_length(sender_json)+COALESCE(octet_length(result_json),0) FROM receipt WHERE rowid>?1 AND rowid<=?2 ORDER BY rowid LIMIT ?3").map_err(sql_error)?
        .query_map(params![progress.last_rowid,progress.receipt_horizon,batch as i64],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?))) .map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    // Only otherwise-deletable receipts reach this guard, so the candidate
    // receipt is not itself a live root. A surviving consumer keeps its facts.
    let reachable = crate::history::reachability::live_root_sql("receipt.message_node_id");
    let sql = format!(
        "DELETE FROM receipt WHERE id=?1 AND {RECEIPT_ELIGIBLE} AND NOT ({reachable}) RETURNING message_node_id"
    );
    let at = Utc::now().to_rfc3339();
    report.candidates = rows.len() as u64;
    let mut wake_threads = std::collections::BTreeSet::new();
    for (rowid, id, bytes) in &rows {
        *candidate.lock().expect("history candidate") = Some(id.clone());
        // Persist recipient-deleted outcome before any payload/receipt sweep.
        // The reply state guard makes restart/replay idempotent.
        if let Some(thread) = crate::control::reply::deleted_recipient_receipt_tx(tx, id, &at, keys)
            .map_err(sql_error)?
        {
            wake_threads.insert(thread);
        }
        if keep == Some(id.as_str()) {
            progress.kept_receipts = progress.kept_receipts.saturating_add(1);
            progress.last_rowid = *rowid;
            continue;
        }
        crate::control::reply::deleted_sender_receipt_tx(tx, id, &at, keys).map_err(sql_error)?;
        if let Some(message) = tx
            .query_row(&sql, [id], |r| r.get::<_, Option<String>>(0))
            .optional()
            .map_err(sql_error)?
        {
            keys.extend(message.map(Key::Reply));
            report.bytes_collected += (*bytes).max(0) as u64;
        }
        progress.last_rowid = *rowid;
    }
    report.reply_wake_threads = wake_threads.into_iter().collect();
    report.scanned = rows.len() as u64;
    if rows.len() < batch {
        progress.stage = Stage::Instructions;
        progress.last_rowid = 0;
    }
    Ok(())
}

/// Turns with an unchanged observation share one immutable instruction
/// snapshot, so a row is garbage only when no Turn references it. The DELETE
/// rechecks that in this transaction; the finite rowid horizon bounds the pass.
fn instructions_step(
    tx: &Transaction<'_>,
    progress: &mut Progress,
    report: &mut HistoryStep,
    batch: usize,
    candidate: &std::sync::Mutex<Option<String>>,
    keep: Option<&str>,
) -> Result<()> {
    let rows = tx.prepare("SELECT rowid,id,octet_length(body) FROM instruction_snapshot WHERE rowid>?1 AND rowid<=?2 ORDER BY rowid LIMIT ?3").map_err(sql_error)?
        .query_map(params![progress.last_rowid, progress.instruction_horizon, batch as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))).map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    let mut delete = tx
        .prepare_cached("DELETE FROM instruction_snapshot WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM turn WHERE instruction_snapshot_id=?1)")
        .map_err(sql_error)?;
    for (rowid, id, bytes) in &rows {
        // A single over-budget delete is retained for this pass.
        if keep != Some(id.as_str()) {
            *candidate.lock().expect("history candidate") = Some(id.clone());
            if delete.execute([id]).map_err(sql_error)? != 0 {
                report.bytes_collected += (*bytes).max(0) as u64;
            }
        }
        progress.last_rowid = *rowid;
    }
    report.scanned = rows.len() as u64;
    if rows.len() < batch {
        progress.stage = Stage::Roots;
        progress.last_rowid = 0;
    }
    Ok(())
}

fn roots_step(
    tx: &Transaction<'_>,
    job: &str,
    progress: &mut Progress,
    report: &mut HistoryStep,
    batch: usize,
    candidate: &std::sync::Mutex<Option<String>>,
    keep: Option<&str>,
) -> Result<()> {
    let horizon = progress.horizon;
    let rows = tx
                .prepare("SELECT rowid,id,turn_id FROM node WHERE rowid>?1 AND rowid<=?3 ORDER BY rowid LIMIT ?2")
                .map_err(sql_error)?
                .query_map(params![progress.last_rowid, batch as i64, horizon], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
    let root_sql = format!("SELECT {ROOT_PREDICATE} FROM node n WHERE n.id=?1");
    let mut root_query = tx.prepare_cached(&root_sql).map_err(sql_error)?;
    for (rowid, node, _) in &rows {
        let root: bool = if keep == Some(node.as_str()) {
            progress.kept_count = progress.kept_count.saturating_add(1);
            if progress.kept_ids.len() < 8 && node.len() <= 256 {
                progress.kept_ids.push(node.clone());
            }
            true
        } else {
            *candidate.lock().expect("history candidate") = Some(node.clone());
            root_query
                .query_row([node], |row| row.get(0))
                .map_err(sql_error)?
        };
        if root {
            mark(tx, job, node)?;
        }
        progress.last_rowid = *rowid;
    }
    report.scanned = rows.len() as u64;
    if rows.len() < batch {
        progress.stage = Stage::Provenance;
        progress.last_rowid = 0;
    }
    Ok(())
}

fn provenance_step(
    tx: &Transaction<'_>,
    job: &str,
    progress: &mut Progress,
    report: &mut HistoryStep,
    batch: usize,
    candidate: &std::sync::Mutex<Option<String>>,
    keep: Option<&str>,
) -> Result<()> {
    let pending = tx.prepare("SELECT node_id,next_replacement_rowid,next_ordinal FROM shellboy_gc_mark WHERE job_id=?1 AND expanded=0 ORDER BY node_id LIMIT ?2").map_err(sql_error)?
        .query_map(params![job,batch as i64], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    if pending.is_empty() {
        begin_sweep(progress);
    }
    let mut budget = batch;
    for (node, replacement_cursor, ordinal) in pending {
        if budget == 0 {
            break;
        }
        if keep == Some(node.as_str()) {
            // An interrupted expansion leaves its protected sources unknown.
            progress.retain_all = true;
            begin_sweep(progress);
            break;
        }
        *candidate.lock().expect("history candidate") = Some(node.clone());
        let replacement: Option<(i64,String)> = tx.query_row(
            "SELECT rowid,id FROM replacement WHERE output_node_id=?1 AND rowid>?2 AND state<>'pruned' ORDER BY rowid LIMIT 1",
            params![node,replacement_cursor], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional().map_err(sql_error)?;
        let Some((replacement_rowid, replacement_id)) = replacement else {
            tx.execute(
                "UPDATE shellboy_gc_mark SET expanded=1 WHERE job_id=?1 AND node_id=?2",
                params![job, node],
            )
            .map_err(sql_error)?;
            budget -= 1;
            report.scanned += 1;
            continue;
        };
        let sources = tx.prepare("SELECT ordinal,source_node_id FROM replacement_source WHERE replacement_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3").map_err(sql_error)?
            .query_map(params![replacement_id,ordinal,budget as i64], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
        for (_, source) in &sources {
            mark(tx, job, source)?;
        }
        let exhausted = sources.len() < budget;
        let next_replacement = if exhausted {
            replacement_rowid
        } else {
            replacement_cursor
        };
        let next_ordinal = if exhausted {
            -1
        } else {
            sources.last().unwrap().0
        };
        tx.execute("UPDATE shellboy_gc_mark SET next_replacement_rowid=?3,next_ordinal=?4 WHERE job_id=?1 AND node_id=?2",params![job,node,next_replacement,next_ordinal]).map_err(sql_error)?;
        report.scanned += sources.len().max(1) as u64;
        budget = budget.saturating_sub(sources.len().max(1));
    }
    Ok(())
}

fn begin_sweep(progress: &mut Progress) {
    // The conservative full-pass scan counts each retained node once, including
    // any node already counted during root marking.
    if progress.retain_all {
        progress.kept_count = 0;
        progress.kept_ids.clear();
    }
    progress.stage = Stage::Sweep;
    progress.last_rowid = 0;
}

fn tombstone_node(
    tx: &Transaction<'_>,
    node: &str,
    kind: &str,
    turn: &Option<String>,
) -> Result<()> {
    crate::index::retire_index_source_tx(tx, node, false)?;
    // A node released by `thread prune` records that reason (#235); the rest keep the thread-deletion reason.
    let reason = if crate::history::reachability::released_by_prune(tx, node).map_err(sql_error)? {
        crate::history::prune::PRUNED_REASON
    } else {
        "thread_deleted_unreachable"
    };
    tx.execute("UPDATE node SET payload_json=NULL,payload_bytes=0,payload_revision=payload_revision+1,deleted_reason=?2 WHERE id=?1",params![node,reason]).map_err(sql_error)?;
    // Once the payload is a tombstone, search can no longer expose
    // it; release deleted-owner evidence in this same transaction.
    release_deleted_ownership(tx, node)?;
    tx.execute(
        "UPDATE output_item SET payload_json=?2 WHERE node_id=?1",
        params![
            node,
            json!({"type":"shellboy_tombstone","reason":reason,"node_id":node}).to_string()
        ],
    )
    .map_err(sql_error)?;
    if kind == "incomplete_response" {
        tx.execute("UPDATE model_attempt SET error_json=CASE WHEN json_valid(error_json) THEN json_remove(error_json,'$.partial_text','$.partial_reasoning','$.completed_items','$.raw_response','$.received_failure') ELSE error_json END WHERE turn_id=?1 AND state IN ('completed','failed','incomplete','cancelled')",[turn]).map_err(sql_error)?;
    }
    Ok(())
}

fn sweep_step(
    tx: &Transaction<'_>,
    job: &str,
    progress: &mut Progress,
    report: &mut HistoryStep,
    batch: usize,
    candidate: &std::sync::Mutex<Option<String>>,
    keep: Option<&str>,
) -> Result<()> {
    let horizon = progress.horizon;
    // Runtime pins can change without changing the root revision. Revalidate
    // every destructive candidate in this transaction, rather than scanning
    // an unbounded set of pins before every batch.
    progress.revalidate_roots = true;
    let rows=tx.prepare("SELECT n.rowid,n.id,COALESCE(length(CAST(n.payload_json AS BLOB)),0),n.kind,n.turn_id FROM node n WHERE n.rowid>?1 AND n.rowid<=?3 ORDER BY n.rowid LIMIT ?2").map_err(sql_error)?.query_map(params![progress.last_rowid,batch as i64,horizon],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)? as u64,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?))).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    let reachability_sql = crate::history::reachability::live_root_sql("?1");
    let mut current_roots = if progress.revalidate_roots {
        Some(tx.prepare_cached(&reachability_sql).map_err(sql_error)?)
    } else {
        None
    };
    for (rowid, node, bytes, kind, turn) in &rows {
        progress.last_rowid = *rowid;
        report.scanned += 1;
        if keep == Some(node.as_str()) || (progress.retain_all && *bytes > 0) {
            progress.kept_count = progress.kept_count.saturating_add(1);
            if progress.kept_ids.len() < 8 && node.len() <= 256 {
                progress.kept_ids.push(node.clone());
            }
            continue;
        }
        if *bytes == 0 {
            *candidate.lock().expect("history candidate") = Some(node.clone());
            release_deleted_ownership(tx, node)?;
            continue;
        }
        let protected:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM shellboy_gc_mark WHERE job_id=?1 AND node_id=?2) OR EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind IN ('node','history_node','source_node') AND resource_id=?2)",params![job,node],|row|row.get(0)).map_err(sql_error)?;
        if protected {
            continue;
        }
        let reachable = if let Some(query) = &mut current_roots {
            *candidate.lock().expect("history candidate") = Some(node.clone());
            query
                .query_row([node], |row| row.get::<_, bool>(0))
                .map_err(sql_error)?
        } else {
            false
        };
        if reachable {
            // Newly adopted roots/provenance remain conservatively
            // protected for this job, even if later removed again.
            mark(tx, job, node)?;
            continue;
        }
        // A single node's derived index cleanup can exceed the worker budget.
        // Keep the node if this transaction is interrupted before it commits.
        *candidate.lock().expect("history candidate") = Some(node.clone());
        tombstone_node(tx, node, kind, turn)?;
        report.nodes_tombstoned += 1;
        report.bytes_collected += bytes;
    }
    if rows.len() < batch {
        progress.stage = Stage::Cleanup;
        progress.last_rowid = 0;
    }
    Ok(())
}

fn clean_marks(
    tx: &Transaction<'_>,
    job: &str,
    progress: &mut Progress,
    report: &mut HistoryStep,
    batch: usize,
    candidate: &std::sync::Mutex<Option<String>>,
    keep: Option<&str>,
) -> Result<()> {
    let resetting = matches!(progress.stage, Stage::Reset);
    // Scan by rowid even for other jobs so examination itself is bounded.
    let rows = tx.prepare("SELECT rowid,job_id,node_id FROM shellboy_gc_mark WHERE rowid>?1 ORDER BY rowid LIMIT ?2").map_err(sql_error)?
        .query_map(params![progress.last_rowid,batch as i64], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    for (rowid, owner, node) in &rows {
        if owner == job {
            if keep == Some(node.as_str()) {
                progress.kept_marks = progress.kept_marks.saturating_add(1);
                // Stale expanded marks cannot be used as evidence after reset.
                if resetting {
                    progress.retain_all = true;
                }
            } else {
                *candidate.lock().expect("history candidate") = Some(node.clone());
                tx.execute("DELETE FROM shellboy_gc_mark WHERE rowid=?1", [rowid])
                    .map_err(sql_error)?;
            }
        }
        progress.last_rowid = *rowid;
    }
    report.scanned = rows.len() as u64;
    if rows.len() < batch {
        progress.stage = if resetting {
            Stage::Receipts
        } else {
            Stage::Finish
        };
        progress.last_rowid = 0;
    }
    Ok(())
}

fn finish_step(
    tx: &Transaction<'_>,
    job: &str,
    progress: &mut Progress,
    report: &mut HistoryStep,
    root: i64,
    batch: usize,
) -> Result<()> {
    // Do not mark this job done until the finite set of coalesced jobs has
    // been closed; the current job carries the durable finish cursor.
    let rows = tx.prepare("SELECT rowid,id,kind,state,root_revision FROM gc_job WHERE rowid>?1 AND rowid<=?2 ORDER BY rowid LIMIT ?3").map_err(sql_error)?
        .query_map(params![progress.last_rowid,progress.job_horizon,batch as i64], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?))).map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    for (rowid, id, kind, state, revision) in &rows {
        if id != job
            && kind == "history"
            && (state == "pending" || state == "running")
            && *revision <= root
        {
            tx.execute(
                "UPDATE gc_job SET state='done',cursor_json=NULL WHERE id=?1",
                [id],
            )
            .map_err(sql_error)?;
        }
        progress.last_rowid = *rowid;
    }
    report.scanned = rows.len() as u64;
    if rows.len() < batch {
        tx.execute("UPDATE gc_job SET state='done',cursor_json=NULL,error_json=?2 WHERE id=?1", params![job,serde_json::json!({"kept_over_budget":progress.kept_count,"kept_receipts":progress.kept_receipts,"kept_marks":progress.kept_marks,"kept_ids":progress.kept_ids}).to_string()]).map_err(sql_error)?;
        tx.execute("DELETE FROM shellboy_gc_session WHERE job_id=?1", [job])
            .map_err(sql_error)?;
        report.complete = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fork_replay_keeps_the_admission_receipt_until_its_body_loses_the_last_root() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('source','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','source','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','source','r','completed','now');").unwrap();
        let tx = db.transaction().unwrap();
        crate::history::append_node(
            &tx,
            "source",
            "received",
            Some("u"),
            "user",
            &json!({"role":"user","content":[{"type":"input_text","text":"received body"}]}),
            "now",
        )
        .unwrap();
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,path_admission) VALUES('receipt','operation',?1,'source','received','applied','now','admitted')", [json!({"kind":"external_cli","label":"fixture sender"}).to_string()]).unwrap();
        crate::history::fork_thread(
            &tx,
            &crate::protocol::ThreadForkArgs {
                agents: None,
                thread_id: "source".into(),
                node_id: "received".into(),
                cwd: None,
            },
            "fork",
            "now",
        )
        .unwrap();
        tx.commit().unwrap();

        let cache = crate::control::replay::ReplayCache::new(1024 * 1024);
        let before = cache.load(&db, "fork").unwrap().materialize();
        assert_eq!(before.len(), 2);
        assert!(
            before[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("fixture sender")
        );
        assert_eq!(before[1]["content"][0]["text"], "received body");
        let collect = |db: &mut Connection, job: &str| {
            db.execute(
                "INSERT INTO gc_job VALUES(?1,'history',0,NULL,'pending',NULL)",
                [job],
            )
            .unwrap();
            let candidate = std::sync::Mutex::new(None);
            let wakes = Wakes::default();
            assert!((0..64).any(|_| {
                history_step(db, job, BATCH, &candidate, None, &wakes)
                    .unwrap()
                    .complete
            }));
        };
        let receipt_exists = |db: &Connection| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM receipt WHERE id='receipt')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        };
        db.execute(
            "UPDATE thread SET deleted_at='now',cursor_node_id=NULL WHERE id='source'",
            [],
        )
        .unwrap();
        collect(&mut db, "protected");
        assert!(receipt_exists(&db));
        let fresh = crate::control::replay::ReplayCache::new(0);
        assert_eq!(fresh.load(&db, "fork").unwrap().materialize(), before);
        assert_eq!(cache.load(&db, "fork").unwrap().materialize(), before);

        db.execute(
            "UPDATE thread SET deleted_at='now',cursor_node_id=NULL WHERE id='fork'",
            [],
        )
        .unwrap();
        collect(&mut db, "released");
        assert!(!receipt_exists(&db));
    }

    #[test]
    fn durable_stage_bytes_and_current_progress_fields_are_required() {
        for (stage, literal) in [
            (Stage::Reset, "reset"),
            (Stage::Receipts, "receipts"),
            (Stage::Instructions, "instructions"),
            (Stage::Roots, "roots"),
            (Stage::Provenance, "provenance"),
            (Stage::Sweep, "sweep"),
            (Stage::Cleanup, "cleanup"),
            (Stage::Finish, "finish"),
        ] {
            assert_eq!(
                serde_json::to_string(&stage).unwrap(),
                format!("\"{literal}\"")
            );
            let decoded: Stage = serde_json::from_str(&format!("\"{literal}\"")).unwrap();
            assert_eq!(decoded.as_str(), literal);
        }
        // A cursor written before #225 still decodes; new counters default.
        let current =
            r#"{"stage":"sweep","last_rowid":123,"horizon":456,"revalidate_roots":false}"#;
        let progress: Progress = serde_json::from_str(current).unwrap();
        assert_eq!(progress.horizon, 456);
        assert!(!progress.revalidate_roots && !progress.retain_all);
        assert_eq!(progress.last_rowid, 123);
        assert_eq!(progress.receipt_horizon, 0);
        assert_eq!(progress.job_horizon, 0);
        assert_eq!(
            (
                progress.kept_count,
                progress.kept_receipts,
                progress.kept_marks
            ),
            (0, 0, 0)
        );
        assert!(progress.kept_ids.is_empty());
        assert!(serde_json::from_str::<Progress>(r#"{"stage":"sweep","last_rowid":123}"#).is_err());
        assert!(
            serde_json::from_str::<Progress>(r#"{"stage":"future","last_rowid":123}"#).is_err()
        );
        // history_step keeps its existing .ok() -> reset path for these; no
        // partially trusted destructive cursor is introduced.
    }
}
