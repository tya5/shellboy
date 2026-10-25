//! Durable embedding claim lifecycle on the existing DB worker.
use super::*;

pub(super) fn claim(db: &mut Connection, keys: IndexKeys<'_>) -> Result<ClaimResult, StorageError> {
    let tx = db.transaction()?;
    if meta(&tx, &keys.pause())?.is_some() {
        return Ok(ClaimResult::Idle);
    }
    let candidate:Option<(String,String,i64)>=tx.query_row("SELECT j.id,j.space_id,j.attempts FROM embedding_job j JOIN index_space s ON s.id=j.space_id WHERE j.state='pending' AND s.state IN ('active','candidate') AND s.splitter_version=?1 AND (j.next_attempt_at IS NULL OR j.next_attempt_at<=?2) ORDER BY j.next_attempt_at,j.id LIMIT 1",params![keys.splitter(),now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((job, space, attempts)) = candidate {
        let mut work: Work = json_meta(&tx, &IndexKeys::input(&job))?.ok_or_else(serde_error)?;
        if !witness_valid(&tx, &work.witness, &work.body_kind)?
            || !group_current(&tx, keys, &work.group)?
        {
            tx.execute(
                "UPDATE embedding_job SET state='failed',error_json=?2 WHERE id=?1",
                params![job, json!({"kind":"source_changed"}).to_string()],
            )?;
            tx.execute(
                "DELETE FROM app_meta WHERE key=?1",
                [IndexKeys::input(&job)],
            )?;
            tx.commit()?;
            return Ok(ClaimResult::Progress);
        }
        work.attempts = attempts as usize + 1;
        work.state = "running".into();
        tx.execute(
            "UPDATE embedding_job SET state='running',attempts=attempts+1 WHERE id=?1",
            [&job],
        )?;
        let space = load_space(&tx, &space)?.ok_or_else(serde_error)?;
        tx.commit()?;
        return Ok(ClaimResult::Ready(Box::new(Claimed {
            job_id: Some(job),
            bootstrap_key: None,
            space: Some(space),
            work,
        })));
    }
    let mut statement =
        tx.prepare("SELECT key,value FROM app_meta WHERE key LIKE ?1 AND json_extract(value,'$.state')='pending' ORDER BY key LIMIT 128")?;
    let rows = statement
        .query_map([keys.bootstrap_pattern()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut progressed = false;
    for (bootstrap, input) in rows {
        let mut work: Work = decode_json(&input)?;
        if work.state != "pending" || work.next_at.as_ref().is_some_and(|next| next > &now()) {
            continue;
        }
        if !witness_valid(&tx, &work.witness, &work.body_kind)?
            || !group_current(&tx, keys, &work.group)?
        {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
            progressed = true;
            continue;
        }
        if let Some(space) = space_for_group(&tx, keys, &work.group)? {
            queue_job(&tx, &space, &work)?;
            tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
            progressed = true;
            continue;
        }
        work.attempts += 1;
        work.state = "running".into();
        update_json_meta(&tx, &bootstrap, &work)?;
        tx.commit()?;
        return Ok(ClaimResult::Ready(Box::new(Claimed {
            job_id: None,
            bootstrap_key: Some(bootstrap),
            space: None,
            work,
        })));
    }
    tx.commit()?;
    Ok(if progressed {
        ClaimResult::Progress
    } else {
        ClaimResult::Idle
    })
}
pub(super) fn claim_valid(db: &Connection, claimed: &Claimed) -> Result<bool, StorageError> {
    if let Some(job) = &claimed.job_id {
        return db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM embedding_job WHERE id=?1 AND state='running')",
                [job],
                |row| row.get(0),
            )
            .map_err(StorageError::from);
    }
    if let Some(key) = &claimed.bootstrap_key {
        return Ok(meta(db, key)?
            .and_then(|value| decode_json::<Work>(&value).ok())
            .is_some_and(|current| {
                current.state == "running"
                    && current.witness.node_id == claimed.work.witness.node_id
                    && current.witness.revision == claimed.work.witness.revision
                    && current.body_kind == claimed.work.body_kind
                    && current.segment.start == claimed.work.segment.start
                    && current.segment.text == claimed.work.segment.text
            }));
    }
    Ok(false)
}
pub(super) fn discard_claim(
    tx: &Transaction<'_>,
    claimed: &Claimed,
    reason: &str,
) -> Result<(), StorageError> {
    if let Some(job) = &claimed.job_id {
        tx.execute(
            "UPDATE embedding_job SET state='failed',error_json=?2 WHERE id=?1",
            params![
                job,
                json!({"kind":"source_changed","message":reason}).to_string()
            ],
        )?;
        tx.execute("DELETE FROM app_meta WHERE key=?1", [IndexKeys::input(job)])?;
    }
    if let Some(bootstrap) = &claimed.bootstrap_key {
        tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
    }
    Ok(())
}

/// A space transition does not revoke a valid, already-authorized source input.
/// Keep it failed/observable until explicit selection/retry; never auto-rebill.
pub(super) fn defer_claim(
    tx: &Transaction<'_>,
    claimed: &Claimed,
    space_state: &str,
) -> Result<(), StorageError> {
    let error = json!({"kind":"space_unavailable","space_state":space_state,"message":"Space changed before adoption; select/retry explicitly. The API request may already have been billed."});
    if let Some(job) = &claimed.job_id {
        tx.execute("UPDATE embedding_job SET state='failed',error_json=?2,next_attempt_at=NULL WHERE id=?1",params![job,error.to_string()])?;
    }
    if let Some(bootstrap) = &claimed.bootstrap_key {
        let mut work = claimed.work.clone();
        work.state = "failed".into();
        work.error = Some(error);
        work.next_at = None;
        update_json_meta(tx, bootstrap, &work)?;
    }
    Ok(())
}
