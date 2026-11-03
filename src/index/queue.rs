//! Authorized fixed-input job admission and durable queue keys.
use super::*;
use std::io::{self, Read, Seek, SeekFrom, Write};

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
    let watermark = json!({"witness":work.witness,"group":work.group,"authorization":work.authorization,"recovery":work.recovery});
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
        && (!work.authorization.starts_with("new-finalized") || work.recovery)
        && let Some(job) = job_id_for(tx, space, work)?
        && let Some(mut prior) = json_meta::<Work>(tx, &IndexKeys::input(&job))?
        && prior.state == "pending"
        && prior.authorization.starts_with("new-finalized")
    {
        prior.authorization.clone_from(&work.authorization);
        prior.recovery |= work.recovery;
        set_json_meta(tx, &IndexKeys::input(&job), &prior)?;
        tx.execute("UPDATE embedding_job SET authorized_watermark=json_set(authorized_watermark,'$.authorization',?2,'$.recovery',json(?3)) WHERE id=?1 AND state='pending'", params![job,work.authorization,if prior.recovery { "true" } else { "false" }])?;
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

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct AutoAdmission {
    pub(super) node_id: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) recovery: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) recovery_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) recovery_authorization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) recovery_witness: Option<Witness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) recovery_body_kind: Option<String>,
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) recovery: bool,
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
            recovery: false,
            attempts: 0,
            state: "pending".into(),
            next_at: None,
            error: None,
        }
    }
}
fn split_bounded_work(work: Work) -> Result<Vec<Work>, StorageError> {
    let mut pending = vec![work];
    let mut result = Vec::new();
    while let Some(work) = pending.pop() {
        if encode_bootstrap_work(&work).is_ok() {
            result.push(work);
            continue;
        }
        let chars = work.segment.text.chars().count();
        if chars < 2 {
            return Err(StorageError::InvalidInput(
                "bootstrap work metadata exceeds byte limit".into(),
            ));
        }
        let midpoint = work
            .segment
            .text
            .char_indices()
            .nth(chars / 2)
            .map(|(i, _)| i)
            .unwrap();
        let mut left = work.clone();
        let mut right = work;
        left.segment.text = right.segment.text[..midpoint].to_owned();
        right.segment.text = right.segment.text[midpoint..].to_owned();
        left.segment.end = left.segment.start + chars / 2;
        right.segment.start = left.segment.end;
        pending.push(right);
        pending.push(left);
    }
    Ok(result)
}

impl IndexService {
    pub(super) async fn enqueue_batch(
        &self,
        key: &str,
        work: Vec<Work>,
    ) -> Result<(usize, usize), ApiError> {
        let key = key.to_owned();
        let work = work
            .into_iter()
            .map(split_bounded_work)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let total = work.len();
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
                        let value = encode_bootstrap_work(&work)?;
                        inserted += tx.execute(
                            "INSERT OR IGNORE INTO app_meta(key,value)VALUES(?1,?2)",
                            params![bootstrap, value],
                        )?;
                        if (!work.authorization.starts_with("new-finalized") || work.recovery)
                            && let Some(mut prior) = json_meta::<Work>(&tx, &bootstrap)?
                            && prior.state == "pending"
                            && prior.authorization.starts_with("new-finalized")
                        {
                            prior.authorization.clone_from(&work.authorization);
                            prior.recovery |= work.recovery;
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
        Ok((inserted, total))
    }
}

pub(super) const RECOVERY_REQUIRED_MESSAGE: &str = "index_manual_rebuild_required";
const RECOVERY_ROWS: i64 = 128;
// A work record must fit one bounded recovery page, including its JSON metadata.
const RECOVERY_BYTES: i64 = 1024 * 1024;
const MAX_BOOTSTRAP_RECORD_BYTES: usize = RECOVERY_BYTES as usize;

struct BoundedJson(Vec<u8>, bool);

impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_BOOTSTRAP_RECORD_BYTES {
            self.1 = true;
            return Err(io::Error::other("bootstrap JSON limit exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_bootstrap_work(work: &Work) -> Result<String, StorageError> {
    if work.segment.text.len() > MAX_BOOTSTRAP_RECORD_BYTES {
        return Err(StorageError::Operation(Box::new(err(
            ErrorCode::InvalidArgument,
            format!("bootstrap segment exceeds {MAX_BOOTSTRAP_RECORD_BYTES} bytes"),
        ))));
    }
    let mut output = BoundedJson(Vec::with_capacity(4096), false);
    if let Err(error) = serde_json::to_writer(&mut output, work) {
        if output.1 {
            return Err(StorageError::Operation(Box::new(err(
                ErrorCode::InvalidArgument,
                format!("bootstrap work record exceeds {MAX_BOOTSTRAP_RECORD_BYTES} bytes"),
            ))));
        }
        return Err(StorageError::InvalidInput(error.to_string()));
    }
    String::from_utf8(output.0).map_err(|error| StorageError::InvalidInput(error.to_string()))
}

// Work JSON is written with witness and body kind first. Inspect only a bounded
// prefix; an unparseable prefix is never treated as authorization.
fn legacy_source_prefix(bytes: &[u8]) -> Option<(Witness, String)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_prefix("{\"witness\":")?;
    let mut witness = serde_json::Deserializer::from_str(text).into_iter::<Witness>();
    let parsed = witness.next()?.ok()?;
    let text = text
        .get(witness.byte_offset()..)?
        .strip_prefix(",\"body_kind\":")?;
    let kind = serde_json::Deserializer::from_str(text)
        .into_iter::<String>()
        .next()?
        .ok()?;
    Some((parsed, kind))
}

#[derive(Deserialize)]
struct LegacyTail {
    group: String,
    authorization: String,
    state: String,
}

fn legacy_source_tail(bytes: &[u8]) -> Option<LegacyTail> {
    let text = String::from_utf8_lossy(bytes);
    let offset = text.rfind(",\"group\":")?;
    serde_json::from_str(&format!("{{{}", &text[offset + 1..])).ok()
}

pub(super) fn bootstrap_recovery_query(cursor: Option<&str>) -> (&'static str, &str) {
    // Two lower bounds can make SQLite rescan the prefix before testing cursor.
    match cursor {
        Some(last) if last >= IndexKeys::BOOT_PREFIX => (
            "SELECT key,octet_length(value) FROM app_meta WHERE key>?1 AND key<'index.bootstrap/' ORDER BY key LIMIT 1",
            last,
        ),
        _ => (
            "SELECT key,octet_length(value) FROM app_meta WHERE key>=?1 AND key<'index.bootstrap/' ORDER BY key LIMIT 1",
            IndexKeys::BOOT_PREFIX,
        ),
    }
}

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
    let mut last = cursor;
    let mut count = 0;
    let mut bytes = 0;
    let mut modified = false;
    loop {
        let (sql, bound) = bootstrap_recovery_query(last.as_deref());
        let row: Option<(String, i64)> = tx
            .query_row(sql, [bound], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((key, size)) = row else {
            tx.commit()?;
            return Ok((modified, None, true));
        };
        // Defer an oversized candidate too: its prefix/tail reads need a fresh page.
        if count == RECOVERY_ROWS || (count > 0 && size > RECOVERY_BYTES.saturating_sub(bytes)) {
            tx.commit()?;
            return Ok((modified, last, false));
        }
        if size > RECOVERY_BYTES {
            if let Some((profile, _)) = key
                .strip_prefix(IndexKeys::BOOT_PREFIX)
                .and_then(|suffix| suffix.rsplit_once('.'))
            {
                let rowid: i64 =
                    tx.query_row("SELECT rowid FROM app_meta WHERE key=?1", [&key], |row| {
                        row.get(0)
                    })?;
                let mut prefix = vec![0; 65536];
                let mut tail = vec![0; 65536];
                {
                    let mut blob = tx.blob_open("main", "app_meta", "value", rowid, true)?;
                    blob.read_exact(&mut prefix)
                        .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
                    blob.seek(SeekFrom::End(-65536))
                        .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
                    blob.read_exact(&mut tail)
                        .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
                }
                // The prefix may end in the middle of a UTF-8 character.
                let prefix = String::from_utf8_lossy(&prefix);
                let source = legacy_source_prefix(prefix.as_bytes());
                let identified = if let (Some((witness, kind)), Some(tail)) =
                    (source, legacy_source_tail(&tail))
                {
                    if (tail.authorization.starts_with("new-finalized")
                        || ["explicit-backfill", "explicit-rebuild", "explicit-retry"]
                            .contains(&tail.authorization.as_str()))
                        && ["pending", "running", "failed"].contains(&tail.state.as_str())
                        && group_current(&tx, IndexKeys::new(profile), &tail.group)?
                        && witness_valid(&tx, &witness, &kind)?
                    {
                        let admission = AutoAdmission {
                            node_id: witness.node_id.clone(),
                            recovery: true,
                            recovery_group: Some(tail.group.clone()),
                            recovery_authorization: Some(tail.authorization.clone()),
                            recovery_witness: Some(witness.clone()),
                            recovery_body_kind: Some(kind),
                            state: if tail.state == "pending" { "pending" } else { "failed" }.into(),
                            error: (tail.state != "pending").then(|| "Previous bootstrap request interrupted or failed; retry explicitly".into()),
                        };
                        let record = IndexKeys::new(profile).auto(&format!(
                            "recovery.{}",
                            hash(format!("{}\0{}", witness.node_id, tail.group).as_bytes())
                        ));
                        tx.execute(
                            "INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE json_extract(excluded.value,'$.state')='failed'",
                            params![record, encode_json(&admission)?],
                        )?;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !identified {
                    set_meta(&tx, &IndexKeys::new(profile).recovery_required(), &id())?;
                }
            }
            tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])?;
            tx.commit()?;
            return Ok((true, Some(key), false));
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
        last = Some(key);
        count += 1;
        bytes += size;
    }
}
#[cfg(test)]
pub(super) fn recover_authorization(db: &mut Connection) -> Result<(), StorageError> {
    let mut cursor = None;
    loop {
        let (result, _) = recover_authorization_page(db, cursor);
        match result? {
            Some(next) => cursor = Some(next),
            None => return Ok(()),
        }
    }
}

const AUTH_RECOVERY_FIRST: &str = "SELECT key,octet_length(value) FROM app_meta WHERE key>=?1 AND key<'index.auto/' ORDER BY key LIMIT 1";
const AUTH_RECOVERY_NEXT: &str = "SELECT key,octet_length(value) FROM app_meta WHERE key>?1 AND key<'index.auto/' ORDER BY key LIMIT 1";

/// Each update autocommits; return its notification even on a later error.
pub(super) fn recover_authorization_page(
    db: &mut Connection,
    cursor: Option<String>,
) -> (Result<Option<String>, StorageError>, bool) {
    let mut changed = false;
    let result = (|| {
        let mut first = cursor.is_none();
        let mut last = cursor.unwrap_or_else(|| "index.auto.".to_string());
        let mut count = 0;
        let mut bytes = 0;
        loop {
            let row: Option<(String, i64)> = db
                .query_row(
                    if first {
                        AUTH_RECOVERY_FIRST
                    } else {
                        AUTH_RECOVERY_NEXT
                    },
                    [&last],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((key, size)) = row else {
                return Ok(None);
            };
            if count == RECOVERY_ROWS || (count > 0 && size > RECOVERY_BYTES.saturating_sub(bytes))
            {
                return Ok(Some(last));
            }
            if size > RECOVERY_BYTES {
                return Err(StorageError::InvalidInput(
                    "oversized index authorization record requires manual repair".into(),
                ));
            }
            let (value, running): (String, bool) = db.query_row(
                "SELECT value,coalesce(json_extract(value,'$.state')='running',0) FROM app_meta WHERE key=?1",
                [&key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if running {
                let mut admission: AutoAdmission = decode_json(&value)?;
                admission.state = "failed".into();
                admission.error =
                    Some("Index source preparation interrupted; retry explicitly".into());
                changed |= update_json_meta(db, &key, &admission)? > 0;
            }
            last = key;
            first = false;
            count += 1;
            bytes += size;
        }
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
    auto_history: bool,
) -> Result<(bool, Option<(String, AutoAdmission)>), StorageError> {
    let app_meta_sql = "SELECT key,value FROM app_meta WHERE key>=?1 AND key<?2 AND json_extract(value,'$.state')='pending' AND (?3 OR json_extract(value,'$.recovery_authorization') IN ('explicit-backfill','explicit-rebuild','explicit-retry')) ORDER BY key LIMIT 1";
    let tx = db.transaction()?;
    let row: Option<(String, String)> = tx
        .query_row(
            app_meta_sql,
            params![range.0, range.1, auto_history],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
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
    let mut admission: AutoAdmission = decode_json(&value)?;
    if admission.recovery
        && (admission.recovery_authorization.is_none()
            || admission.recovery_group.is_none()
            || admission.recovery_witness.is_none()
            || admission.recovery_body_kind.is_none())
    {
        admission.state = "failed".into();
        admission.error =
            Some("Recovered source admission is incomplete; explicitly rebuild".into());
        update_json_meta(&tx, &key, &admission)?;
        tx.commit()?;
        return Ok((true, None));
    }
    if paused && admission.recovery {
        return Ok((false, None));
    }
    if paused {
        tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])?;
        tx.commit()?;
        return Ok((true, None));
    }
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
    fn oversized_work_splits_without_losing_unicode_or_offsets() {
        let text = "\"\n日本".repeat(200_000);
        let work = Work::pending(
            Witness {
                node_id: "n".into(),
                revision: 1,
                body_hash: None,
                raw: None,
            },
            "text".into(),
            Segment {
                start: 9,
                end: 9 + text.chars().count(),
                text: text.clone(),
            },
            "active",
            "test",
        );
        let parts = split_bounded_work(work).unwrap();
        assert!(parts.len() > 1);
        assert_eq!(
            parts
                .iter()
                .map(|part| part.segment.text.as_str())
                .collect::<String>(),
            text
        );
        let mut offset = 9;
        for part in parts {
            assert_eq!(part.segment.start, offset);
            offset += part.segment.text.chars().count();
            assert_eq!(part.segment.end, offset);
            assert!(encode_bootstrap_work(&part).unwrap().len() <= MAX_BOOTSTRAP_RECORD_BYTES);
        }
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
        let (sql, bound) = bootstrap_recovery_query(None);
        let meta: String = db
            .query_row(&format!("EXPLAIN QUERY PLAN {sql}"), [bound], |r| r.get(3))
            .unwrap();
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
    fn recovery_skips_oversized_legacy_bootstrap_without_decoding_it() {
        let mut db = recovery_db();
        let key = "index.bootstrap.profile.a";
        let value = "not JSON".to_owned() + &"x".repeat(RECOVERY_BYTES as usize + 1);
        db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![key, value])
            .unwrap();
        let ordinary = Work::pending(
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
                text: "x".into(),
            },
            "g",
            "test",
        );
        db.execute(
            "INSERT INTO app_meta VALUES('index.bootstrap.profile.b',?1)",
            [encode_json(&ordinary).unwrap()],
        )
        .unwrap();

        let (changed, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
        assert!(changed && !done);
        assert_eq!(cursor.as_deref(), Some(key));
        assert_eq!(
            db.query_row("SELECT count(*) FROM app_meta WHERE key=?1", [key], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        assert!(
            meta(&db, &IndexKeys::new("profile").recovery_required())
                .unwrap()
                .is_some()
        );
        // The oversized row is gone; the next row is visited normally.
        let (changed, _, done) = recover_inflight_page(&mut db, cursor).unwrap();
        assert!(!changed && done);
    }

    #[test]
    fn bootstrap_writer_rejects_oversized_segments_and_serialized_metadata() {
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
                end: MAX_BOOTSTRAP_RECORD_BYTES + 1,
                text: "x".repeat(MAX_BOOTSTRAP_RECORD_BYTES + 1),
            },
            "g",
            "test",
        );
        let error = encode_bootstrap_work(&work).unwrap_err();
        assert!(error.to_string().contains("bootstrap segment exceeds"));

        let work = Work::pending(
            Witness {
                node_id: "n".repeat(MAX_BOOTSTRAP_RECORD_BYTES),
                revision: 1,
                body_hash: None,
                raw: None,
            },
            "text".into(),
            Segment {
                start: 0,
                end: 1,
                text: "x".into(),
            },
            "g",
            "test",
        );
        let error = encode_bootstrap_work(&work).unwrap_err();
        assert!(error.to_string().contains("bootstrap work record exceeds"));
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
            claim_authorization(&mut db, &keys.auto_range(), "profile", Some("family"), true)
                .unwrap();
        assert!(discarded);
        assert!(claim.is_none());
        assert!(meta(&db, &keys.auto("node")).unwrap().is_none());
    }
    #[test]
    fn authorization_recovery_pages_large_set_and_restarts() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        for n in 0..1000 {
            db.execute(
                "INSERT INTO app_meta VALUES(?1,?2)",
                params![
                    format!("index.auto.{n:04}"),
                    r#"{"node_id":"node","state":"running","error":null}"#
                ],
            )
            .unwrap();
        }
        let (first, changed) = recover_authorization_page(&mut db, None);
        assert!(changed);
        let first = first.unwrap();
        assert_eq!(first.as_deref(), Some("index.auto.0127"));
        let (second, changed) = recover_authorization_page(&mut db, first);
        assert!(changed);
        assert_eq!(second.unwrap().as_deref(), Some("index.auto.0255"));
        let failed: i64 = db
            .query_row(
                "SELECT count(*) FROM app_meta WHERE json_extract(value,'$.state')='failed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(failed, 2 * RECOVERY_ROWS);
        // Restart without a cursor, as after a crash between pages.
        recover_authorization(&mut db).unwrap();
        let failed: i64 = db
            .query_row(
                "SELECT count(*) FROM app_meta WHERE json_extract(value,'$.state')='failed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(failed, 1000);
        let (done, changed) = recover_authorization_page(&mut db, None);
        assert!(done.is_ok());
        assert!(!changed);
    }

    #[test]
    fn authorization_recovery_late_cursor_seeks_without_scanning_history() {
        use rusqlite::StatementStatus;
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        for n in 0..10_000 {
            db.execute(
                "INSERT INTO app_meta VALUES(?1,'{}')",
                [format!("index.auto.{n:05}")],
            )
            .unwrap();
        }
        let steps = |cursor: &str| {
            let mut stmt = db.prepare(AUTH_RECOVERY_NEXT).unwrap();
            let result: Option<String> = stmt.query_row([cursor], |r| r.get(0)).optional().unwrap();
            (result, stmt.get_status(StatementStatus::VmStep))
        };
        let (early, early_steps) = steps("index.auto.00127");
        let (late, late_steps) = steps("index.auto.09998");
        let (end, end_steps) = steps("index.auto.09999");
        assert_eq!(early.as_deref(), Some("index.auto.00128"));
        assert_eq!(late.as_deref(), Some("index.auto.09999"));
        assert_eq!(end, None);
        assert!(
            late_steps <= early_steps + 10 && end_steps <= early_steps + 10,
            "early={early_steps}, late={late_steps}, end={end_steps}"
        );
    }

    #[test]
    fn authorization_recovery_preserves_notification_on_error_and_caps_bytes() {
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
            ["x".repeat(RECOVERY_BYTES as usize + 1)],
        )
        .unwrap();
        let (next, changed) = recover_authorization_page(&mut db, None);
        assert!(changed);
        let cursor = next.unwrap().unwrap();
        let (result, changed) = recover_authorization_page(&mut db, Some(cursor));
        assert!(result.is_err());
        assert!(!changed);
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='index.auto.b'",
            [r#"{"state":"running"}"#],
        )
        .unwrap();
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='index.auto.a'",
            [r#"{"node_id":"node","state":"running","error":null}"#],
        )
        .unwrap();
        let (result, changed) = recover_authorization_page(&mut db, None);
        assert!(result.is_err());
        assert!(changed);
    }

    #[test]
    fn authorization_recovery_limits_aggregate_page_bytes() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        let value = serde_json::json!({"node_id":"node", "state":"running", "error":null, "padding":"x".repeat(600_000)}).to_string();
        for key in ["index.auto.a", "index.auto.b"] {
            db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![key, value])
                .unwrap();
        }
        let (page, changed) = recover_authorization_page(&mut db, None);
        assert!(changed);
        assert_eq!(page.unwrap().as_deref(), Some("index.auto.a"));
        let second: Value = json_meta(&db, "index.auto.b").unwrap().unwrap();
        assert_eq!(second["state"], "running");
        recover_authorization(&mut db).unwrap();
        let second: AutoAdmission = json_meta(&db, "index.auto.b").unwrap().unwrap();
        assert_eq!(second.state, "failed");
    }

    #[test]
    fn authorization_save_failure_reports_prior_commit_and_retry_resumes() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        for key in ["index.auto.a", "index.auto.b"] {
            db.execute(
                "INSERT INTO app_meta VALUES(?1,?2)",
                params![key, r#"{"node_id":"node","state":"running","error":null}"#],
            )
            .unwrap();
        }
        db.execute_batch("CREATE TRIGGER fail_b BEFORE UPDATE ON app_meta WHEN OLD.key='index.auto.b' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let (result, changed) = recover_authorization_page(&mut db, None);
        assert!(result.is_err());
        assert!(changed);
        let first: AutoAdmission = json_meta(&db, "index.auto.a").unwrap().unwrap();
        assert_eq!(first.state, "failed");
        db.execute_batch("DROP TRIGGER fail_b;").unwrap();
        recover_authorization(&mut db).unwrap();
        let second: AutoAdmission = json_meta(&db, "index.auto.b").unwrap().unwrap();
        assert_eq!(second.state, "failed");
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
