//! #164: while indexing is paused, no automatic work is admitted, and pending
//! automatic work is discarded in small restartable transactions. Explicit
//! backfill/rebuild work, running claims and failed records are kept.
use super::*;

const STATE: &str = "index.pause_cleanup.state";
const TOTAL: &str = "index.pause_cleanup.discarded";
/// Rows or metadata keys examined per transaction (not a queue cap).
pub(super) const PAGE: i64 = 128;
const DONE: u8 = 3;

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    pause: String,
    stage: u8,
    rowid: i64,
    key: String,
    discarded: u64,
}

pub(super) struct Step {
    /// Another bounded step should run before normal processing.
    pub(super) more: bool,
    /// Set once when a pass that discarded something completes.
    pub(super) finished: Option<u64>,
}

/// Saved pause id for the profile or its endpoint family, if paused.
pub(super) fn pause_id(
    db: &Connection,
    profile: &str,
    family: Option<&str>,
) -> Result<Option<String>, StorageError> {
    let family_pause = family.map(|f| IndexKeys::new(f).pause());
    let profile_pause = IndexKeys::new(profile).pause();
    for key in family_pause.iter().chain(std::iter::once(&profile_pause)) {
        if let Some(value) = meta(db, key)? {
            return Ok(Some(pause::facts(key, &value).1));
        }
    }
    Ok(None)
}

pub(super) fn automatic(authorization: &str) -> bool {
    authorization.starts_with("new-finalized")
}

/// One bounded transaction of the cleanup pass for the current pause.
pub(super) fn step(
    db: &mut Connection,
    profile: &str,
    family: Option<&str>,
) -> Result<Step, StorageError> {
    let idle = Step {
        more: false,
        finished: None,
    };
    let tx = db.transaction()?;
    let Some(pause) = pause_id(&tx, profile, family)? else {
        return Ok(idle);
    };
    let mut cursor = json_meta::<Cursor>(&tx, STATE)?
        .filter(|c| c.pause == pause)
        .unwrap_or_else(|| Cursor {
            pause,
            ..Cursor::default()
        });
    if cursor.stage >= DONE {
        return Ok(idle);
    }
    let keys = IndexKeys::new(profile);
    let mut deleted = 0u64;
    let examined;
    if cursor.stage == 0 {
        let rows = tx
            .prepare(
                "SELECT j.rowid,j.id,j.state,COALESCE(json_extract(j.authorized_watermark,'$.authorization'),''),s.splitter_version IS ?3 FROM embedding_job j LEFT JOIN index_space s ON s.id=j.space_id WHERE j.rowid>?1 ORDER BY j.rowid LIMIT ?2",
            )?
            .query_map(params![cursor.rowid, PAGE, keys.splitter()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        examined = rows.len();
        for (rowid, id, state, authorization, owned) in rows {
            cursor.rowid = rowid;
            if owned && state == "pending" && automatic(&authorization) {
                tx.execute("DELETE FROM app_meta WHERE key=?1", [IndexKeys::input(&id)])?;
                deleted += tx.execute(
                    "DELETE FROM embedding_job WHERE rowid=?1 AND state='pending'",
                    [rowid],
                )? as u64;
            }
        }
    } else {
        // Profile-owned key ranges: '/' sorts directly after '.'.
        let prefix = if cursor.stage == 1 {
            keys.bootstrap("")
        } else {
            keys.auto("")
        };
        let upper = format!("{}/", &prefix[..prefix.len() - 1]);
        let after = if cursor.key.is_empty() {
            prefix.clone()
        } else {
            cursor.key.clone()
        };
        let rows = tx
            .prepare(
                "SELECT key,value FROM app_meta WHERE key>=?1 AND key<?2 AND key>?3 ORDER BY key LIMIT ?4",
            )?
            .query_map(params![prefix, upper, after, PAGE], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        examined = rows.len();
        for (key, value) in rows {
            cursor.key.clone_from(&key);
            // Malformed records are left for the existing strict paths.
            let Ok(data) = serde_json::from_str::<Value>(&value) else {
                continue;
            };
            let pending = data.get("state").and_then(Value::as_str) == Some("pending");
            // Auto admissions are automatic by definition; bootstrap work says so.
            let auto = cursor.stage == 2
                || data
                    .get("authorization")
                    .and_then(Value::as_str)
                    .is_some_and(automatic);
            if pending && auto {
                deleted += tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])? as u64;
            }
        }
    }
    if examined < PAGE as usize {
        cursor.stage += 1;
        cursor.key.clear();
    }
    cursor.discarded = cursor.discarded.saturating_add(deleted);
    if deleted > 0 {
        let total = total(&tx)?.saturating_add(deleted);
        set_meta(&tx, TOTAL, &total.to_string())?;
    }
    let done = cursor.stage >= DONE;
    let finished = (done && cursor.discarded > 0).then_some(cursor.discarded);
    set_json_meta(&tx, STATE, &cursor)?;
    tx.commit()?;
    Ok(Step {
        more: !done,
        finished,
    })
}

/// A claim returned to pending while paused; scan the current pause again.
pub(super) fn rearm(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute("DELETE FROM app_meta WHERE key=?1", [STATE])?;
    Ok(())
}

/// Cumulative discarded automatic objects (jobs, bootstrap work, admissions).
pub(super) fn total(db: &Connection) -> Result<u64, StorageError> {
    Ok(meta(db, TOTAL)?.and_then(|s| s.parse().ok()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(db: &Connection, sql: &str) -> i64 {
        db.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn paused_cleanup_is_bounded_restartable_and_keeps_explicit_running_and_other_profiles() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             CREATE TABLE index_space(id TEXT PRIMARY KEY,splitter_version TEXT);
             CREATE TABLE embedding_job(id TEXT PRIMARY KEY,space_id TEXT,state TEXT NOT NULL,authorized_watermark TEXT);
             INSERT INTO app_meta VALUES('index.pause.p','{}');",
        )
        .unwrap();
        let keys = IndexKeys::new("p");
        db.execute(
            "INSERT INTO index_space VALUES('mine',?1),('other','unicode-paragraph-v1:q')",
            [keys.splitter()],
        )
        .unwrap();
        let auto = r#"{"authorization":"new-finalized"}"#;
        for i in 0..300 {
            let id = format!("auto-{i:03}");
            db.execute(
                "INSERT INTO embedding_job VALUES(?1,'mine','pending',?2)",
                params![id, auto],
            )
            .unwrap();
            db.execute(
                "INSERT INTO app_meta VALUES(?1,'input')",
                [IndexKeys::input(&id)],
            )
            .unwrap();
        }
        for (id, space, state, watermark) in [
            (
                "explicit",
                "mine",
                "pending",
                r#"{"authorization":"explicit-backfill"}"#,
            ),
            (
                "rebuild",
                "mine",
                "pending",
                r#"{"authorization":"explicit-rebuild"}"#,
            ),
            ("running", "mine", "running", auto),
            ("failed", "mine", "failed", auto),
            ("foreign", "other", "pending", auto),
        ] {
            db.execute(
                "INSERT INTO embedding_job VALUES(?1,?2,?3,?4)",
                params![id, space, state, watermark],
            )
            .unwrap();
            db.execute(
                "INSERT INTO app_meta VALUES(?1,'input')",
                [IndexKeys::input(id)],
            )
            .unwrap();
        }
        for (key, value) in [
            (
                keys.bootstrap("a"),
                r#"{"state":"pending","authorization":"new-finalized"}"#,
            ),
            (
                keys.bootstrap("b"),
                r#"{"state":"pending","authorization":"explicit-backfill"}"#,
            ),
            (
                keys.auto("n1"),
                r#"{"node_id":"n1","state":"pending","error":null}"#,
            ),
            (
                keys.auto("n2"),
                r#"{"node_id":"n2","state":"failed","error":"x"}"#,
            ),
            (
                IndexKeys::new("q").auto("n3"),
                r#"{"node_id":"n3","state":"pending","error":null}"#,
            ),
        ] {
            db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![key, value])
                .unwrap();
        }
        let first = step(&mut db, "p", None).unwrap();
        assert!(first.more && first.finished.is_none());
        assert_eq!(total(&db).unwrap(), PAGE as u64, "one page per transaction");
        let mut steps = 1;
        let finished = loop {
            steps += 1;
            let next = step(&mut db, "p", None).unwrap();
            if !next.more {
                break next.finished;
            }
        };
        assert!(steps > 3, "more than one job page");
        assert_eq!(finished, Some(302));
        assert_eq!(total(&db).unwrap(), 302);
        assert_eq!(count(&db, "SELECT count(*) FROM embedding_job"), 5);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM app_meta WHERE key LIKE 'index.input.auto-%'"
            ),
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM app_meta WHERE key LIKE 'index.input.%'"
            ),
            5
        );
        assert!(meta(&db, &keys.bootstrap("a")).unwrap().is_none());
        assert!(meta(&db, &keys.bootstrap("b")).unwrap().is_some());
        assert!(meta(&db, &keys.auto("n1")).unwrap().is_none());
        assert!(meta(&db, &keys.auto("n2")).unwrap().is_some());
        assert!(
            meta(&db, &IndexKeys::new("q").auto("n3"))
                .unwrap()
                .is_some()
        );
        let again = step(&mut db, "p", None).unwrap();
        assert!(!again.more && again.finished.is_none());
        // No pause: no work.
        db.execute("DELETE FROM app_meta WHERE key='index.pause.p'", [])
            .unwrap();
        assert!(!step(&mut db, "p", None).unwrap().more);
    }
}
