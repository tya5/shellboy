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
        self.database
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
                    }
                }
                tx.commit()?;
                Ok(inserted)
            })
            .await
            .map_err(db_error)
    }
}

pub(super) fn recover_inflight(db: &mut Connection) -> Result<(), StorageError> {
    let tx = db.transaction()?;
    let interrupted = json!({"kind":"interrupted","message":"Previous embedding request interrupted; retry explicitly and consider duplicate billing"});
    tx.execute(
        "UPDATE embedding_job SET state='failed',error_json=?1 WHERE state='running'",
        [interrupted.to_string()],
    )?;
    let mut statement =
        tx.prepare("SELECT key,value FROM app_meta WHERE key LIKE 'index.bootstrap.%'")?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (key, value) in rows {
        let mut work: Work = decode_json(&value)?;
        if work.state == "running" {
            work.state = "failed".into();
            work.error = Some(
                json!({ "kind":"interrupted","message":"Previous bootstrap request interrupted; retry explicitly"} ),
            );
            update_json_meta(&tx, &key, &work)?;
        }
    }
    tx.commit()?;
    Ok(())
}
pub(super) fn recover_authorization(db: &mut Connection) -> Result<(), StorageError> {
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
        update_json_meta(db, &key, &admission)?;
    }
    Ok(())
}

/// Select and mark one authorized source before any parsing/HTTP work.
pub(super) fn claim_authorization(
    db: &mut Connection,
    prefix: &str,
) -> Result<Option<(String, AutoAdmission)>, StorageError> {
    let app_meta_sql = "SELECT key,value FROM app_meta WHERE key LIKE ?1 AND json_extract(value,'$.state')='pending' ORDER BY key LIMIT 1";
    let tx = db.transaction()?;
    let row: Option<(String, String)> = tx
        .query_row(app_meta_sql, [prefix], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((key, value)) = row else {
        return Ok(None);
    };
    let mut admission: AutoAdmission = decode_json(&value)?;
    admission.state = "running".into();
    update_json_meta(&tx, &key, &admission)?;
    tx.commit()?;
    Ok(Some((key, admission)))
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
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
