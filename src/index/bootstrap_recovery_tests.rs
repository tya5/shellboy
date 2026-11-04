use super::queue::*;
use super::*;
use rusqlite::{OpenFlags, StatementStatus};
use std::sync::atomic::{AtomicUsize, Ordering};

fn recovery_db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE app_meta(key TEXT NOT NULL PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE embedding_job(id TEXT PRIMARY KEY,state TEXT NOT NULL,error_json TEXT); CREATE INDEX jobs_running_recovery ON embedding_job(id) WHERE state='running';").unwrap();
    db
}

fn work(state: &str, text: &str) -> Work {
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
            end: text.len(),
            text: text.into(),
        },
        "g",
        "test",
    );
    work.state = state.into();
    work
}

fn seed(db: &mut Connection, count: usize, work: &Work) {
    let tx = db.transaction().unwrap();
    let value = encode_json(work).unwrap();
    for i in 0..count {
        tx.execute(
            "INSERT INTO app_meta VALUES(?1,?2)",
            params![format!("index.bootstrap.{i:06}"), value],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn bootstrap_seek_is_bounded_on_read_only_connection_at_all_cursor_boundaries() {
    let mut db = recovery_db();
    seed(&mut db, 10_000, &work("pending", "x"));
    for key in [
        "index.bootstrap-",
        "index.bootstrap.",
        "index.bootstrap/",
        "z",
    ] {
        db.execute("INSERT INTO app_meta VALUES(?1,'{}')", [key])
            .unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("synthetic.db");
    db.execute("VACUUM INTO ?1", [path.to_str().unwrap()])
        .unwrap();
    drop(db);
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for (cursor, expected) in [
        (None, Some("index.bootstrap.")),
        (Some(""), Some("index.bootstrap.")),
        (Some("index.bootstrap-"), Some("index.bootstrap.")),
        (Some("index.bootstrap."), Some("index.bootstrap.000000")),
        (
            Some("index.bootstrap.000127"),
            Some("index.bootstrap.000128"),
        ),
        (
            Some("index.bootstrap.009998"),
            Some("index.bootstrap.009999"),
        ),
        (Some("index.bootstrap.009999"), None),
        (Some("index.bootstrap/"), None),
        (Some("z"), None),
    ] {
        let (sql, bound) = bootstrap_recovery_query(cursor);
        let mut statement = db.prepare(sql).unwrap();
        let actual: Option<String> = statement
            .query_row([bound], |r| r.get(0))
            .optional()
            .unwrap();
        let steps = statement.get_status(StatementStatus::VmStep);
        assert_eq!(actual.as_deref(), expected, "cursor={cursor:?}");
        assert!(steps <= 40, "cursor={cursor:?}: {steps} VM steps");
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        eprintln!("bootstrap seek cursor={cursor:?}, key={actual:?}, vm_steps={steps}");
    }
    // Negative control: INDEX and LIMIT 1 alone did not prevent the reported rescan.
    let mut old = db.prepare("SELECT key,octet_length(value) FROM app_meta WHERE key>='index.bootstrap.' AND key>?1 AND key<'index.bootstrap/' ORDER BY key LIMIT 1").unwrap();
    let key: String = old
        .query_row(["index.bootstrap.009998"], |r| r.get(0))
        .unwrap();
    assert_eq!(key, "index.bootstrap.009999");
    assert!(old.get_status(StatementStatus::VmStep) > 10_000);
}

#[test]
fn bootstrap_actual_pages_cannot_reintroduce_prefix_rescans() {
    let mut db = recovery_db();
    seed(&mut db, 10_000, &work("pending", "x"));
    let steps = Arc::new(AtomicUsize::new(0));
    let counted = steps.clone();
    // Guard the real mutation entry point, not a copy of its candidate SQL.
    db.progress_handler(
        100,
        Some(move || counted.fetch_add(100, Ordering::SeqCst) >= 20_000),
    )
    .unwrap();
    for (cursor, expected) in [
        (
            Some("index.bootstrap.000127"),
            Some("index.bootstrap.000255"),
        ),
        (Some("index.bootstrap.009998"), None),
        (Some("index.bootstrap.009999"), None),
    ] {
        steps.store(0, Ordering::SeqCst);
        let (changed, next, done) =
            recover_inflight_page(&mut db, cursor.map(String::from)).unwrap();
        assert!(!changed);
        assert_eq!(next.as_deref(), expected);
        assert_eq!(done, expected.is_none());
        assert!(steps.load(Ordering::SeqCst) < 20_000);
    }
    let mut cursor = None;
    let mut pages = 0;
    loop {
        steps.store(0, Ordering::SeqCst);
        let (changed, next, done) = recover_inflight_page(&mut db, cursor.clone()).unwrap();
        assert!(!changed, "pending work must not change or start paid work");
        pages += 1;
        assert!(steps.load(Ordering::SeqCst) < 20_000);
        if done {
            break;
        }
        assert_eq!(
            next,
            Some(format!("index.bootstrap.{:06}", pages * 128 - 1))
        );
        assert!(next > cursor);
        cursor = next;
    }
    assert_eq!(pages, 79);
    db.progress_handler(0, None::<fn() -> bool>).unwrap();
    // Discarding the ephemeral cursor on restart safely starts at the first page.
    let (_, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
    assert!(!done);
    assert_eq!(cursor.as_deref(), Some("index.bootstrap.000127"));
}

#[test]
fn bootstrap_exact_byte_budget_defers_the_next_record_without_loss() {
    let mut db = recovery_db();
    let mut first = work("pending", "");
    let overhead = encode_json(&first).unwrap().len();
    first.segment.text = "x".repeat(1024 * 1024 - overhead);
    let value = encode_json(&first).unwrap();
    assert_eq!(value.len(), 1024 * 1024);
    db.execute(
        "INSERT INTO app_meta VALUES('index.bootstrap.a',?1)",
        [value],
    )
    .unwrap();
    db.execute(
        "INSERT INTO app_meta VALUES('index.bootstrap.b',?1)",
        [encode_json(&work("pending", "x")).unwrap()],
    )
    .unwrap();
    let (changed, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
    assert!(!changed && !done);
    assert_eq!(cursor.as_deref(), Some("index.bootstrap.a"));
    assert_eq!(
        recover_inflight_page(&mut db, cursor).unwrap(),
        (false, None, true)
    );
}

#[test]
fn bootstrap_oversized_record_gets_its_own_count_and_byte_bounded_page() {
    for count in [1, 128] {
        let mut db = recovery_db();
        // One nearly full record exercises bytes; 128 small records exercise count.
        seed(
            &mut db,
            count,
            &work(
                "pending",
                &"x".repeat(if count == 1 { 1_000_000 } else { 1 }),
            ),
        );
        let key = "index.bootstrap.profile.z";
        db.execute(
            "INSERT INTO app_meta VALUES(?1,?2)",
            params![key, "x".repeat(1024 * 1024 + 1)],
        )
        .unwrap();
        let (changed, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
        assert!(
            !changed && !done,
            "oversized work must not join the ordinary page"
        );
        assert_eq!(cursor, Some(format!("index.bootstrap.{:06}", count - 1)));
        assert!(meta(&db, key).unwrap().is_some());
        let (changed, next, done) = recover_inflight_page(&mut db, cursor).unwrap();
        assert!(changed && !done);
        assert_eq!(next.as_deref(), Some(key));
        assert!(meta(&db, key).unwrap().is_none());
        assert!(
            meta(&db, &IndexKeys::new("profile").recovery_required())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            recover_inflight_page(&mut db, next).unwrap(),
            (false, None, true)
        );
    }
}

#[test]
fn bootstrap_decode_failure_rolls_back_only_the_current_page() {
    let mut db = recovery_db();
    seed(&mut db, 129, &work("running", "x"));
    db.execute(
        "INSERT INTO app_meta VALUES('index.bootstrap.000129','broken')",
        [],
    )
    .unwrap();
    let (changed, cursor, done) = recover_inflight_page(&mut db, None).unwrap();
    assert!(changed && !done);
    assert_eq!(cursor.as_deref(), Some("index.bootstrap.000127"));
    assert!(recover_inflight_page(&mut db, cursor.clone()).is_err());
    let failed: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE json_valid(value) AND json_extract(value,'$.state')='failed'", [], |r| r.get(0)).unwrap();
    assert_eq!(failed, 128);
    let last: Work = json_meta(&db, "index.bootstrap.000128").unwrap().unwrap();
    assert_eq!(last.state, "running");
    // Repair only the synthetic bad row; no earlier committed work is lost.
    db.execute(
        "DELETE FROM app_meta WHERE key='index.bootstrap.000129'",
        [],
    )
    .unwrap();
    assert_eq!(
        recover_inflight_page(&mut db, cursor).unwrap(),
        (true, None, true)
    );
}
