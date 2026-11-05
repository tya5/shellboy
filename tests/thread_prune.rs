//! `thread prune` (#235): release a Thread's hold on originals of adopted
//! summaries and spills; the existing reachability GC reclaims them.
use chrono::{TimeZone, Utc};
use rusqlite::Connection;
use serde_json::{Value, json};
use shellboy::{
    attachments::AttachmentService,
    config::Config,
    control::db::Store,
    gc::GcService,
    history::{get_node, prune::prune_thread, reachability_for_tests},
    protocol::{Command, ErrorCode, HistoryGetArgs, Request, StorageCategory, ThreadPruneArgs},
    storage::DbWorker,
};
use std::sync::Arc;

struct Fixture {
    _temp: tempfile::TempDir,
    db: DbWorker,
    gc: GcService,
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 7, 0, 0, 0).unwrap()
}
fn scalar(db: &Connection, sql: &str) -> rusqlite::Result<i64> {
    db.query_row(sql, [], |r| r.get(0))
}
fn get(node: &str) -> HistoryGetArgs {
    HistoryGetArgs {
        end_message_only: false,
        parts: false,
        node_id: node.into(),
        limit_bytes: 4096,
        cursor: None,
        encoding: "UTF-8".into(),
    }
}

// Threads: live (T, owner of every node's Turn) and fork (F, no Turn nodes).
// Layout of T's path is written by each test; R-rows and sources likewise.
const BASE: &str = "
INSERT INTO run(id,thread_id,state,created_at)VALUES('run','live','completed','now');
INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','live','run','completed','now');";

impl Fixture {
    async fn new(script: &'static str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("shellboy.db"))
            .await
            .unwrap();
        let cwd = temp.path().to_string_lossy().into_owned();
        db.call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('live',?1,?1,'p','m','now'),('fork',?1,?1,'p','m','now')", [cwd])?;
            db.execute_batch(BASE)?;
            db.execute_batch(script)?;
            Ok(())
        })
        .await
        .unwrap();
        let config = Config::default();
        let attachments = Arc::new(AttachmentService::new(
            db.clone(),
            &config,
            temp.path().to_owned(),
            "fixture".into(),
        ));
        let gc = GcService::new(
            db.clone(),
            Arc::new(config),
            temp.path().to_owned(),
            attachments,
        );
        Self {
            _temp: temp,
            db,
            gc,
        }
    }
    async fn prune(
        &self,
        before: &str,
        dry_run: bool,
    ) -> Result<Value, shellboy::protocol::ApiError> {
        let before = before.to_owned();
        self.db
            .call(move |db| {
                let tx = db.transaction()?;
                let result = prune_thread(&tx, "live", &before, dry_run);
                if result.is_ok() {
                    tx.commit()?;
                }
                Ok(result)
            })
            .await
            .unwrap()
    }
    async fn collect(&self) {
        self.gc
            .collect(StorageCategory::History, now())
            .await
            .unwrap();
    }
    async fn alive(&self, ids: &[&str]) -> Vec<bool> {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        self.db
            .call(move |db| {
                Ok(ids
                    .iter()
                    .map(|id| {
                        db.query_row(
                            "SELECT payload_json IS NOT NULL FROM node WHERE id=?1",
                            [id],
                            |r| r.get::<_, bool>(0),
                        )
                        .unwrap()
                    })
                    .collect())
            })
            .await
            .unwrap()
    }
}

fn node(id: &str, kind: &str) -> String {
    format!(
        "INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('{id}','turn','{kind}','1900','{{\"text\":\"{id}-body\"}}',20);"
    )
}
fn nodes(spec: &[(&str, &str)]) -> String {
    spec.iter().map(|(i, k)| node(i, k)).collect()
}
fn path(thread: &str, ids: &[&str]) -> String {
    ids.iter()
        .enumerate()
        .map(|(i, id)| format!("INSERT INTO thread_path VALUES('{thread}',{i},'{id}');"))
        .collect()
}
fn replacement(id: &str, output: &str, sources: &[&str], holders: &[&str]) -> String {
    let mut sql = format!(
        "INSERT INTO replacement VALUES('{id}','{output}','key-{id}','1','1','completed','now');"
    );
    for (i, s) in sources.iter().enumerate() {
        sql += &format!("INSERT INTO replacement_source VALUES('{id}',{i},'{s}',0,NULL);");
    }
    for h in holders {
        sql += &format!("INSERT INTO thread_replacement VALUES('{h}','{id}');");
    }
    sql
}

// Summary R1 over a,b and spill R2 over big; T path: s1, sp, tail.
fn basic() -> &'static str {
    Box::leak(
        [
            nodes(&[
                ("a", "message"),
                ("b", "message"),
                ("big", "tool_result"),
                ("s1", "summary"),
                ("sp", "spill"),
                ("tail", "message"),
            ]),
            path("live", &["s1", "sp", "tail"]),
            replacement("r1", "s1", &["a", "b"], &["live"]),
            replacement("r2", "sp", &["big"], &["live"]),
        ]
        .concat()
        .into_boxed_str(),
    )
}

#[tokio::test]
async fn dry_run_counts_summaries_and_spills_separately_and_changes_nothing() {
    let f = Fixture::new(basic()).await;
    let value = f.prune("tail", true).await.unwrap();
    assert_eq!(value["dry_run"], true);
    assert_eq!(
        value["replacements"],
        json!({"summaries":1,"spills":1,"nested":0})
    );
    assert_eq!(
        value["shared_replacements_kept"],
        json!({"summaries":0,"spills":0})
    );
    assert_eq!(value["source_nodes"], 3);
    assert_eq!(value["source_nodes_on_other_live_paths"], 0);
    let expected =
        f.db.call(|db| {
            Ok(scalar(
                db,
                "SELECT sum(octet_length(payload_json)) FROM node WHERE id IN ('a','b','big')",
            )?)
        })
        .await
        .unwrap();
    assert_eq!(value["candidate_payload_bytes"], expected);
    assert!(value["reclaimable_bytes"].is_null());
    assert!(value["gc_job_id"].is_null());
    f.db.call(|db| {
        assert_eq!(
            scalar(db, "SELECT count(*) FROM replacement WHERE state='pruned'")?,
            0
        );
        assert_eq!(scalar(db, "SELECT count(*) FROM gc_job")?, 0);
        Ok(())
    })
    .await
    .unwrap();
    // The real run reports the same counts and enqueues a normal history job.
    let real = f.prune("tail", false).await.unwrap();
    assert_eq!(real["replacements"], value["replacements"]);
    assert!(real["gc_job_id"].is_string());
    // Boundary: only replacements strictly before NODE; s1 (ordinal 0) is before sp.
    let f2 = Fixture::new(basic()).await;
    let value = f2.prune("sp", true).await.unwrap();
    assert_eq!(
        value["replacements"],
        json!({"summaries":1,"spills":0,"nested":0})
    );
}

#[tokio::test]
async fn prune_releases_summary_and_spill_originals_for_the_existing_gc() {
    let f = Fixture::new(basic()).await;
    assert_eq!(f.alive(&["a", "b", "big"]).await, [true, true, true]);
    // Without prune the history GC keeps every original: the Turn owns them.
    f.db.call(|db| {
        db.execute_batch("INSERT INTO gc_job VALUES('plain','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL)")?;
        Ok(())
    })
    .await
    .unwrap();
    f.collect().await;
    assert_eq!(f.alive(&["a", "b", "big"]).await, [true, true, true]);
    f.prune("tail", false).await.unwrap();
    f.collect().await;
    assert_eq!(f.alive(&["a", "b", "big"]).await, [false, false, false]);
    // Replacement outputs, the later tail and the audit rows stay.
    assert_eq!(f.alive(&["s1", "sp", "tail"]).await, [true, true, true]);
    f.db.call(|db| {
        assert_eq!(
            db.query_row("SELECT deleted_reason FROM node WHERE id='a'", [], |r| {
                r.get::<_, String>(0)
            })?,
            "pruned_unreachable"
        );
        assert_eq!(scalar(db, "SELECT count(*) FROM replacement_source")?, 3);
        // A second prune finds nothing new.
        Ok(())
    })
    .await
    .unwrap();
    let again = f.prune("tail", true).await.unwrap();
    assert_eq!(
        again["replacements"],
        json!({"summaries":0,"spills":0,"nested":0})
    );
    assert_eq!(again["already_pruned_replacements"], 2);
}

#[tokio::test]
async fn shared_replacement_and_shared_originals_survive_and_are_counted() {
    // F forked after adoption (holds s1 and a copy of r1) and F2-like prefix: `a`
    // is also on F's path through an earlier fork point.
    let script = [
        nodes(&[
            ("a", "message"),
            ("b", "message"),
            ("c", "message"),
            ("d", "message"),
            ("s1", "summary"),
            ("s2", "summary"),
            ("tail", "message"),
        ]),
        path("live", &["s1", "s2", "tail"]),
        // r1 is shared with the fork (it holds s1 on its path and registers r1).
        replacement("r1", "s1", &["a", "b"], &["live", "fork"]),
        // r2 is only T's; its source `c` is also on the fork's path, `d` is not.
        replacement("r2", "s2", &["c", "d"], &["live"]),
        path("fork", &["s1", "c"]),
    ]
    .concat();
    let f = Fixture::new(Box::leak(script.into_boxed_str())).await;
    let value = f.prune("tail", true).await.unwrap();
    assert_eq!(
        value["replacements"],
        json!({"summaries":1,"spills":0,"nested":0})
    );
    assert_eq!(
        value["shared_replacements_kept"],
        json!({"summaries":1,"spills":0})
    );
    assert_eq!(value["source_nodes_on_other_live_paths"], 1);
    f.prune("tail", false).await.unwrap();
    f.collect().await;
    // r1 is shared: a and b survive. r2 pruned: c survives on the fork's path, d goes.
    assert_eq!(
        f.alive(&["a", "b", "c", "d"]).await,
        [true, true, true, false]
    );
    assert_eq!(f.alive(&["s1", "s2"]).await, [true, true]);
}

#[tokio::test]
async fn nested_summaries_are_released_together() {
    let script = [
        nodes(&[
            ("a", "message"),
            ("b", "message"),
            ("c", "message"),
            ("s1", "summary"),
            ("s2", "summary"),
            ("tail", "message"),
        ]),
        path("live", &["s2", "tail"]),
        replacement("r1", "s1", &["a", "b"], &["live"]),
        replacement("r2", "s2", &["s1", "c"], &["live"]),
    ]
    .concat();
    let f = Fixture::new(Box::leak(script.into_boxed_str())).await;
    let value = f.prune("tail", true).await.unwrap();
    assert_eq!(
        value["replacements"],
        json!({"summaries":2,"spills":0,"nested":1})
    );
    f.prune("tail", false).await.unwrap();
    f.collect().await;
    assert_eq!(f.alive(&["a", "b", "c"]).await, [false, false, false]);
    // The inner summary is still registered by T, the outer is on its path.
    assert_eq!(f.alive(&["s1", "s2", "tail"]).await, [true, true, true]);
}

#[tokio::test]
async fn nested_inner_summary_held_by_a_fork_keeps_its_originals() {
    let script = [
        nodes(&[
            ("a", "message"),
            ("b", "message"),
            ("c", "message"),
            ("s1", "summary"),
            ("s2", "summary"),
            ("tail", "message"),
        ]),
        path("live", &["s2", "tail"]),
        path("fork", &["s1"]),
        replacement("r1", "s1", &["a", "b"], &["live", "fork"]),
        replacement("r2", "s2", &["s1", "c"], &["live"]),
    ]
    .concat();
    let f = Fixture::new(Box::leak(script.into_boxed_str())).await;
    let value = f.prune("tail", false).await.unwrap();
    assert_eq!(
        value["replacements"],
        json!({"summaries":1,"spills":0,"nested":0})
    );
    assert_eq!(
        value["shared_replacements_kept"],
        json!({"summaries":1,"spills":0})
    );
    f.collect().await;
    assert_eq!(f.alive(&["a", "b", "c"]).await, [true, true, false]);
}

#[tokio::test]
async fn a_pinned_original_survives_a_prune_until_released() {
    let f = Fixture::new(basic()).await;
    f.db.call(|db| {
        db.execute_batch("INSERT INTO runtime_pin VALUES('pin','fixture','node','a',0,'now');")?;
        Ok(())
    })
    .await
    .unwrap();
    f.prune("tail", false).await.unwrap();
    f.collect().await;
    assert_eq!(f.alive(&["a", "b", "big"]).await, [true, false, false]);
    // Releasing the pin lets the next explicit pass reclaim it.
    f.db.call(|db| {
        db.execute_batch("DELETE FROM runtime_pin; INSERT INTO gc_job VALUES('again','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL)")?;
        Ok(())
    })
    .await
    .unwrap();
    f.collect().await;
    assert_eq!(f.alive(&["a"]).await, [false]);
}

#[tokio::test]
async fn a_gc_job_in_progress_before_the_prune_is_conservative_and_the_next_job_reclaims() {
    let f = Fixture::new(basic()).await;
    f.db.call(|db| {
        db.execute_batch("INSERT INTO gc_job VALUES('early','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL)")?;
        Ok(())
    })
    .await
    .unwrap();
    let job = f.gc.start_history().await.unwrap().unwrap();
    loop {
        if f.gc.history_step(&job).await.unwrap().stage == "sweep" {
            break;
        }
    }
    // Prune lands between mark and sweep: this job already marked the originals.
    f.prune("tail", false).await.unwrap();
    while !f.gc.history_step(&job).await.unwrap().complete {}
    assert_eq!(f.alive(&["a", "b", "big"]).await, [true, true, true]);
    // The prune's own job (not yet run) now sees the released holds.
    f.collect().await;
    assert_eq!(f.alive(&["a", "b", "big"]).await, [false, false, false]);
}

#[tokio::test]
async fn fetching_a_pruned_original_fails_clearly_before_and_after_collection() {
    let f = Fixture::new(basic()).await;
    f.db.call(|db| {
        assert!(get_node(db, &get("big")).is_ok());
        Ok(())
    })
    .await
    .unwrap();
    f.prune("tail", false).await.unwrap();
    for collected in [false, true] {
        if collected {
            f.collect().await;
        }
        f.db.call(|db| {
            for id in ["a", "big"] {
                let error = get_node(db, &get(id)).unwrap_err();
                assert_eq!(error.code, ErrorCode::TargetUnavailable);
                assert!(error.message.contains("pruned"), "{}", error.message);
                assert!(
                    !reachability_for_tests(db, id)?,
                    "pruned original must not be searchable"
                );
            }
            // The summary and the spill placeholder remain readable.
            assert!(get_node(db, &get("s1")).is_ok());
            assert!(get_node(db, &get("sp")).is_ok());
            Ok(())
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn a_pruned_original_held_by_another_thread_stays_readable() {
    let script = [
        nodes(&[("a", "message"), ("s1", "summary"), ("tail", "message")]),
        path("live", &["s1", "tail"]),
        path("fork", &["a"]),
        replacement("r1", "s1", &["a"], &["live"]),
    ]
    .concat();
    let f = Fixture::new(Box::leak(script.into_boxed_str())).await;
    f.prune("tail", false).await.unwrap();
    f.collect().await;
    f.db.call(|db| {
        assert!(get_node(db, &get("a")).is_ok());
        assert!(reachability_for_tests(db, "a")?);
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_boundaries_and_threads_are_rejected_without_writes() {
    let f = Fixture::new(basic()).await;
    let error = f.prune("a", false).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    f.db.call(|db| {
        assert_eq!(
            scalar(db, "SELECT count(*) FROM replacement WHERE state='pruned'")?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn command_prunes_the_caller_by_default_and_signals_the_existing_gc() {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("db")).await.unwrap();
    let config = Config::parse(
        r#"
        default_connection="test"
        default_model="fixture"
        [connections.test]
        base_url="http://127.0.0.1:1/v1"
        default_effort="custom"
    "#,
    )
    .unwrap();
    let store = Store::new(
        database,
        Arc::new(config),
        directory.path().join("project"),
        "instance".into(),
    );
    let cwd = directory.path().to_string_lossy().into_owned();
    store
        .database
        .call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('live',?1,?1,'p','m','now')", [cwd])?;
            db.execute_batch(BASE)?;
            db.execute_batch(basic())?;
            Ok(())
        })
        .await
        .unwrap();
    let request = |args: ThreadPruneArgs| {
        Request::new(
            format!("instance:{}", uuid::Uuid::new_v4()),
            Command::ThreadPrune(args),
        )
    };
    store.database.signals().take();
    // An external caller without --thread has no target.
    let error = store
        .execute(
            request(ThreadPruneArgs {
                thread_id: None,
                before_node_id: "tail".into(),
                dry_run: false,
            }),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    // Dry-run neither changes state nor signals.
    let dry = store
        .execute(
            request(ThreadPruneArgs {
                thread_id: Some("live".into()),
                before_node_id: "tail".into(),
                dry_run: true,
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(dry.result["replacements"]["spills"], 1);
    assert!(
        !store
            .database
            .signals()
            .take()
            .contains(shellboy::maintenance::Work::History)
    );
    // The real run signals the same work thread delete does.
    let real = store
        .execute(
            request(ThreadPruneArgs {
                thread_id: Some("live".into()),
                before_node_id: "tail".into(),
                dry_run: false,
            }),
            None,
        )
        .await
        .unwrap();
    assert!(real.result["gc_job_id"].is_string());
    let marked = store.database.signals().take();
    for work in shellboy::maintenance::Work::DELETION {
        assert!(marked.contains(work), "{work:?} not signalled by prune");
    }
    store
        .database
        .call(|db| {
            assert_eq!(
                scalar(
                    db,
                    "SELECT count(*) FROM gc_job WHERE kind='history' AND state='pending'"
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
}
