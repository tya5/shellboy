//! Durable embedding claim lifecycle on the existing DB worker.
use super::*;

/// Consume one paused item atomically, including delayed and failed work.
pub(super) fn discard_paused(
    db: &mut Connection,
    profile: &str,
    family: Option<&str>,
) -> Result<Option<bool>, StorageError> {
    let tx = db.transaction()?;
    let keys = IndexKeys::new(profile);
    let paused = meta(&tx, &keys.pause())?.is_some()
        || family
            .map(|key| meta(&tx, &IndexKeys::new(key).pause()))
            .transpose()?
            .flatten()
            .is_some();
    if !paused {
        return Ok(None);
    }
    for prefix in [keys.auto(""), keys.bootstrap("")] {
        let upper = format!("{}/", &prefix[..prefix.len() - 1]);
        let key: Option<String> = tx.query_row("SELECT key FROM app_meta WHERE key>=?1 AND key<?2 AND json_extract(value,'$.state') IN ('pending','failed') AND COALESCE(json_extract(value,'$.recovery'),0) != 1 ORDER BY key LIMIT 1", params![prefix, upper], |r| r.get(0)).optional()?;
        if let Some(key) = key {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
            tx.commit()?;
            return Ok(Some(true));
        }
    }
    let job: Option<String> = tx.query_row("SELECT j.id FROM index_space s JOIN embedding_job j ON j.space_id=s.id WHERE s.splitter_version=?1 AND j.state IN ('pending','failed') AND COALESCE(CASE WHEN json_valid(j.authorized_watermark) THEN json_extract(j.authorized_watermark,'$.recovery') END,0) != 1 LIMIT 1", [keys.splitter()], |r| r.get(0)).optional()?;
    if let Some(job) = job {
        tx.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [IndexKeys::input(&job)],
        )?;
        tx.execute("DELETE FROM embedding_job WHERE id=?1", [job])?;
        tx.commit()?;
        return Ok(Some(true));
    }
    Ok(Some(false))
}

pub(super) fn singleton_fallback(work: &Work) -> bool {
    work.error
        .as_ref()
        .is_some_and(|error| error.get("batch_fallback").is_some())
}

/// Ready pending jobs of one space in claim order, each a `jobs_ready` seek
/// (storage design #171): unscheduled (NULL) first in insertion order, then
/// due ones by `next_attempt_at`. ?3 admits automatic-history work.
const READY_UNSCHEDULED: &str = "SELECT id,attempts FROM embedding_job INDEXED BY jobs_ready WHERE space_id=?1 AND state='pending' AND next_attempt_at IS NULL AND ?2 IS NOT NULL AND (?3 OR COALESCE(json_extract(authorized_watermark,'$.authorization'),'') NOT LIKE 'new-finalized%') ORDER BY rowid LIMIT ?4";
const READY_DUE: &str = "SELECT id,attempts FROM embedding_job INDEXED BY jobs_ready WHERE space_id=?1 AND state='pending' AND next_attempt_at<=?2 AND (?3 OR COALESCE(json_extract(authorized_watermark,'$.authorization'),'') NOT LIKE 'new-finalized%') ORDER BY next_attempt_at,rowid LIMIT ?4";

/// The first ready job across this splitter's active/candidate spaces.
fn next_ready(
    tx: &Transaction<'_>,
    splitter: &str,
    cutoff: &str,
    auto_history: bool,
) -> Result<Option<(String, String, i64)>, StorageError> {
    let spaces = tx
        .prepare("SELECT id FROM index_space WHERE state IN ('active','candidate') AND splitter_version=?1 ORDER BY id")?
        .query_map([splitter], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for sql in [READY_UNSCHEDULED, READY_DUE] {
        let mut best: Option<(String, String, i64, Option<String>)> = None;
        for space in &spaces {
            let row = tx
                .query_row(
                    &sql.replace("SELECT id,attempts", "SELECT id,attempts,next_attempt_at"),
                    params![space, cutoff, auto_history, 1],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((job, attempts, at)) = row
                && best.as_ref().is_none_or(|b| at < b.3)
            {
                best = Some((job, space.clone(), attempts, at));
            }
        }
        if let Some((job, space, attempts, _)) = best {
            return Ok(Some((job, space, attempts)));
        }
    }
    Ok(None)
}

pub(super) fn claim(
    db: &mut Connection,
    keys: IndexKeys<'_>,
    max_inputs: usize,
    max_chars: usize,
    auto_history: bool,
    family: Option<&str>,
) -> Result<ClaimResult, StorageError> {
    let tx = db.transaction()?;
    if meta(&tx, &keys.pause())?.is_some()
        || family
            .map(|key| meta(&tx, &IndexKeys::new(key).pause()))
            .transpose()?
            .flatten()
            .is_some()
    {
        return Ok(ClaimResult::Idle);
    }
    // One due cutoff for this serialized claim snapshot, never a fill window.
    let cutoff = now();
    let candidate = next_ready(&tx, &keys.splitter(), &cutoff, auto_history)?;
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
            let mut rows = Vec::new();
            for sql in [READY_UNSCHEDULED, READY_DUE] {
                let remaining = 128 - rows.len() as i64;
                if remaining <= 0 {
                    break;
                }
                rows.extend(
                    tx.prepare(sql)?
                        .query_map(params![space_id, cutoff, true, remaining], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
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
    // Byte-exact key range seek of this profile's bootstrap rows (#171).
    let (start, end) = keys.bootstrap_range();
    let mut statement = tx.prepare("SELECT key,value FROM app_meta WHERE key>=?1 AND key<?3 AND json_extract(value,'$.state')='pending' AND (?2 OR json_extract(value,'$.authorization') NOT LIKE 'new-finalized%') ORDER BY key LIMIT 128")?;
    let rows = statement
        .query_map(params![start, auto_history, end], |row| {
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
            "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key>=?1 AND key<?3 AND json_extract(value,'$.state')='running' AND json_extract(value,'$.group')=?2)",
            params![start,work.group,end], |row| row.get(0),
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
) -> Result<bool, StorageError> {
    let mut changed = false;
    if let Some(job) = &claimed.job_id {
        changed |= tx.execute(
            "UPDATE embedding_job SET state='failed',error_json=?2 WHERE id=?1 AND (state<>'failed' OR error_json IS NOT ?2)",
            params![
                job,
                json!({"kind":"source_changed","message":reason}).to_string()
            ],
        )? > 0;
        changed |= tx.execute("DELETE FROM app_meta WHERE key=?1", [IndexKeys::input(job)])? > 0;
    }
    if let Some(bootstrap) = &claimed.bootstrap_key {
        changed |= tx.execute("DELETE FROM app_meta WHERE key=?1", [bootstrap])? > 0;
    }
    Ok(changed)
}

/// A space transition does not revoke a valid, already-authorized source input.
/// Keep it failed/observable until explicit selection/retry; never auto-rebill.
pub(super) fn defer_claim(
    tx: &Transaction<'_>,
    claimed: &Claimed,
    space_state: &str,
) -> Result<bool, StorageError> {
    let error = json!({"kind":"space_unavailable","space_state":space_state,"message":"Space changed before adoption; select/retry explicitly. The API request may already have been billed."});
    let mut changed = false;
    if let Some(job) = &claimed.job_id {
        changed |= tx.execute("UPDATE embedding_job SET state='failed',error_json=?2,next_attempt_at=NULL WHERE id=?1 AND (state<>'failed' OR error_json IS NOT ?2 OR next_attempt_at IS NOT NULL)",params![job,error.to_string()])? > 0;
    }
    if let Some(bootstrap) = &claimed.bootstrap_key {
        let mut work = claimed.work.clone();
        work.state = "failed".into();
        work.error = Some(error);
        work.next_at = None;
        let encoded = encode_json(&work)?;
        if meta(tx, bootstrap)?.is_some_and(|old| old != encoded) {
            changed |= tx.execute(
                "UPDATE app_meta SET value=?2 WHERE key=?1",
                params![bootstrap, encoded],
            )? > 0;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paused_consumer_discards_each_queue_and_keeps_running() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE index_space(id TEXT,splitter_version TEXT); CREATE TABLE embedding_job(id TEXT,space_id TEXT,state TEXT,next_attempt_at TEXT,authorized_watermark TEXT);").unwrap();
        let keys = IndexKeys::new("p");
        db.execute("INSERT INTO app_meta VALUES(?1,'{}')", [keys.pause()])
            .unwrap();
        for (key, value) in [
            (keys.auto("a"), r#"{"state":"failed"}"#),
            (
                keys.bootstrap("b"),
                r#"{"state":"pending","next_at":"2999"}"#,
            ),
            (keys.auto("running"), r#"{"state":"running"}"#),
            (IndexKeys::input("job"), "{}"),
        ] {
            db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![key, value])
                .unwrap();
        }
        db.execute("INSERT INTO index_space VALUES('s',?1)", [keys.splitter()])
            .unwrap();
        db.execute(
            "INSERT INTO embedding_job VALUES('job','s','pending','2999',NULL)",
            [],
        )
        .unwrap();
        for _ in 0..3 {
            assert_eq!(discard_paused(&mut db, "p", None).unwrap(), Some(true));
        }
        assert_eq!(discard_paused(&mut db, "p", None).unwrap(), Some(false));
        assert!(meta(&db, &keys.auto("running")).unwrap().is_some());
        db.execute(
            "UPDATE app_meta SET value=json_set(value,'$.state','pending') WHERE key=?1",
            [keys.auto("running")],
        )
        .unwrap();
        assert_eq!(discard_paused(&mut db, "p", None).unwrap(), Some(true));
        assert!(meta(&db, &keys.auto("running")).unwrap().is_none());
        assert!(meta(&db, &IndexKeys::input("job")).unwrap().is_none());
        assert_eq!(
            db.query_row("SELECT count(*) FROM embedding_job", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
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
        let ClaimResult::Ready(claims) = claim(&mut db, keys, 4, 64000, true, None).unwrap() else {
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
