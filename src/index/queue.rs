//! Authorized fixed-input job admission and durable queue keys.
use super::*;

pub(super) fn bootstrap_key(keys: IndexKeys<'_>, work: &Work) -> String {
    let digest = hash(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}\0{}",
            work.group,
            work.witness.node_id,
            work.witness.revision,
            work.body_kind,
            work.segment.start,
            work.segment.end,
            hash(work.segment.text.as_bytes())
        )
        .as_bytes(),
    );
    keys.bootstrap(&digest)
}
pub(super) fn queue_job(
    tx: &Transaction<'_>,
    space: &Space,
    work: &Work,
) -> Result<bool, StorageError> {
    let embedding_job_sql = "INSERT OR IGNORE INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,attempts,authorized_watermark)VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',0,?9)";
    if !witness_valid(tx, &work.witness, &work.body_kind)? {
        return Ok(false);
    }
    let job = id();
    let source_hash = hash(work.segment.text.as_bytes());
    let watermark =
        json!({"witness":work.witness,"group":work.group,"authorization":work.authorization});
    let inserted = tx.execute(
        embedding_job_sql,
        params![
            job,
            space.id,
            work.witness.node_id,
            work.witness.revision,
            work.body_kind,
            source_hash,
            work.segment.start as i64,
            work.segment.end as i64,
            watermark.to_string(),
        ],
    )?;
    if inserted > 0 {
        set_json_meta(tx, &IndexKeys::input(&job), work)?;
    }
    if inserted == 0
        && !work.authorization.starts_with("new-finalized")
        && let Some(job) = job_id_for(tx, space, work)?
        && let Some(mut prior) = json_meta::<Work>(tx, &IndexKeys::input(&job))?
        && prior.state == "pending"
        && prior.authorization.starts_with("new-finalized")
    {
        prior.authorization.clone_from(&work.authorization);
        set_json_meta(tx, &IndexKeys::input(&job), &prior)?;
        tx.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.authorization',?2) WHERE id=?1 AND state='pending'", params![job,work.authorization])?;
    }
    Ok(inserted > 0)
}
pub(super) fn job_id_for(
    db: &Connection,
    space: &Space,
    work: &Work,
) -> Result<Option<String>, StorageError> {
    let embedding_job_sql = "SELECT id FROM embedding_job WHERE space_id=?1 AND source_node_id=?2 AND source_revision=?3 AND body_kind=?4 AND source_hash=?5 AND span_start=?6 AND span_end=?7";
    db.query_row(
        embedding_job_sql,
        params![
            space.id,
            work.witness.node_id,
            work.witness.revision,
            work.body_kind,
            hash(work.segment.text.as_bytes()),
            work.segment.start as i64,
            work.segment.end as i64
        ],
        |r| r.get(0),
    )
    .optional()
    .map_err(StorageError::from)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct AutoAdmission {
    pub(super) node_id: String,
    pub(super) state: String,
    pub(super) error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Work {
    pub(super) witness: Witness,
    pub(super) body_kind: String,
    pub(super) segment: Segment,
    pub(super) group: String,
    pub(super) authorization: String,
    pub(super) attempts: usize,
    pub(super) state: String,
    pub(super) next_at: Option<String>,
    pub(super) error: Option<Value>,
}

impl Work {
    pub(super) fn pending(
        witness: Witness,
        body_kind: String,
        segment: Segment,
        group: &str,
        authorization: &str,
    ) -> Self {
        Self {
            witness,
            body_kind,
            segment,
            group: group.into(),
            authorization: authorization.into(),
            attempts: 0,
            state: "pending".into(),
            next_at: None,
            error: None,
        }
    }
}
impl IndexService {
    pub(super) async fn enqueue_batch(
        &self,
        key: &str,
        work: Vec<Work>,
    ) -> Result<usize, ApiError> {
        let key = key.to_owned();
        let inserted = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let mut inserted = 0;
                for work in work {
                    if !witness_valid(&tx, &work.witness, &work.body_kind)? {
                        continue;
                    }
                    if let Some(space) = space_for_group(&tx, IndexKeys::new(&key), &work.group)? {
                        inserted += queue_job(&tx, &space, &work)? as usize;
                    } else {
                        let bootstrap = bootstrap_key(IndexKeys::new(&key), &work);
                        inserted += tx.execute(
                            "INSERT OR IGNORE INTO app_meta(key,value)VALUES(?1,?2)",
                            params![bootstrap, encode_json(&work)?],
                        )?;
                        if !work.authorization.starts_with("new-finalized")
                            && let Some(mut prior) = json_meta::<Work>(&tx, &bootstrap)?
                            && prior.state == "pending"
                            && prior.authorization.starts_with("new-finalized")
                        {
                            prior.authorization.clone_from(&work.authorization);
                            set_json_meta(&tx, &bootstrap, &prior)?;
                        }
                    }
                }
                tx.commit()?;
                Ok(inserted)
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(inserted > 0);
        Ok(inserted)
    }
}

const RECOVERY_ROWS: i64 = 128;
const RECOVERY_BYTES: i64 = 1024 * 1024;

/// One transaction per page. The in-memory cursor may be discarded on restart:
/// already converted jobs and bootstrap records are safe to visit again.
pub(super) fn recover_inflight_page(
    db: &mut Connection,
    cursor: Option<String>,
) -> Result<(bool, Option<String>, bool), StorageError> {
    let tx = db.transaction()?;
    let interrupted = json!({"kind":"interrupted","message":"Previous embedding request interrupted; retry explicitly and consider duplicate billing"});
    let changed = tx.execute(
        "UPDATE embedding_job SET state='failed',error_json=?1 WHERE id IN (SELECT id FROM embedding_job INDEXED BY jobs_running_recovery WHERE state='running' ORDER BY id LIMIT ?2)",
        params![interrupted.to_string(), RECOVERY_ROWS],
    )?;
    if changed > 0 {
        tx.commit()?;
        return Ok((true, cursor, false));
    }
    let mut last = cursor.unwrap_or_default();
    let mut count = 0;
    let mut bytes = 0;
    let mut modified = false;
    loop {
        let row: Option<(String, i64)> = tx.query_row(
            "SELECT key,octet_length(value) FROM app_meta WHERE key>='index.bootstrap.' AND key>?1 AND key<'index.bootstrap/' ORDER BY key LIMIT 1",
            [&last], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let Some((key, size)) = row else {
            tx.commit()?;
            return Ok((modified, None, true));
        };
        if count == RECOVERY_ROWS || (count > 0 && size > RECOVERY_BYTES.saturating_sub(bytes)) {
            tx.commit()?;
            return Ok((modified, Some(last), false));
        }
        let value: String =
            tx.query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |r| {
                r.get(0)
            })?;
        let mut work: Work = decode_json(&value)?;
        if work.state == "running" {
            work.state = "failed".into();
            work.error = Some(
                json!({"kind":"interrupted","message":"Previous bootstrap request interrupted; retry explicitly"}),
            );
            modified |= update_json_meta(&tx, &key, &work)? > 0;
        }
        last = key;
        count += 1;
        bytes += size;
    }
}
#[cfg(test)]
pub(super) fn recover_authorization(db: &mut Connection) -> Result<(), StorageError> {
    recover_authorization_progress(db).0
}
/// Preserve each existing autocommit before a possible later decode/save error.
pub(super) fn recover_authorization_progress(
    db: &mut Connection,
) -> (Result<(), StorageError>, bool) {
    let mut changed = false;
    let result = (|| {
        let app_meta_sql = "SELECT key,value FROM app_meta WHERE key LIKE 'index.auto.%' AND json_extract(value,'$.state')='running'";
        let mut statement = db.prepare(app_meta_sql)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (key, value) in rows {
            let mut admission: AutoAdmission = decode_json(&value)?;
            admission.state = "failed".into();
            admission.error = Some("Index source preparation interrupted; retry explicitly".into());
            changed |= update_json_meta(db, &key, &admission)? > 0;
        }
        Ok(())
    })();
    (result, changed)
}

/// Select and mark one authorized source before any parsing/HTTP work.
/// Seeks this profile's byte-exact authorization key range (#171) instead of
/// a LIKE scan of all app_meta.
pub(super) fn claim_authorization(
    db: &mut Connection,
    range: &(String, String),
    profile: &str,
    family: Option<&str>,
) -> Result<(bool, Option<(String, AutoAdmission)>), StorageError> {
    let app_meta_sql = "SELECT key,value FROM app_meta WHERE key>=?1 AND key<?2 AND json_extract(value,'$.state')='pending' ORDER BY key LIMIT 1";
    let tx = db.transaction()?;
    let row: Option<(String, String)> = tx
        .query_row(app_meta_sql, [&range.0, &range.1], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let Some((key, value)) = row else {
        return Ok((false, None));
    };
    let paused = meta(&tx, &IndexKeys::new(profile).pause())?.is_some()
        || family
            .map(|key| meta(&tx, &IndexKeys::new(key).pause()))
            .transpose()?
            .flatten()
            .is_some();
    if paused {
        tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])?;
        tx.commit()?;
        return Ok((true, None));
    }
    let mut admission: AutoAdmission = decode_json(&value)?;
    admission.state = "running".into();
    update_json_meta(&tx, &key, &admission)?;
    tx.commit()?;
    Ok((false, Some((key, admission))))
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    fn recovery_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE embedding_job(id TEXT PRIMARY KEY,state TEXT NOT NULL,error_json TEXT); CREATE INDEX jobs_running_recovery ON embedding_job(id) WHERE state='running';").unwrap();
        db
    }

    #[test]
    fn recovery_pages_resume_and_use_indexes() {
        let mut db = recovery_db();
        for i in 0..300 {
            db.execute(
                "INSERT INTO embedding_job VALUES(?1,'running',NULL)",
                [format!("{i:04}")],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO embedding_job VALUES('pending','pending',NULL)",
            [],
        )
        .unwrap();
        let work = Work::pending(
            Witness {
                node_id: "n".into(),
                revision: 1,
                body_hash: None,
                raw: None,
            },
            "text".into(),
            Segment {
                start: 0,
                end: 1,
                text: "a".into(),
            },
            "g",
            "test",
        );
        let mut running = work.clone();
        running.state = "running".into();
        for i in 0..300 {
            db.execute(
                "INSERT INTO app_meta VALUES(?1,?2)",
                params![
                    format!("index.bootstrap.{i:04}"),
                    encode_json(&running).unwrap()
                ],
            )
            .unwrap();
        }
        let (changed, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
        assert!(changed && !done && cursor.is_none());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM embedding_job WHERE state='running'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            172
        );
        // Simulate an owner restart after the first committed page.
        let mut cursor = None;
        let mut pages = 1;
        loop {
            let (_, next, done) = recover_inflight_page(&mut db, cursor).unwrap();
            pages += 1;
            if done {
                break;
            }
            cursor = next;
        }
        assert!(pages >= 6);
        // Restarting with no cursor after successful bootstrap pages is idempotent.
        let mut cursor = None;
        loop {
            let (changed, next, done) = recover_inflight_page(&mut db, cursor).unwrap();
            assert!(!changed);
            if done {
                break;
            }
            cursor = next;
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM embedding_job WHERE state='failed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            300
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE json_extract(value,'$.state')='failed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            300
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM embedding_job WHERE id='pending'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "pending"
        );
        let jobs: String = db.query_row("EXPLAIN QUERY PLAN SELECT id FROM embedding_job INDEXED BY jobs_running_recovery WHERE state='running' ORDER BY id LIMIT 128", [], |r| r.get(3)).unwrap();
        assert!(jobs.contains("jobs_running_recovery"), "{jobs}");
        let meta: String = db.query_row("EXPLAIN QUERY PLAN SELECT key,octet_length(value) FROM app_meta WHERE key>='index.bootstrap.' AND key>?1 AND key<'index.bootstrap/' ORDER BY key LIMIT 1", [""], |r| r.get(3)).unwrap();
        assert!(meta.contains("SEARCH") && meta.contains("INDEX"), "{meta}");
    }

    #[test]
    fn bootstrap_page_caps_bytes_and_does_not_enter_adjacent_prefix() {
        let mut db = recovery_db();
        let mut work = Work::pending(
            Witness {
                node_id: "n".into(),
                revision: 1,
                body_hash: None,
                raw: None,
            },
            "text".into(),
            Segment {
                start: 0,
                end: 1,
                text: "x".repeat(600_000),
            },
            "g",
            "test",
        );
        work.state = "running".into();
        let value = encode_json(&work).unwrap();
        for key in ["index.bootstrap.a", "index.bootstrap.b"] {
            db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![key, value])
                .unwrap();
        }
        db.execute(
            "INSERT INTO app_meta VALUES('index.bootstrap/z','not json')",
            [],
        )
        .unwrap();
        let (changed, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
        assert!(changed && !done);
        assert_eq!(cursor.as_deref(), Some("index.bootstrap.a"));
        let (_, _, done) = recover_inflight_page(&mut db, cursor).unwrap();
        assert!(done);
        assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key<'index.bootstrap/' AND json_extract(value,'$.state')='failed'", [], |r| r.get::<_,i64>(0)).unwrap(), 2);
    }

    #[test]
    fn recovery_processes_oversized_bootstrap_alone() {
        let mut db = recovery_db();
        let mut work = Work::pending(
            Witness {
                node_id: "n".into(),
                revision: 1,
                body_hash: None,
                raw: None,
            },
            "text".into(),
            Segment {
                start: 0,
                end: 1,
                text: "x".repeat(RECOVERY_BYTES as usize + 1),
            },
            "g",
            "test",
        );
        work.state = "running".into();
        let value = encode_json(&work).unwrap();
        for key in ["index.bootstrap.a", "index.bootstrap.b"] {
            db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![key, value])
                .unwrap();
        }
        let (changed, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
        assert!(changed && !done);
        assert_eq!(cursor.as_deref(), Some("index.bootstrap.a"));
        assert_eq!(
            json_meta::<Work>(&db, "index.bootstrap.a")
                .unwrap()
                .unwrap()
                .state,
            "failed"
        );
        assert_eq!(
            json_meta::<Work>(&db, "index.bootstrap.b")
                .unwrap()
                .unwrap()
                .state,
            "running"
        );
        // A new owner starts over without a cursor; the oversized failed row
        // remains intact and the other row is eventually recovered.
        let mut cursor = None;
        loop {
            let (_, next, done) = recover_inflight_page(&mut db, cursor).unwrap();
            if done {
                break;
            }
            cursor = next;
        }
        assert_eq!(
            json_meta::<Work>(&db, "index.bootstrap.b")
                .unwrap()
                .unwrap()
                .state,
            "failed"
        );
    }

    #[test]
    fn claim_discards_admission_if_family_pauses_before_claim() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        let keys = IndexKeys::new("profile");
        db.execute(
            "INSERT INTO app_meta VALUES(?1,?2)",
            params![
                keys.auto("node"),
                r#"{"node_id":"node","state":"pending","error":null}"#
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO app_meta VALUES(?1,'{}')",
            [IndexKeys::new("family").pause()],
        )
        .unwrap();
        let (discarded, claim) =
            claim_authorization(&mut db, &keys.auto_range(), "profile", Some("family")).unwrap();
        assert!(discarded);
        assert!(claim.is_none());
        assert!(meta(&db, &keys.auto("node")).unwrap().is_none());
    }
    #[test]
    fn interrupted_auto_recovery_keeps_existing_per_row_commit_and_decode_order() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        db.execute(
            "INSERT INTO app_meta VALUES('index.auto.a',?1)",
            [r#"{"node_id":"node","state":"running","error":null}"#],
        )
        .unwrap();
        db.execute(
            "INSERT INTO app_meta VALUES('index.auto.b',?1)",
            [r#"{"state":"running"}"#],
        )
        .unwrap();
        assert!(recover_authorization(&mut db).is_err());
        let first: AutoAdmission = json_meta(&db, "index.auto.a").unwrap().unwrap();
        assert_eq!(
            first.state, "failed",
            "typed parsing must not predecode the entire scan before existing per-row updates"
        );
        let second: Value = json_meta(&db, "index.auto.b").unwrap().unwrap();
        assert_eq!(second["state"], "running");
    }
}
