//! CI enforcement of the requester contract (storage design #171, rule 1):
//! every background kind's normal unit, run on a large synthetic database,
//! uses at most `BACKGROUND_STEP_BUDGET` SQLite VM steps. A sanity scan first
//! proves the fixture is large enough that an unbounded unit exceeds the
//! budget, so a new or regressed unbounded background kind fails here.
use crate::{
    attachments::AttachmentService,
    config::Config,
    control::db::Store,
    gc::GcService,
    index::IndexService,
    protocol::StorageCategory,
    storage::{BACKGROUND_STEP_BUDGET, DbWorker, PROGRESS_GRANULARITY},
};
use rusqlite::params;
use std::{collections::BTreeMap, sync::Arc};

/// Synthetic row counts. Each table is far larger than one bounded unit.
const LIVE_NODES: usize = 20_000;
const DELETED_NODES: usize = 60_000;
const RECEIPTS: usize = 40_000;
const DELIVERIES: usize = 20_000;
const EXECUTIONS: usize = 20_000;
const SUMMARY_PARTS: usize = 10_000;
const DELETED_IMAGES: usize = 10_000;
const READY_IMAGES: usize = 1_000;
const JOBS: usize = 40_000;

fn config(url: &str) -> Config {
    let mut config = Config::default();
    config.connections.insert(
        "fixture".into(),
        crate::config::Connection {
            base_url: url.into(),
            ..Default::default()
        },
    );
    config.embedding.connection = Some("fixture".into());
    config.embedding.model = Some("synthetic".into());
    config.indexing.auto_history = true;
    // Nothing in the fixture is over a cap or past its age: every unit is a
    // normal (healthy) pass over large tables, not a bulk deletion.
    config.retention.tool_raw_bytes = u64::MAX / 4;
    config.retention.clipboard_bytes = u64::MAX / 4;
    config
}

fn seed(db: &mut rusqlite::Connection) {
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    let tx = db.transaction().unwrap();
    let now = chrono::Utc::now();
    let recent = now.to_rfc3339();
    let future = (now + chrono::Duration::days(1)).to_rfc3339();
    tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now');INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2000','2001');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');").unwrap();
    tx.execute("INSERT INTO index_space(id,profile_id,model_id,dimension,splitter_version,state,vec_table_name)VALUES('s','p','m',4,'v','active','vec_s')",[]).unwrap();
    {
        let mut node = tx.prepare("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,deleted_reason)VALUES(?1,'user',?2,?3,?4,?5)").unwrap();
        let payload = format!("{{\"text\":\"{}\"}}", "x".repeat(200));
        for i in 0..LIVE_NODES {
            node.execute(params![
                format!("n{i:06}"),
                recent,
                payload,
                payload.len() as i64,
                None::<String>
            ])
            .unwrap();
        }
        for i in 0..DELETED_NODES {
            node.execute(params![format!("d{i:06}"), recent, None::<String>, 0, "gc"])
                .unwrap();
        }
        let mut receipt = tx.prepare("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES(?1,?2,'{}','h','completed','{\"result\":1}',?3)").unwrap();
        for i in 0..RECEIPTS {
            let instance = if i % 2 == 0 { "fixture" } else { "old" };
            receipt
                .execute(params![format!("op{i:06}"), instance, recent])
                .unwrap();
        }
        let mut delivery = tx.prepare("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at)VALUES(?1,'t','applied',?2,'{}',?3)").unwrap();
        for i in 0..DELIVERIES {
            delivery
                .execute(params![format!("n{i:06}"), format!("op{i:06}"), recent])
                .unwrap();
        }
        let mut execution = tx.prepare("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at)VALUES(?1,'r','u',?1,'exited','/','{}','UTF-8','UTF-8','closed',?2,?2)").unwrap();
        let mut chunk = tx.prepare("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES(?1,0,'stdout',0,x'00ff',?2)").unwrap();
        for i in 0..EXECUTIONS {
            // Distinct end instants, as real executions have.
            let ended = (now - chrono::Duration::milliseconds(i as i64)).to_rfc3339();
            execution
                .execute(params![format!("x{i:06}"), ended])
                .unwrap();
            chunk.execute(params![format!("x{i:06}"), recent]).unwrap();
        }
        let mut meta = tx
            .prepare("INSERT INTO app_meta(key,value)VALUES(?1,?2)")
            .unwrap();
        for i in 0..SUMMARY_PARTS {
            meta.execute(params![
                format!("summary.part.{i:06}"),
                format!("{{\"sources\":[\"n{:06}\"]}}", i % LIVE_NODES)
            ])
            .unwrap();
        }
        let mut image = tx.prepare("INSERT INTO managed_file(id,absolute_path,state,raw_size,created_at)VALUES(?1,?2,?3,10,?4)").unwrap();
        for i in 0..DELETED_IMAGES {
            image
                .execute(params![
                    format!("del{i:06}"),
                    format!("/del/{i}"),
                    "deleted",
                    recent
                ])
                .unwrap();
        }
        for i in 0..READY_IMAGES {
            image
                .execute(params![
                    format!("img{i:06}"),
                    format!("/img/{i}"),
                    "ready",
                    recent
                ])
                .unwrap();
        }
        let mut job = tx.prepare("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,next_attempt_at)VALUES(?1,'s',?2,0,'text','h',?3,?3,'pending',?4)").unwrap();
        for i in 0..JOBS {
            job.execute(params![
                format!("j{i:06}"),
                format!("n{:06}", i % LIVE_NODES),
                i as i64,
                future
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

/// Counted VM steps of one statement, with the same handler granularity.
fn scan_steps(db: &rusqlite::Connection, sql: &str) -> u64 {
    let ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counter = ticks.clone();
    db.progress_handler(
        PROGRESS_GRANULARITY as std::ffi::c_int,
        Some(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            false
        }),
    )
    .unwrap();
    db.query_row(sql, [], |_| Ok(())).unwrap();
    db.progress_handler(0, None::<fn() -> bool>).unwrap();
    ticks.load(std::sync::atomic::Ordering::Relaxed) * PROGRESS_GRANULARITY
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_background_kind_fits_the_step_budget_on_a_large_database() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("shellboy.db");
    // Create the current schema, then seed through a plain connection.
    DbWorker::open(&path)
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    {
        let mut db = rusqlite::Connection::open(&path).unwrap();
        seed(&mut db);
        // The fixture must make unbounded work visible: these full scans of
        // tables that background kinds visit exceed the budget.
        for sql in [
            "SELECT count(*) FROM node WHERE payload_json IS NULL OR deleted_reason IS NOT NULL",
            "SELECT count(*) FROM operation_receipt o WHERE NOT EXISTS(SELECT 1 FROM app_meta m WHERE m.key=o.operation_id)",
            "SELECT sum((SELECT count(*) FROM output_chunk c WHERE c.execution_id=s.id)) FROM shell_execution s",
        ] {
            let steps = scan_steps(&db, sql);
            println!("FIXTURE_SCAN steps={steps} sql={sql}");
            assert!(
                steps > BACKGROUND_STEP_BUDGET,
                "fixture too small: {steps} steps for {sql}"
            );
        }
    }
    let db = DbWorker::open(&path).await.unwrap();
    let config = Arc::new(config("http://127.0.0.1:9/v1"));
    let store = Store::new(
        db.clone(),
        config.clone(),
        root.path().into(),
        "fixture".into(),
    );
    let attachments = Arc::new(AttachmentService::new(
        db.clone(),
        &config,
        root.path().into(),
        "fixture".into(),
    ));
    let gc = GcService::new(db.clone(), config.clone(), root.path().into(), attachments);
    let index = IndexService::new(db.clone(), config.clone());

    let started = std::time::Instant::now();
    let receipts = store.collect_receipts().await.unwrap();
    assert!(!receipts.deferred);
    let report = gc
        .collect(StorageCategory::All, chrono::Utc::now())
        .await
        .unwrap();
    assert!(report.deferred_kinds.is_empty(), "{report:?}");
    index.measure_background_units().await;
    // Paused: one discard_paused unit with the full job backlog present (#173).
    index.pause_for_tests().await;
    index.measure_background_units().await;
    let elapsed = started.elapsed();

    // The worker records each unit after replying; shutdown orders the read.
    db.shutdown().await.unwrap();
    let mut per_kind: BTreeMap<&str, (u64, usize)> = BTreeMap::new();
    for (kind, steps, deferred) in db.take_background_steps() {
        assert!(
            !deferred,
            "no foreground demand, so nothing may be deferred: {kind}"
        );
        let entry = per_kind.entry(kind).or_default();
        entry.0 = entry.0.max(steps);
        entry.1 += 1;
    }
    let units: usize = per_kind.values().map(|v| v.1).sum();
    println!(
        "BACKGROUND_STEPS total_units={units} elapsed_ms={} (fact, not a bound)",
        elapsed.as_millis()
    );
    for (kind, (max, units)) in &per_kind {
        println!("BACKGROUND_STEPS kind={kind} max_steps={max} units={units}");
    }
    for expected in [
        "control::db::Store::collect_receipts",
        "gc::GcService::collect_raw",
        "gc::GcService::collect_held",
        "attachments::AttachmentService::collect_managed_observed",
        "index::identity",
        "index::IndexService::process_key",
    ] {
        assert!(
            per_kind.keys().any(|kind| kind.starts_with(expected)),
            "kind not exercised: {expected}; saw {:?}",
            per_kind.keys().collect::<Vec<_>>()
        );
    }
    for (kind, (max, _)) in per_kind {
        assert!(
            max <= BACKGROUND_STEP_BUDGET,
            "{kind} uses {max} steps per unit, over BACKGROUND_STEP_BUDGET={BACKGROUND_STEP_BUDGET}: split it into bounded units"
        );
    }
}
