//! Finite history mark/provenance/sweep steps in the caller transaction.
use super::*;

pub(super) fn history_step(db: &mut Connection, job: &str) -> Result<HistoryStep> {
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
        nodes_tombstoned: 0,
        bytes_collected: 0,
        restarted: false,
        complete: state == "done",
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
    if !session
        || parsed
            .as_ref()
            .is_none_or(|progress| progress.horizon.is_none())
    {
        reset(&tx, job, current)?;
        tx.commit().map_err(sql_error)?;
        report.stage = Stage::Roots.as_str().into();
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
    match progress.stage {
        Stage::Roots => roots_step(&tx, job, &mut progress, &mut report)?,

        Stage::Provenance => provenance_step(&tx, job, &mut progress, &mut report)?,

        Stage::Sweep => sweep_step(&tx, job, &mut progress, &mut report, root)?,
    }
    report.stage = progress.stage.as_str().into();
    if !report.complete {
        tx.execute(
            "UPDATE gc_job SET cursor_json=?2 WHERE id=?1",
            params![job, serde_json::to_string(&progress).unwrap()],
        )
        .map_err(sql_error)?;
    }
    tx.commit().map_err(sql_error)?;
    Ok(report)
}

fn roots_step(
    tx: &Transaction<'_>,
    job: &str,
    progress: &mut Progress,
    report: &mut HistoryStep,
) -> Result<()> {
    let horizon = progress.horizon.expect("validated collection horizon");
    let rows = tx
                .prepare("SELECT rowid,id,turn_id FROM node WHERE rowid>?1 AND rowid<=?3 ORDER BY rowid LIMIT ?2")
                .map_err(sql_error)?
                .query_map(params![progress.last_rowid, BATCH as i64, horizon], |row| {
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
        let root: bool = root_query
            .query_row([node], |row| row.get(0))
            .map_err(sql_error)?;
        if root {
            mark(tx, job, node)?;
        }
        progress.last_rowid = *rowid;
    }
    report.scanned = rows.len() as u64;
    if rows.len() < BATCH {
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
) -> Result<()> {
    let pending=tx.prepare("SELECT node_id,next_source_rowid FROM shellboy_gc_mark WHERE job_id=?1 AND expanded=0 ORDER BY node_id LIMIT ?2").map_err(sql_error)?.query_map(params![job,BATCH as i64],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?))).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    if pending.is_empty() {
        progress.stage = Stage::Sweep;
        progress.last_rowid = 0;
    }
    let mut budget = BATCH;
    for (node, cursor) in pending {
        if budget == 0 {
            break;
        }
        let sources=tx.prepare("SELECT s.rowid,s.source_node_id FROM replacement_source s JOIN replacement r ON r.id=s.replacement_id WHERE r.output_node_id=?1 AND s.rowid>?2 ORDER BY s.rowid LIMIT ?3").map_err(sql_error)?.query_map(params![node,cursor,budget as i64],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?))).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
        for (_, source) in &sources {
            mark(tx, job, source)?;
        }
        let complete = sources.len() < budget;
        let last = sources.last().map_or(cursor, |row| row.0);
        tx.execute("UPDATE shellboy_gc_mark SET expanded=?3,next_source_rowid=?4 WHERE job_id=?1 AND node_id=?2",params![job,node,complete,last]).map_err(sql_error)?;
        report.scanned += 1;
        budget = budget.saturating_sub(sources.len().max(1));
    }
    Ok(())
}

fn tombstone_node(
    tx: &Transaction<'_>,
    node: &str,
    kind: &str,
    turn: &Option<String>,
) -> Result<()> {
    crate::index::delete_source_vectors_tx(tx, node)?;
    tx.execute("UPDATE node SET payload_json=NULL,payload_bytes=0,payload_revision=payload_revision+1,deleted_reason='thread_deleted_unreachable' WHERE id=?1",[node]).map_err(sql_error)?;
    // Once the payload is a tombstone, search can no longer expose
    // it; release deleted-owner evidence in this same transaction.
    release_deleted_ownership(tx, node)?;
    tx.execute(
        "UPDATE output_item SET payload_json=?2 WHERE node_id=?1",
        params![
            node,
            json!({"type":"shellboy_tombstone","reason":"thread_deleted_unreachable","node_id":node})
                .to_string()
        ],
    )
    .map_err(sql_error)?;
    if kind == "incomplete_response" {
        tx.execute("UPDATE model_attempt SET error_json=CASE WHEN json_valid(error_json) THEN json_remove(error_json,'$.partial_text','$.completed_items','$.raw_response') ELSE error_json END WHERE turn_id=?1",[turn]).map_err(sql_error)?;
    }
    Ok(())
}

fn sweep_step(
    tx: &Transaction<'_>,
    job: &str,
    progress: &mut Progress,
    report: &mut HistoryStep,
    root: i64,
) -> Result<()> {
    let horizon = progress.horizon.expect("validated collection horizon");
    // Pins can change without a persistent root revision. Keep the
    // sweep cursor advancing and validate all remaining candidates
    // against current roots/provenance in this same transaction.
    let late=tx.prepare("SELECT DISTINCT p.resource_id FROM runtime_pin p WHERE p.resource_kind IN ('node','history_node','source_node') AND NOT EXISTS(SELECT 1 FROM shellboy_gc_mark m WHERE m.job_id=?1 AND m.node_id=p.resource_id) LIMIT 128").map_err(sql_error)?.query_map([job],|r|r.get::<_,String>(0)).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    if !late.is_empty() {
        progress.revalidate_roots = true;
        for node in late {
            mark(tx, job, &node)?;
        }
    }
    let rows=tx.prepare("SELECT n.rowid,n.id,COALESCE(length(CAST(n.payload_json AS BLOB)),0),n.kind,n.turn_id FROM node n WHERE n.rowid>?1 AND n.rowid<=?3 ORDER BY n.rowid LIMIT ?2").map_err(sql_error)?.query_map(params![progress.last_rowid,BATCH as i64,horizon],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)? as u64,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?))).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    let reachability_sql = format!(
        "WITH RECURSIVE consumers(id) AS(SELECT ?1 UNION SELECT r.output_node_id FROM consumers c JOIN replacement_source s ON s.source_node_id=c.id JOIN replacement r ON r.id=s.replacement_id) SELECT EXISTS(SELECT 1 FROM consumers c JOIN node n ON n.id=c.id WHERE {ROOT_PREDICATE})"
    );
    let mut current_roots = if progress.revalidate_roots {
        Some(tx.prepare_cached(&reachability_sql).map_err(sql_error)?)
    } else {
        None
    };
    for (rowid, node, bytes, kind, turn) in &rows {
        progress.last_rowid = *rowid;
        report.scanned += 1;
        if *bytes == 0 {
            release_deleted_ownership(tx, node)?;
            continue;
        }
        let protected:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM shellboy_gc_mark WHERE job_id=?1 AND node_id=?2) OR EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind IN ('node','history_node','source_node') AND resource_id=?2)",params![job,node],|row|row.get(0)).map_err(sql_error)?;
        if protected {
            continue;
        }
        if let Some(query) = &mut current_roots
            && query
                .query_row([node], |row| row.get::<_, bool>(0))
                .map_err(sql_error)?
        {
            // Newly adopted roots/provenance remain conservatively
            // protected for this job, even if later removed again.
            mark(tx, job, node)?;
            continue;
        }
        tombstone_node(tx, node, kind, turn)?;
        report.nodes_tombstoned += 1;
        report.bytes_collected += bytes;
    }
    if rows.len() < BATCH {
        // Jobs already present at the start share this horizon; later
        // deletions must get their own fresh mark/horizon pass.
        tx.execute("UPDATE gc_job SET state='done',cursor_json=NULL WHERE kind='history' AND state IN ('pending','running') AND root_revision<=?1",[root]).map_err(sql_error)?;
        tx.execute("DELETE FROM shellboy_gc_mark WHERE job_id=?1", [job])
            .map_err(sql_error)?;
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
    fn durable_stage_bytes_and_legacy_progress_defaults_are_unchanged() {
        for (stage, literal) in [
            (Stage::Roots, "roots"),
            (Stage::Provenance, "provenance"),
            (Stage::Sweep, "sweep"),
        ] {
            assert_eq!(
                serde_json::to_string(&stage).unwrap(),
                format!("\"{literal}\"")
            );
            let decoded: Stage = serde_json::from_str(&format!("\"{literal}\"")).unwrap();
            assert_eq!(decoded.as_str(), literal);
        }
        let legacy: Progress =
            serde_json::from_str(r#"{"stage":"sweep","last_rowid":123}"#).unwrap();
        assert!(legacy.horizon.is_none() && !legacy.revalidate_roots);
        assert_eq!(legacy.last_rowid, 123);
        assert!(
            serde_json::from_str::<Progress>(r#"{"stage":"future","last_rowid":123}"#).is_err()
        );
        // history_step keeps its existing .ok() -> reset path for this unknown
        // stage; no partially trusted destructive cursor is introduced.
    }
}
