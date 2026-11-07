//! Collection of index spaces. Automatic maintenance and `storage collect` take
//! only what holds no paid work: discarded int8 candidates (free copies) and
//! inactive spaces without vectors. A retained space goes only through `index remove`.
use super::*;
use crate::protocol::IndexRemoveArgs;

/// All guards are checked only when committing removal, never while reclaiming.
fn removal_blocker(db: &Connection, id: &str) -> Result<Option<&'static str>, StorageError> {
    let state: String = db.query_row("SELECT state FROM index_space WHERE id=?1", [id], |r| {
        r.get(0)
    })?;
    if state == "removing" {
        return Ok(None);
    }
    if meta(db, IndexKeys::ACTIVE_SPACE)?.as_deref() == Some(id) {
        return Ok(Some("it is the active index"));
    }
    if !matches!(state.as_str(), "inactive" | "discarded") {
        return Ok(Some("it is not inactive"));
    }
    let running: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM embedding_job WHERE space_id=?1 AND state='running')",
        [id],
        |r| r.get(0),
    )?;
    if running {
        return Ok(Some("an embedding request for it is running"));
    }
    if shrink::involves(db, id)? {
        return Ok(Some("a conversion is using it"));
    }
    let rebuilding: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_meta g JOIN app_meta r ON r.key='index.rebuild.'||substr(g.key,length('index.group.')+1,instr(substr(g.key,length('index.group.')+1),'.')-1) WHERE g.value=?1 AND g.key>='index.group.' AND g.key<'index.group/' AND g.key='index.group.'||substr(r.key,length('index.rebuild.')+1)||'.'||r.value)",
        [id],|r|r.get(0))?;
    Ok(rebuilding.then_some("a rebuild is using it"))
}

/// Compatibility for callers collecting one bounded unit. Production maintenance owns the cursor.
#[cfg(test)]
pub(crate) fn collect_inactive_spaces(db: &mut Connection) -> Result<u64, StorageError> {
    let mut after = String::new();
    let mut removed = 0;
    loop {
        let (n, more) = reclaim_step(db, 32, &mut after)?;
        removed += n;
        if !more {
            return Ok(removed);
        }
    }
}

pub(crate) fn reclaim_step(
    db: &mut Connection,
    limit: i64,
    after: &mut String,
) -> Result<(u64, bool), StorageError> {
    use rusqlite::OptionalExtension;
    let tx = db.transaction()?;
    let candidate = tx
        .query_row(
            "SELECT id,vec_table_name FROM index_space WHERE state='removing' ORDER BY id LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((space, table)) = candidate else {
        // Legacy discarded conversion copies and empty inactive spaces still need a decision.
        // Keyset paging prevents one protected space from starving later candidates.
        let row = tx.query_row(
            "SELECT min(id) FROM (SELECT id FROM (SELECT id FROM index_space WHERE state='discarded' AND id>?1 ORDER BY id LIMIT 1) UNION ALL SELECT id FROM (SELECT id FROM index_space WHERE state='inactive' AND id>?1 ORDER BY id LIMIT 1)) HAVING min(id) IS NOT NULL",
            [&*after], |row| row.get::<_,String>(0),
        ).optional()?;
        let Some(id) = row else {
            return Ok((0, false));
        };
        *after = id.clone();
        if removal_blocker(&tx, &id)?.is_none() {
            let empty_or_discarded: bool = tx.query_row(
                "SELECT state='discarded' OR NOT EXISTS(SELECT 1 FROM embedding_chunk WHERE space_id=?1) FROM index_space WHERE id=?1",
                [&id], |r|r.get(0))?;
            if empty_or_discarded {
                tx.execute("UPDATE index_space SET state='removing' WHERE id=?1", [&id])?;
            }
        }
        tx.commit()?;
        return Ok((0, true));
    };
    valid_table(&table)?;
    // An empty retained index must become non-selectable before any cleanup.
    tx.execute(
        "UPDATE index_space SET state='removing' WHERE id=?1 AND state='inactive'",
        [&space],
    )?;
    let chunks = tx.prepare("SELECT id,vec_row_id FROM embedding_chunk WHERE space_id=?1 ORDER BY vec_row_id LIMIT ?2")?
        .query_map(params![space, limit], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !chunks.is_empty() {
        for (id, row) in chunks {
            tx.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [row])?;
            tx.execute(
                "DELETE FROM app_meta WHERE key=?1",
                [IndexKeys::preview(&id)],
            )?;
            tx.execute("DELETE FROM embedding_chunk WHERE id=?1", [&id])?;
        }
        tx.commit()?;
        return Ok((0, true));
    }
    let jobs = tx
        .prepare("SELECT id FROM embedding_job WHERE space_id=?1 LIMIT ?2")?
        .query_map(params![space, limit], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !jobs.is_empty() {
        for id in jobs {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [IndexKeys::input(&id)])?;
            tx.execute("DELETE FROM embedding_job WHERE id=?1", [&id])?;
        }
        tx.commit()?;
        return Ok((0, true));
    }
    let keys = tx
        .prepare("SELECT key FROM app_meta WHERE value=?1 AND key>='index.group.' AND key<'index.group/' LIMIT ?2")?
        .query_map(params![space, limit], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !keys.is_empty() {
        for key in keys {
            tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
        }
        tx.commit()?;
        return Ok((0, true));
    }
    // Also drain any orphan vec rows before dropping the now-empty table.
    let rows = tx
        .prepare(&format!("SELECT rowid FROM {table} LIMIT ?1"))?
        .query_map([limit], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !rows.is_empty() {
        for row in rows {
            tx.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [row])?;
        }
        tx.commit()?;
        return Ok((0, true));
    }
    tx.execute_batch(&format!("DROP TABLE {table}"))?;
    for key in [
        IndexKeys::query_profile(&space),
        dimensions::space_key(&space),
        IndexKeys::converted_from(&space),
        IndexKeys::partial(&space),
    ] {
        tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
    }
    tx.execute("DELETE FROM index_space WHERE id=?1", [&space])?;
    tx.commit()?;
    Ok((1, true))
}

/// The facts of removing one space. Read-only; `blocked_by` says why it cannot go.
fn removal_preview(db: &Connection, id: &str) -> Result<Value, StorageError> {
    let space = load_space(db, id)?.ok_or_else(|| {
        StorageError::from(err(ErrorCode::TargetUnavailable, "Unknown index space"))
    })?;
    let segments: i64 = db.query_row(
        "SELECT count(*) FROM embedding_chunk WHERE space_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let unfinished: i64 = db.query_row(
        "SELECT count(*) FROM embedding_job WHERE space_id=?1 AND state IN ('pending','failed','split')",
        [id],
        |row| row.get(0),
    )?;
    let blocked_by = removal_blocker(db, id)?;
    Ok(json!({
        "space_id":space.id,
        "connection":space.profile,
        "model":space.model,
        "dimension":space.dimension,
        "element_type":if space.int8_scale.is_some() {"int8"} else {"float32"},
        "state":space.state,
        "partial":meta(db, &IndexKeys::partial(id))?.is_some(),
        "segments":segments,
        "unfinished_jobs_deleted":unfinished,
        "vector_payload_bytes_freed":segments as u64 * space.dimension as u64 * space.element_bytes(),
        "removable":blocked_by.is_none(),
        "blocked_by":blocked_by,
        "effects":"Commits removal from search. Maintenance subsequently reclaims vectors and unfinished jobs with their saved inputs; conversation history is not touched. Recreating it means paid embedding of every segment. Reclaimed pages can be reused; the database file shrinks only with `storage vacuum`.",
    }))
}

impl IndexService {
    /// Remove one named inactive space. Nothing else ever deletes a retained one.
    pub async fn remove_space(&self, args: IndexRemoveArgs) -> Result<Value, ApiError> {
        crate::protocol::Command::IndexRemove(args.clone()).validate()?;
        let dry_run = args.dry_run;
        let result = self
            .database
            .call(move |db| {
                let mut preview = removal_preview(db, &args.space_id)?;
                preview["dry_run"] = json!(dry_run);
                if dry_run {
                    return Ok(preview);
                }
                if let Some(reason) = preview["blocked_by"].as_str() {
                    return Err(StorageError::from(err(
                        ErrorCode::InvalidState,
                        format!("Cannot remove this index: {reason}"),
                    )));
                }
                let tx = db.transaction()?;
                // Recheck the protection predicates in the transaction that commits the decision.
                if let Some(reason) = removal_blocker(&tx, &args.space_id)? {
                    return Err(StorageError::from(err(
                        ErrorCode::InvalidState,
                        format!("Cannot remove this index: {reason}"),
                    )));
                }
                tx.execute(
                    "UPDATE index_space SET state='removing' WHERE id=?1",
                    [&args.space_id],
                )?;
                tx.commit()?;
                preview["removed"] = json!(true);
                preview["reclaim_pending"] = json!(true);
                preview["state"] = json!("removing");
                preview["unfinished_jobs_pending"] = preview["unfinished_jobs_deleted"].clone();
                preview["unfinished_jobs_deleted"] = json!(0);
                preview["vector_payload_bytes_pending"] =
                    preview["vector_payload_bytes_freed"].clone();
                preview["vector_payload_bytes_freed"] = json!(0);
                Ok(preview)
            })
            .await
            .map_err(db_error)?;
        if !dry_run {
            self.publish_saved(true);
            self.spaces_changed();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_reclaim_resumes_after_reopen_and_ignores_new_protection_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bounded.db");
        let db = crate::storage::DbWorker::open(&path).await.unwrap();
        db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('source','message','now','{}',2);
                CREATE VIRTUAL TABLE shellboy_vec_0123456789abcdef0123456789abcdef USING vec0(embedding float[2]);
                INSERT INTO index_space VALUES('removed','connection','model',2,'splitter','removing','shellboy_vec_0123456789abcdef0123456789abcdef',NULL);")?;
            for n in 0..257 {
                let id=format!("job-{n:04}");
                tx.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state) VALUES(?1,'removed','source',0,'message',?1,0,1,'failed')",[&id])?;
                tx.execute("INSERT INTO shellboy_vec_0123456789abcdef0123456789abcdef(rowid,embedding) VALUES(?1,'[1,2]')",[n])?;
                tx.execute("INSERT INTO embedding_chunk VALUES(?1,?1,'removed','source',0,0,1,?1,?2)",params![id,n])?;
                tx.execute("INSERT INTO app_meta VALUES(?1,'synthetic input')",[IndexKeys::input(&id)])?;
                tx.execute("INSERT INTO app_meta VALUES(?1,'synthetic preview')",[IndexKeys::preview(&id)])?;
            }
            tx.commit()?;
            // Exact row bound, including vector and preview deletion in the same unit.
            let mut after=String::new();
            assert_eq!(reclaim_step(db,7,&mut after)?,(0,true));
            let left:i64=db.query_row("SELECT count(*) FROM embedding_chunk",[],|r|r.get(0))?;
            assert_eq!(left,250);
            let vectors:i64=db.query_row("SELECT count(*) FROM shellboy_vec_0123456789abcdef0123456789abcdef",[],|r|r.get(0))?;
            assert_eq!(vectors,250);
            // New unrelated/rebuild metadata cannot revive a committed removal.
            db.execute_batch("INSERT INTO app_meta VALUES('index.rebuild.unrelated','later'),('index.group.unrelated.later','removed');")?;
            Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
        let db = crate::storage::DbWorker::open(path).await.unwrap();
        db.call(|db| {
            let mut finished=0;
            let mut steps=0;
            loop {
                // No persisted or process-local cursor is needed to resume a committed decision.
                let (n,more)=reclaim_step(db,7,&mut String::new())?;
                finished+=n; steps+=1;
                let chunks:i64=db.query_row("SELECT count(*) FROM embedding_chunk",[],|r|r.get(0))?;
                let charged:i64=db.query_row("SELECT logical_bytes FROM quota_counter WHERE category='logical.index'",[],|r|r.get(0))?;
                let metadata:i64=db.query_row("SELECT COALESCE(sum(octet_length(value)),0) FROM app_meta WHERE key LIKE 'index.%'",[],|r|r.get(0))?;
                assert_eq!(charged,chunks*8+metadata);
                if !more {break;}
                assert!(steps<200);
            }
            assert_eq!(finished,1);
            for table in ["index_space","embedding_chunk","embedding_job"] {
                let count:i64=db.query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get(0))?; assert_eq!(count,0);
            }
            let saved:i64=db.query_row("SELECT count(*) FROM app_meta WHERE key>='index.input.' AND key<'index.input/' OR key>='index.preview.' AND key<'index.preview/'",[],|r|r.get(0))?;
            assert_eq!(saved,0);
            Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn bounded_reclaim_chunk_failure_rolls_back_one_unit_and_lookup_uses_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let db = crate::storage::DbWorker::open(temp.path().join("failure.db"))
            .await
            .unwrap();
        db.call(|db| {
            db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('s','message','now','{}',2);
                CREATE VIRTUAL TABLE shellboy_vec_1123456789abcdef0123456789abcdef USING vec0(embedding int8[2]);
                INSERT INTO shellboy_vec_1123456789abcdef0123456789abcdef(rowid,embedding) VALUES(1,vec_int8(X'0102'));
                INSERT INTO index_space VALUES('r','p','m',2,'splitter','removing','shellboy_vec_1123456789abcdef0123456789abcdef',1);
                INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state) VALUES('j','r','s',0,'message','hash',0,1,'done');
                INSERT INTO embedding_chunk VALUES('c','j','r','s',0,0,1,'hash',1);
                INSERT INTO app_meta VALUES('index.preview.c','preview');
                CREATE TRIGGER fail_chunk BEFORE DELETE ON embedding_chunk BEGIN SELECT RAISE(ABORT,'synthetic reclaim failure'); END;")?;
            assert!(reclaim_step(db,1,&mut String::new()).is_err());
            let vectors:i64=db.query_row("SELECT count(*) FROM shellboy_vec_1123456789abcdef0123456789abcdef",[],|r|r.get(0))?;
            assert_eq!(vectors,1);
            assert!(meta(db,"index.preview.c")?.is_some());
            db.execute_batch("DROP TRIGGER fail_chunk;")?;
            for (sql,index) in [
                ("EXPLAIN QUERY PLAN SELECT id FROM index_space WHERE state='removing' ORDER BY id LIMIT 1","index_reclaim_state"),
                ("EXPLAIN QUERY PLAN SELECT key FROM app_meta WHERE value='r' AND key>='index.group.' AND key<'index.group/' LIMIT 7","index_reclaim_group_value"),
            ] {
                let lines=db.prepare(sql)?.query_map([],|r|r.get::<_,String>(3))?.collect::<rusqlite::Result<Vec<_>>>()?;
                assert!(lines.iter().any(|s|s.contains(index)),"{lines:?}");
            }
            let mut finished=0;
            loop {let (n,more)=reclaim_step(db,1,&mut String::new())?;finished+=n;if !more {break;}}
            assert_eq!(finished,1);
            Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
    }
}
