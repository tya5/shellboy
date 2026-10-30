//! Durable embedding claim lifecycle on the existing DB worker.
use super::*;

pub(super) fn singleton_fallback(work: &Work) -> bool {
    work.error
        .as_ref()
        .is_some_and(|error| error.get("batch_fallback").is_some())
}

pub(super) fn claim(
    db: &mut Connection,
    keys: IndexKeys<'_>,
    max_inputs: usize,
    max_chars: usize,
    auto_history: bool,
) -> Result<ClaimResult, StorageError> {
    let tx = db.transaction()?;
    if meta(&tx, &keys.pause())?.is_some() {
        return Ok(ClaimResult::Idle);
    }
    // One due cutoff for this serialized claim snapshot, never a fill window.
    let cutoff = now();
    let candidate: Option<(String, String, i64)> = tx.query_row(
        "SELECT j.id,j.space_id,j.attempts FROM embedding_job j JOIN index_space s ON s.id=j.space_id WHERE j.state='pending' AND s.state IN ('active','candidate') AND s.splitter_version=?1 AND (?3 OR COALESCE(json_extract(j.authorized_watermark,'$.authorization'),'') NOT LIKE 'new-finalized%') AND (j.next_attempt_at IS NULL OR j.next_attempt_at<=?2) ORDER BY j.next_attempt_at,j.id LIMIT 1",
        params![keys.splitter(),cutoff,auto_history], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional()?;
    if let Some((job, space_id, attempts)) = candidate {
        let mut work: Work = json_meta(&tx, &IndexKeys::input(&job))?.ok_or_else(serde_error)?;
        let space = load_space(&tx, &space_id)?.ok_or_else(serde_error)?;
        let first = Claimed {
            requested_dimensions: space.requested_dimensions,
            job_id: Some(job.clone()),
            bootstrap_key: None,
            space: Some(space.clone()),
            work: work.clone(),
        };
        if !witness_valid(&tx, &work.witness, &work.body_kind)?
            || !group_current(&tx, keys, &work.group)?
        {
            discard_claim(&tx, &first, "Source or index group changed before claim")?;
            tx.commit()?;
            return Ok(ClaimResult::Progress);
        }
        if super::dimensions::candidate_stopped(&tx, keys, &work.group)? {
            defer_claim(&tx, &first, "candidate_validation_failed")?;
            tx.commit()?;
            return Ok(ClaimResult::Progress);
        }
        let individual = singleton_fallback(&work);
        let mut characters = work.segment.text.chars().count();
        work.attempts = attempts as usize + 1;
        work.state = "running".into();
        tx.execute(
            "UPDATE embedding_job SET state='running',attempts=attempts+1 WHERE id=?1",
            [&job],
        )?;
        let group = work.group.clone();
        let mut claims = vec![Claimed { work, ..first }];
        if max_inputs > 1 && !individual && characters <= max_chars {
            // Bounded read of one space. Invalid rows may make a smaller batch;
            // no provider heuristics or unbounded scan tries to fill the cap.
            let rows = tx.prepare("SELECT id,attempts FROM embedding_job WHERE space_id=?1 AND state='pending' AND (next_attempt_at IS NULL OR next_attempt_at<=?2) ORDER BY next_attempt_at,id LIMIT 128")?
                .query_map(params![space_id,cutoff], |row| Ok((row.get::<_,String>(0)?, row.get::<_,i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (job, attempts) in rows {
                if claims.len() >= max_inputs {
                    break;
                }
                let mut work: Work =
                    json_meta(&tx, &IndexKeys::input(&job))?.ok_or_else(serde_error)?;
                if !auto_history && work.authorization.starts_with("new-finalized") {
                    continue;
                }
                let next = Claimed {
                    requested_dimensions: space.requested_dimensions,
                    job_id: Some(job.clone()),
                    bootstrap_key: None,
                    space: Some(space.clone()),
                    work: work.clone(),
                };
                if !witness_valid(&tx, &work.witness, &work.body_kind)?
                    || !group_current(&tx, keys, &work.group)?
                {
                    discard_claim(&tx, &next, "Source or index group changed before claim")?;
                    continue;
                }
                if work.group != group || singleton_fallback(&work) {
                    break;
                }
                let count = work.segment.text.chars().count();
                if count > max_chars.saturating_sub(characters) {
                    break;
                }
                characters += count;
                work.attempts = attempts as usize + 1;
                work.state = "running".into();
                tx.execute(
                    "UPDATE embedding_job SET state='running',attempts=attempts+1 WHERE id=?1",
                    [&job],
                )?;
                claims.push(Claimed { work, ..next });
            }
        }
        tx.commit()?;
        return Ok(ClaimResult::Ready(claims));
    }
    let mut statement = tx.prepare("SELECT key,value FROM app_meta WHERE key LIKE ?1 AND json_extract(value,'$.state')='pending' AND (?2 OR json_extract(value,'$.authorization') NOT LIKE 'new-finalized%') ORDER BY key LIMIT 128")?;
    let rows = statement
        .query_map(params![keys.bootstrap_pattern(), auto_history], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let mut progressed = false;
    for (bootstrap, input) in rows {
        let mut work: Work = decode_json(&input)?;
        if work.state != "pending" || work.next_at.as_ref().is_some_and(|next| next > &cutoff) {
            continue;
        }
        if !witness_valid(&tx, &work.witness, &work.body_kind)?
            || !group_current(&tx, keys, &work.group)?
        {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
            progressed = true;
            continue;
        }
        if super::dimensions::candidate_stopped(&tx, keys, &work.group)? {
            let claimed = Claimed {
                requested_dimensions: None,
                job_id: None,
                bootstrap_key: Some(bootstrap),
                space: None,
                work,
            };
            defer_claim(&tx, &claimed, "candidate_validation_failed")?;
            progressed = true;
            continue;
        }
        if let Some(space) = space_for_group(&tx, keys, &work.group)? {
            queue_job(&tx, &space, &work)?;
            tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])?;
            progressed = true;
            continue;
        }
        // A first vector is real work. Until it settles, never spend another
        // unknown-space request in this group, even with multiple workers.
        let discovering: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key LIKE ?1 AND json_extract(value,'$.state')='running' AND json_extract(value,'$.group')=?2)",
            params![keys.bootstrap_pattern(),work.group], |row| row.get(0),
        )?;
        if discovering {
            continue;
        }
        work.attempts += 1;
        work.state = "running".into();
        update_json_meta(&tx, &bootstrap, &work)?;
        let requested_dimensions =
            json_meta::<dimensions::CandidateMode>(&tx, &dimensions::candidate_key(keys))?
                .filter(|mode| mode.group == work.group)
                .map(|mode| mode.dimensions);
        tx.commit()?;
        return Ok(ClaimResult::Ready(vec![Claimed {
            requested_dimensions,
            job_id: None,
            bootstrap_key: Some(bootstrap),
            space: None,
            work,
        }]));
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_claim_keeps_request_mode_when_a_later_rebuild_replaces_metadata() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE index_space(id TEXT,splitter_version TEXT,state TEXT);
            CREATE TABLE embedding_job(id TEXT,space_id TEXT,attempts INTEGER,state TEXT,next_attempt_at TEXT,authorized_watermark TEXT);
            CREATE TABLE node(id TEXT,payload_revision INTEGER,body_hash TEXT,payload_json TEXT,deleted_reason TEXT);
            INSERT INTO node VALUES('source',1,NULL,'{}',NULL);").unwrap();
        let keys = IndexKeys::new("profile");
        let tx = db.transaction().unwrap();
        set_meta(&tx, &keys.rebuild(), "first").unwrap();
        set_json_meta(
            &tx,
            &dimensions::candidate_key(keys),
            &dimensions::CandidateMode {
                group: "first".into(),
                dimensions: 3,
                error: None,
                paused_at: None,
            },
        )
        .unwrap();
        let work = Work::pending(
            Witness {
                node_id: "source".into(),
                revision: 1,
                body_hash: None,
                raw: None,
            },
            "user".into(),
            Segment {
                start: 0,
                end: 1,
                text: "x".into(),
            },
            "first",
            "explicit-rebuild",
        );
        set_json_meta(&tx, &keys.bootstrap("input"), &work).unwrap();
        tx.commit().unwrap();
        let ClaimResult::Ready(claims) = claim(&mut db, keys, 4, 64000, true).unwrap() else {
            panic!("expected claim")
        };
        let tx = db.transaction().unwrap();
        set_meta(&tx, &keys.rebuild(), "second").unwrap();
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [dimensions::candidate_key(keys)],
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            claims[0].requested_dimensions,
            Some(3),
            "superseded work must not silently send native dimensions"
        );
        assert!(
            !group_current(&db, keys, &claims[0].work.group).unwrap(),
            "late adoption remains fenced"
        );
    }
}
