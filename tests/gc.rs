use chrono::{TimeZone, Utc};
use rusqlite::{Connection, params};
use shellboy::{
    attachments::AttachmentService,
    config::Config,
    gc::{GcService, require_capacity_tx},
    protocol::{ErrorCode, StorageCategory},
    storage::DbWorker,
};
use std::sync::Arc;

struct Fixture {
    _temp: tempfile::TempDir,
    db: DbWorker,
    gc: GcService,
}
impl Fixture {
    async fn new(config: Config) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("shellboy.db"))
            .await
            .unwrap();
        let cwd = temp.path().to_string_lossy().into_owned();
        db.call(move|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('live',?1,?1,'p','m','now'),('deleted',?1,?1,'p','m','now')",[cwd])?;db.execute_batch("UPDATE thread SET deleted_at='now' WHERE id='deleted'; INSERT INTO run(id,thread_id,state,created_at)VALUES('run','live','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','live','run','completed','now');")?;Ok(())}).await.unwrap();
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
    async fn raw(&self, id: &str, state: &str, date: &str, bytes: Vec<u8>, pinned: bool) {
        let (id, state, date) = (id.to_owned(), state.to_owned(), date.to_owned());
        self.db.call(move|db|{db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,started_at,ended_at)VALUES(?1,'run','turn',?1,?2,'/','{}','UTF-8','UTF-8','closed',1,?3,?3)",params![id,state,date])?;db.execute("INSERT INTO output_chunk VALUES(?1,1,'stdout',0,?2,?3)",params![id,bytes,date])?;if pinned{db.execute("INSERT INTO runtime_pin VALUES(?1,'fixture','shell_output',?1,0,'now')",[id])?;}Ok(())}).await.unwrap();
    }
    async fn node(&self, id: &str, turn: Option<&str>) {
        let (id, turn) = (id.to_owned(), turn.map(str::to_owned));
        self.db.call(move|db|{db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'message','1900-01-01T00:00:00Z','{\"text\":\"original\"}',19)",params![id,turn])?;Ok(())}).await.unwrap();
    }
    async fn job(&self) {
        self.db.call(|db|{db.execute("INSERT INTO gc_job VALUES('job','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL)",[])?;Ok(())}).await.unwrap();
    }
}
fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap()
}
fn scalar(db: &Connection, sql: &str) -> rusqlite::Result<i64> {
    db.query_row(sql, [], |r| r.get(0))
}

#[tokio::test]
async fn received_audits_survive_live_thread_gc_then_release_all_paid_channels_on_deletion() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        let body = serde_json::json!({"attempt_id":"audit-attempt","partial_text":"paid answer","partial_reasoning":"paid reasoning","completed_items":{"0":{"text":"done"}},"raw_response":{"output":[]},"error":{"kind":"protocol"}}).to_string();
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('audit-attempt','turn',0,0,'p','m',0,'incomplete','{}',?1,'now')",[&body])?;
        for (id,kind) in [("failed-audit","incomplete_response"),("successful-audit","model_stream_observation")] {
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'turn',?2,'now',?3,length(?3))",params![id,kind,body])?;
        }
        Ok(())
    }).await.unwrap();
    f.job().await;
    assert_eq!(
        f.gc.collect(StorageCategory::History, now())
            .await
            .unwrap()
            .nodes_tombstoned,
        0
    );
    f.db.call(|db| {
        assert_eq!(scalar(db,"SELECT count(*) FROM node WHERE payload_json IS NOT NULL")?,2);
        assert_eq!(scalar(db,"SELECT count(*) FROM thread_path")?,0);
        db.execute_batch("UPDATE thread SET deleted_at='now' WHERE id='live'; UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'; INSERT INTO gc_job VALUES('after-delete','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL);")?;
        Ok(())
    }).await.unwrap();
    assert_eq!(
        f.gc.collect(StorageCategory::History, now())
            .await
            .unwrap()
            .nodes_tombstoned,
        2
    );
    f.db.call(|db| {
        assert_eq!(scalar(db,"SELECT count(*) FROM node WHERE payload_json IS NULL AND deleted_reason='thread_deleted_unreachable'")?,2);
        let saved:String=db.query_row("SELECT error_json FROM model_attempt WHERE id='audit-attempt'",[],|r|r.get(0))?;
        let saved:serde_json::Value=serde_json::from_str(&saved).unwrap();
        for key in ["partial_text","partial_reasoning","completed_items","raw_response"] {
            assert!(saved.get(key).is_none(),"retained paid channel {key}");
        }
        assert_eq!(saved["attempt_id"],"audit-attempt");
        assert_eq!(saved["error"]["kind"],"protocol");
        Ok(())
    }).await.unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn raw_retention_respects_live_and_pins_then_expires_durably() {
    let mut config = Config::default();
    config.retention.tool_raw_bytes = 10;
    let f = Fixture::new(config).await;
    f.raw("old", "exited", "2020-01-01T00:00:00Z", vec![1; 8], false)
        .await;
    f.raw("pinned", "exited", "2020-01-01T00:00:00Z", vec![2; 8], true)
        .await;
    f.raw(
        "running",
        "running",
        "2020-01-01T00:00:00Z",
        vec![3; 8],
        false,
    )
    .await;
    f.raw("new", "exited", "2026-10-24T00:00:00Z", vec![4; 8], false)
        .await;
    let report = f.gc.collect(StorageCategory::Raw, now()).await.unwrap();
    assert_eq!(report.raw_bytes_collected, 16);
    assert_eq!(report.protected_raw_bytes, 16);
    assert!(report.deferred);
    f.db.call(|db| {
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM output_chunk")?, 2);
        assert_eq!(
            scalar(
                db,
                "SELECT retention_generation FROM shell_execution WHERE id='old'"
            )?,
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT json_extract(value,'$.state') FROM app_meta WHERE key='raw_tombstone.old'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "deleted"
        );
        db.execute("DELETE FROM runtime_pin", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.gc.collect(StorageCategory::Raw, now())
            .await
            .unwrap()
            .raw_bytes_collected,
        8
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_raw_collection_resumes_marker_without_incrementing_generation_twice() {
    let f = Fixture::new(Config::default()).await;
    f.raw("raw", "exited", "2020-01-01T00:00:00Z", vec![7; 32], false)
        .await;
    f.db.call(|db|{for sequence in 2..=129 { db.execute("INSERT INTO output_chunk VALUES('raw',?1,'stdout',?1,x'010203','2020-01-01T00:00:00Z')",[sequence])?; } db.execute_batch("CREATE TRIGGER fail_raw BEFORE DELETE ON output_chunk WHEN OLD.sequence=129 BEGIN SELECT RAISE(ABORT,'injected'); END;")?;Ok(())}).await.unwrap();
    assert!(f.gc.collect(StorageCategory::Raw, now()).await.is_err());
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT retention_generation FROM shell_execution WHERE id='raw'"
            )?,
            1
        );
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM output_chunk")?, 1);
        assert_eq!(
            db.query_row(
                "SELECT json_extract(value,'$.state') FROM app_meta WHERE key='raw_tombstone.raw'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "deleting"
        );
        db.execute_batch("DROP TRIGGER fail_raw")?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.gc.collect(StorageCategory::Raw, now())
            .await
            .unwrap()
            .raw_bytes_collected,
        3
    );
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT retention_generation FROM shell_execution WHERE id='raw'"
            )?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_keeps_shared_prefix_recursive_sources_live_creator_and_pending_receipts() {
    let f = Fixture::new(Config::default()).await;
    for id in [
        "prefix",
        "deleted_suffix",
        "summary",
        "nested_summary",
        "source",
        "pending",
        "pinned",
    ] {
        f.node(id, None).await;
    }
    f.node("unselected_live", Some("turn")).await;
    f.db.call(|db|{db.execute_batch("INSERT INTO thread_path VALUES('live',0,'prefix'),('live',1,'summary'),('deleted',0,'prefix'),('deleted',1,'deleted_suffix'); INSERT INTO edge VALUES('prefix','deleted_suffix'); INSERT INTO replacement VALUES('r1','summary','1','1','1','adopted','now'),('r2','nested_summary','2','1','1','adopted','now'); INSERT INTO replacement_source VALUES('r1',0,'nested_summary',0,NULL),('r2',0,'source',0,NULL); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES('receipt','op','{}','deleted','pending','held','now',json_object('reply',json_object('state','pending'))); INSERT INTO runtime_pin VALUES('pin','fixture','node','pinned',0,'now');")?;Ok(())}).await.unwrap();
    // No explicit deletion job: even unreachable old payloads are not collected.
    assert_eq!(
        f.gc.collect(StorageCategory::History, now())
            .await
            .unwrap()
            .nodes_tombstoned,
        0
    );
    f.job().await;
    let report = f.gc.collect(StorageCategory::History, now()).await.unwrap();
    assert_eq!(report.nodes_tombstoned, 1);
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE payload_json IS NULL AND id='deleted_suffix'"
            )?,
            1
        );
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL"
            )?,
            7
        );
        assert_eq!(
            scalar(db, "SELECT COUNT(*) FROM edge WHERE dst='deleted_suffix'")?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn root_revision_change_revalidates_before_destructive_batch() {
    let f = Fixture::new(Config::default()).await;
    f.node("late", None).await;
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    loop {
        let step = f.gc.history_step(&job).await.unwrap();
        if step.stage == "sweep" {
            break;
        }
    }
    f.db.call(|db|{db.execute_batch("INSERT INTO thread_path VALUES('live',0,'late'); UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision';")?;Ok(())}).await.unwrap();
    let restarted = f.gc.history_step(&job).await.unwrap();
    assert!(
        !restarted.restarted,
        "a root change must not discard bounded progress"
    );
    assert_eq!(restarted.nodes_tombstoned, 0);
    loop {
        if f.gc.history_step(&job).await.unwrap().complete {
            break;
        }
    }
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL"
            )?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_vectors_and_node_tombstone_roll_back_together_and_duplicate_payload_is_cleared() {
    let f = Fixture::new(Config::default()).await;
    f.node("dead", None).await;
    f.db.call(|db|{db.execute_batch("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','turn',0,0,'p','m',0,'completed','{}','now'); INSERT INTO output_item VALUES('attempt',0,NULL,'message',NULL,'completed','{\"secret\":\"duplicate\"}','dead'); CREATE VIRTUAL TABLE shellboy_vec_00000000000000000000000000000000 USING vec0(embedding float[2]); INSERT INTO index_space VALUES('space','p','m',2,'1','inactive','shellboy_vec_00000000000000000000000000000000'); INSERT INTO shellboy_vec_00000000000000000000000000000000(rowid,embedding)VALUES(1,'[1,2]'); INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state)VALUES('embed','space','dead',0,'text','hash',0,2,'completed'); INSERT INTO embedding_chunk VALUES('chunk','embed','space','dead',0,0,2,'hash',1); CREATE TRIGGER fail_node BEFORE UPDATE OF payload_json ON node WHEN NEW.payload_json IS NULL BEGIN SELECT RAISE(ABORT,'injected'); END;")?;Ok(())}).await.unwrap();
    f.job().await;
    assert!(f.gc.collect(StorageCategory::History, now()).await.is_err());
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM shellboy_vec_00000000000000000000000000000000"
            )?,
            1
        );
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM embedding_chunk")?, 1);
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL"
            )?,
            1
        );
        db.execute_batch("DROP TRIGGER fail_node")?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.gc.collect(StorageCategory::History, now())
            .await
            .unwrap()
            .nodes_tombstoned,
        1
    );
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM shellboy_vec_00000000000000000000000000000000"
            )?,
            0
        );
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM embedding_chunk")?, 0);
        assert_eq!(
            scalar(db, "SELECT payload_revision FROM node WHERE id='dead'")?,
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT json_extract(payload_json,'$.type') FROM output_item",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "shellboy_tombstone"
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn logical_capacity_and_physical_database_space_are_reported_separately() {
    let mut config = Config::default();
    config.limits.data_max_bytes = 1;
    config.limits.data_warning_bytes = 1;
    let f = Fixture::new(config.clone()).await;
    f.node("live", Some("turn")).await;
    let status = f.gc.details().await.unwrap();
    assert!(status.status.logical_live_bytes > 0);
    assert!(status.status.db_bytes > 0);
    assert!(status.warning && status.limit_reached);
    assert!(status.reclaimable_is_lower_bound);
    assert_eq!(status.status.reclaimable_bytes, 0);
    assert_eq!(
        f.gc.require_capacity().await.unwrap_err().code,
        ErrorCode::StorageLimit
    );
    f.db.call(move |db| {
        assert_eq!(
            require_capacity_tx(db, &config).unwrap_err().code,
            ErrorCode::StorageLimit
        );
        Ok(())
    })
    .await
    .unwrap();
    // Quota only gates admission; collection and observation still work.
    assert_eq!(
        f.gc.collect(StorageCategory::All, now())
            .await
            .unwrap()
            .nodes_tombstoned,
        0
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn persisted_mark_progress_after_reopen_is_recomputed_before_sweep() {
    let f = Fixture::new(Config::default()).await;
    f.node("keep", None).await;
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    assert!(!f.gc.history_step(&job).await.unwrap().complete);
    f.db.shutdown().await.unwrap();
    let db = DbWorker::open(f._temp.path().join("shellboy.db"))
        .await
        .unwrap();
    db.call(|db| {
        db.execute_batch("INSERT INTO thread_path VALUES('live',0,'keep');")?;
        Ok(())
    })
    .await
    .unwrap();
    let config = Config::default();
    let attachments = Arc::new(AttachmentService::new(
        db.clone(),
        &config,
        f._temp.path().to_owned(),
        "reopen".into(),
    ));
    let gc = GcService::new(
        db.clone(),
        Arc::new(config),
        f._temp.path().to_owned(),
        attachments,
    );
    // Even without root revision change, absence of TEMP marks invalidates a persisted plan.
    assert!(gc.history_step(&job).await.unwrap().restarted);
    loop {
        if gc.history_step(&job).await.unwrap().complete {
            break;
        }
    }
    db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL"
            )?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn pin_added_after_marking_protects_summary_and_recursive_source() {
    let f = Fixture::new(Config::default()).await;
    f.node("summary", None).await;
    f.node("source", None).await;
    f.db.call(|db|{db.execute_batch("INSERT INTO replacement VALUES('r','summary','input','1','1','cached','now'); INSERT INTO replacement_source VALUES('r',0,'source',0,NULL);")?;Ok(())}).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    loop {
        if f.gc.history_step(&job).await.unwrap().stage == "sweep" {
            break;
        }
    }
    f.db.call(|db| {
        db.execute_batch(
            "INSERT INTO runtime_pin VALUES('late','fixture','node','summary',0,'now');",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let step = f.gc.history_step(&job).await.unwrap();
    assert_eq!(step.stage, "cleanup");
    assert_eq!(step.nodes_tombstoned, 0);
    while !f.gc.history_step(&job).await.unwrap().complete {}
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL"
            )?,
            2
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn summary_parts_count_toward_quota_and_explicitly_deleted_sources_invalidate_cache() {
    let f = Fixture::new(Config::default()).await;
    f.node("shared", None).await;
    f.node("deleted_source", None).await;
    f.db.call(|db| {
        db.execute_batch("INSERT INTO thread_path VALUES('live',0,'shared');")?;
        Ok(())
    })
    .await
    .unwrap();
    let before = f.gc.details().await.unwrap().logical_history_bytes;
    let live = serde_json::json!({"text":"live successful partial summary","sources":["shared"]})
        .to_string();
    let mixed=serde_json::json!({"text":"mixed source cached summary","sources":["shared","deleted_source"]}).to_string();
    let expected = live.len() + mixed.len();
    let (a, b) = (live, mixed);
    f.db.call(move |db| {
        db.execute(
            "INSERT INTO app_meta VALUES('summary.part.live',?1),('summary.part.mixed',?2)",
            params![a, b],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.gc.details().await.unwrap().logical_history_bytes - before,
        expected as u64
    );
    // Partial successful summaries remain reusable while original source is live.
    assert_eq!(
        f.gc.collect(StorageCategory::History, now())
            .await
            .unwrap()
            .summary_cache_parts_collected,
        0
    );
    f.job().await;
    let result = f.gc.collect(StorageCategory::History, now()).await.unwrap();
    assert_eq!(result.nodes_tombstoned, 1);
    assert_eq!(result.summary_cache_parts_collected, 1);
    assert!(result.summary_cache_bytes_collected > 0);
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM app_meta WHERE key='summary.part.live'"
            )?,
            1
        );
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM app_meta WHERE key='summary.part.mixed'"
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_wait_metadata_counts_toward_logical_history_until_observer_release() {
    let f = Fixture::new(Config::default()).await;
    let before = f.gc.details().await.unwrap().status.logical_live_bytes;
    let body =
        serde_json::json!({"conditions":[{"kind":"run_ended","run_id":"fixture"}],"caller":null})
            .to_string();
    let bytes = body.len() as u64;
    f.db.call(move |db| {
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('wait.pending.fixture',?1)",
            [body],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.gc.details().await.unwrap().status.logical_live_bytes,
        before + bytes
    );
    f.db.call(|db| {
        db.execute("DELETE FROM app_meta WHERE key='wait.pending.fixture'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.gc.details().await.unwrap().status.logical_live_bytes,
        before
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn detached_pending_attachment_sources_are_roots_until_cancelled_or_target_deleted() {
    let f = Fixture::new(Config::default()).await;
    for id in ["accepted", "held", "cancelled", "deleted_target"] {
        f.node(id, None).await;
    }
    f.db.call(|db| {
        for (node,thread,status) in [("accepted","live","accepted"),("held","live","held"),("cancelled","live","cancelled"),("deleted_target","deleted","held")] {
            db.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at)VALUES(?1,?2,?3,?1,'{}','now')",params![node,thread,status])?;
        }
        Ok(())
    }).await.unwrap();
    f.job().await;
    let report = f.gc.collect(StorageCategory::History, now()).await.unwrap();
    assert_eq!(report.nodes_tombstoned, 2);
    f.db.call(|db| {
        assert_eq!(scalar(db,"SELECT count(*) FROM node WHERE id IN ('accepted','held') AND payload_json IS NOT NULL")?,2);
        assert_eq!(scalar(db,"SELECT count(*) FROM node WHERE id IN ('cancelled','deleted_target') AND payload_json IS NULL")?,2);
        Ok(())
    }).await.unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn compact_downstream_results_remain_part_of_quota_after_acceptance_is_frozen() {
    let f = Fixture::new(Config::default()).await;
    let before = f.gc.details().await.unwrap().status.logical_live_bytes;
    let body = serde_json::json!({"kind":"compact","state":"completed","outcome":{"result":{"summary_node_id":"synthetic-summary","notes":"結果".repeat(200)}}}).to_string();
    let bytes = body.len() as u64;
    f.db.call(move |db| {
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('compact.result.synthetic-operation',?1)",
            [body],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.gc.details().await.unwrap().status.logical_live_bytes,
        before + bytes
    );
    let limit = before + bytes - 1;
    f.db.call(move |db| {
        let tx = db.transaction()?;
        let mut config = Config::default();
        config.limits.data_max_bytes = limit;
        let failure = require_capacity_tx(&tx, &config).unwrap_err();
        assert_eq!(failure.code, ErrorCode::StorageLimit);
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn collection_report_bounds_details_while_retaining_actual_deleted_and_failed_totals() {
    let f = Fixture::new(Config::default()).await;
    let root = f._temp.path().to_owned();
    f.db.call(move|db|{
        let tx=db.transaction()?;
        for n in 0..130 {
            let id=format!("deleted-{n}");
            let path=root.join("managed/clipboard").join(format!("{id}.png")).to_string_lossy().into_owned();
            tx.execute("INSERT INTO managed_file(id,absolute_path,state,raw_size,created_at)VALUES(?1,?2,'failed',0,'2000-01-01')",params![id,path])?;
            tx.execute("INSERT INTO managed_file(id,absolute_path,state,raw_size,created_at)VALUES(?1,?2,'failed',0,'2000-01-01')",params![format!("failed-{n}"),root.join(format!("outside-{n}.png")).to_string_lossy().into_owned()])?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    let result =
        f.gc.collect(StorageCategory::ManagedImages, Utc::now())
            .await
            .unwrap();
    assert_eq!(result.managed_deleted_count, 130);
    assert_eq!(result.managed_failure_count, 130);
    assert_eq!(result.managed_deleted.len(), 128);
    assert_eq!(result.managed_failures.len(), 128);
    assert!(result.managed_details_truncated);
    assert!(serde_json::to_vec(&result).unwrap().len() < 512 * 1024);
    f.db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM managed_file WHERE state='deleted'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            130
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "controlled SQLite GC progress measurement; temporary stores only"]
async fn measure_history_gc_progress_during_root_writes() {
    for nodes in [10_000_i64, 100_000] {
        for write_every in [0_usize, 16, 1] {
            let f = Fixture::new(Config::default()).await;
            f.db.call(move |db| {
                let tx=db.transaction()?;
                tx.execute("WITH RECURSIVE numbers(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM numbers WHERE i<?1) INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) SELECT 'garbage-'||i,'user','now','{\"text\":\"original\"}',19 FROM numbers",[nodes])?;
                tx.commit()?;Ok(())
            }).await.unwrap();
            f.job().await;
            let job = f.gc.start_history().await.unwrap().unwrap();
            let start = std::time::Instant::now();
            let (mut steps, mut scanned, mut restarts, mut collected, mut complete) =
                (0, 0, 0, 0, false);
            while steps < 4000 && restarts < 3 {
                if write_every > 0 && steps % write_every == 0 {
                    f.db.call(move |db| {
                        let tx = db.transaction()?;
                        shellboy::history::append_node(
                            &tx,
                            "live",
                            &format!("writer-{steps}"),
                            Some("turn"),
                            "user",
                            &serde_json::json!({"text":"live work"}),
                            "now",
                        )?;
                        tx.commit()?;
                        Ok(())
                    })
                    .await
                    .unwrap();
                }
                let step = f.gc.history_step(&job).await.unwrap();
                steps += 1;
                scanned += step.scanned;
                restarts += usize::from(step.restarted);
                collected += step.nodes_tombstoned;
                if step.complete {
                    complete = true;
                    break;
                }
            }
            println!(
                "GC_PROGRESS nodes={nodes} write_every={write_every} steps={steps} scanned={scanned} restarts={restarts} collected={collected} complete={complete} elapsed_ms={}",
                start.elapsed().as_millis()
            );
            assert!(
                complete,
                "finite original node set must finish despite new roots"
            );
            assert_eq!(collected, nodes as u64);
            f.db.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn history_gc_finishes_its_original_horizon_while_new_roots_keep_arriving() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("WITH RECURSIVE numbers(i) AS(SELECT 1 UNION ALL SELECT i+1 FROM numbers WHERE i<1024) INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) SELECT 'unreachable-'||i,'user','now','{\"text\":\"garbage\"}',18 FROM numbers",[])?;Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    let (mut complete, mut collected) = (false, 0);
    for step in 0..64 {
        f.db.call(move |db| {
            let tx = db.transaction()?;
            shellboy::history::append_node(
                &tx,
                "live",
                &format!("arriving-{step}"),
                Some("turn"),
                "user",
                &serde_json::json!({"text":"new protected work"}),
                "now",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            f.gc.start_history().await.unwrap().as_deref(),
            Some(job.as_str())
        );
        let result = f.gc.history_step(&job).await.unwrap();
        collected += result.nodes_tombstoned;
        if result.complete {
            complete = true;
            break;
        }
    }
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE id LIKE 'arriving-%' AND payload_json IS NULL"
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
    assert!(
        complete,
        "continuous root writes must not reset the finite horizon forever"
    );
    assert_eq!(collected, 1024);
}

#[tokio::test]
async fn changed_roots_protect_old_recursive_sources_and_leave_later_jobs_pending() {
    let f = Fixture::new(Config::default()).await;
    for id in ["source", "nested", "garbage"] {
        f.node(id, None).await;
    }
    f.job().await;
    f.db.call(|db| {db.execute("INSERT INTO gc_job VALUES('same-horizon','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL)",[])?;Ok(())}).await.unwrap();
    let job = f.gc.start_history().await.unwrap().unwrap();
    while f.gc.history_step(&job).await.unwrap().stage != "sweep" {}
    f.db.call(|db| {
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('summary','summary','now','{\"text\":\"summary\"}',18); INSERT INTO replacement VALUES('outer','summary','1','1','1','adopted','now'),('inner','nested','2','1','1','adopted','now'); INSERT INTO replacement_source VALUES('outer',0,'nested',0,NULL),('inner',0,'source',0,NULL); INSERT INTO thread_path VALUES('live',0,'summary'); UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'; INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('later-debris','user','now','{\"text\":\"late\"}',15); INSERT INTO gc_job VALUES('later-job','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL);")?;
        Ok(())
    }).await.unwrap();
    let result = f.gc.history_step(&job).await.unwrap();
    assert_eq!(result.nodes_tombstoned, 1);
    while !f.gc.history_step(&job).await.unwrap().complete {}
    f.db.call(|db| {
        assert_eq!(scalar(db,"SELECT COUNT(*) FROM node WHERE id IN ('source','nested','summary','later-debris') AND payload_json IS NOT NULL")?,4);
        assert_eq!(scalar(db,"SELECT COUNT(*) FROM gc_job WHERE id IN ('job','same-horizon') AND state='done'")?,2);
        assert_eq!(db.query_row("SELECT state FROM gc_job WHERE id='later-job'",[],|r|r.get::<_,String>(0))?,"pending");
        Ok(())
    }).await.unwrap();
    assert_eq!(
        f.gc.collect(StorageCategory::History, now())
            .await
            .unwrap()
            .nodes_tombstoned,
        1
    );
    f.db.call(|db| {assert_eq!(scalar(db,"SELECT COUNT(*) FROM node WHERE id IN ('source','nested','summary') AND payload_json IS NOT NULL")?,3);Ok(())}).await.unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn late_pins_keep_sweep_advancing_and_protect_sources_beyond_the_pin_batch() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("WITH RECURSIVE numbers(i) AS(SELECT 1 UNION ALL SELECT i+1 FROM numbers WHERE i<1024) INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) SELECT 'garbage-'||i,'user','now','{}',2 FROM numbers",[])?;
        Ok(())
    }).await.unwrap();
    f.node("source", None).await;
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    while f.gc.history_step(&job).await.unwrap().stage != "sweep" {}
    let mut collected = f.gc.history_step(&job).await.unwrap().nodes_tombstoned;
    assert_eq!(collected, 128);
    let mut complete = false;
    let mut previous = 128;
    for batch in 0..32 {
        f.db.call(move |db| {
            let tx = db.transaction()?;
            for i in 0..140 {
                let id = format!("pin-{batch}-{i}");
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES(?1,'summary','now','{}',2)",[&id])?;
                tx.execute("INSERT INTO replacement VALUES(?1,?1,'input','1','1','cached','now')",[&id])?;
                tx.execute("INSERT INTO replacement_source VALUES(?1,0,'source',0,NULL)",[&id])?;
                tx.execute("INSERT INTO runtime_pin VALUES(?1,'fixture','node',?1,0,'now')",[&id])?;
            }
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        assert_eq!(
            f.gc.start_history().await.unwrap().as_deref(),
            Some(job.as_str())
        );
        let step = f.gc.history_step(&job).await.unwrap();
        collected += step.nodes_tombstoned;
        if step.complete {
            complete = true;
            break;
        }
        if step.stage == "cleanup" || step.stage == "finish" {
            continue;
        }
        assert_eq!(step.stage, "sweep");
        let job_id = job.clone();
        let cursor =
            f.db.call(move |db| {
                Ok(db.query_row(
                    "SELECT json_extract(cursor_json,'$.last_rowid') FROM gc_job WHERE id=?1",
                    [job_id],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert!(
            cursor > previous,
            "new pins must not rewind the sweep cursor"
        );
        previous = cursor;
    }
    assert!(complete);
    assert_eq!(collected, 1024);
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE id='source' AND payload_json IS NOT NULL"
            )?,
            1
        );
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE id LIKE 'pin-%' AND payload_json IS NULL"
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_cursor_rebuilds_marks_before_resuming() {
    let f = Fixture::new(Config::default()).await;
    f.node("keep", None).await;
    f.node("garbage", None).await;
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    f.db.call(|db| {
        db.execute_batch("INSERT INTO thread_path VALUES('live',0,'keep'); UPDATE gc_job SET cursor_json='{\"stage\":\"sweep\",\"last_rowid\":9999}' WHERE id='job';")?;
        Ok(())
    }).await.unwrap();
    assert_eq!(
        f.gc.start_history().await.unwrap().as_deref(),
        Some(job.as_str())
    );
    let mut collected = 0;
    loop {
        let step = f.gc.history_step(&job).await.unwrap();
        collected += step.nodes_tombstoned;
        if step.complete {
            break;
        }
    }
    assert_eq!(collected, 1);
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE id='keep' AND payload_json IS NOT NULL"
            )?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_maintenance_collects_dead_deliveries_without_losing_history_or_raw() {
    let f = Fixture::new(Config::default()).await;
    f.raw(
        "raw-preserved",
        "exited",
        "2020-01-01T00:00:00Z",
        vec![7; 8],
        false,
    )
    .await;
    f.db.call(|db| {
        db.execute_batch(r#"
            INSERT INTO run(id,thread_id,state,created_at)VALUES('live-run','live','live','now');
            INSERT INTO event(id,kind,source_json,target_thread_id,target_run_id,payload_ref_json,created_at)VALUES
                ('context','context_changed','{}','live',NULL,'{}','now'),
                ('disposable','shell_ended','{}','live','run','{}','now'),
                ('referenced','shell_ended','{}','live','run','{}','now'),
                ('active','shell_ended','{}','live','live-run','{}','now');
            INSERT INTO delivery(event_id,recipient_id,state)VALUES
                ('context','run','pending'),('disposable','run','processed'),
                ('referenced','run','processed'),('active','live-run','pending');
            INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES
                ('notification','turn','event','now','{"event_id":"referenced"}');
        "#)?;
        Ok(())
    }).await.unwrap();
    f.gc.collect(StorageCategory::History, now()).await.unwrap();
    f.db.call(|db| {
        assert_eq!(
            scalar(db, "SELECT count(*) FROM delivery WHERE event_id='context'")?,
            0
        );
        assert_eq!(
            scalar(db, "SELECT count(*) FROM event WHERE id='context'")?,
            1
        );
        assert_eq!(
            scalar(db, "SELECT count(*) FROM event WHERE id='disposable'")?,
            0
        );
        assert_eq!(
            scalar(db, "SELECT count(*) FROM event WHERE id='referenced'")?,
            1
        );
        assert_eq!(
            scalar(
                db,
                "SELECT count(*) FROM delivery WHERE event_id='active' AND state='pending'"
            )?,
            1
        );
        assert_eq!(
            scalar(
                db,
                "SELECT count(*) FROM output_chunk WHERE execution_id='raw-preserved'"
            )?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn deleted_receipts_are_collected_in_finite_batches_but_live_parties_and_pending_are_roots() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        for i in 0..260 {
            let id=format!("receipt-{i:03}");
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{\"text\":\"synthetic\"}')",[&id])?;
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES(?1,?1,'null','deleted',?1,'cancelled','now','{}')",[id])?;
        }
        for (id,target,sender,state) in [
            ("live-target","live","null","replied"),
            ("live-sender","deleted",r#"{"kind":"model","thread_id":"live"}"#,"recipient_deleted"),
            ("pending","deleted","null","pending"),
        ] {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')",[id])?;
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES(?1,?1,?2,?3,?1,'applied','now',?4)",params![id,sender,target,serde_json::json!({"reply":{"state":state}}).to_string()])?;
        }
        db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('reply-live-sender','user','now','{}')",[])?;
        db.execute("UPDATE receipt SET result_json=json_set(result_json,'$.reply.state','replied','$.reply.node_id','reply-live-sender') WHERE id='live-sender'",[])?;
        db.execute("INSERT INTO gc_job(id,kind,root_revision,state)VALUES('receipt-gc','history',0,'pending')",[])?;
        Ok(())
    }).await.unwrap();
    let job = f.gc.start_history().await.unwrap().unwrap();
    let mut previous = 263;
    for _ in 0..100 {
        let step = f.gc.history_step(&job).await.unwrap();
        assert!(step.scanned <= 128);
        let count =
            f.db.call(|db| {
                Ok(db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?)
            })
            .await
            .unwrap();
        assert!(previous - count <= 128, "receipt deletes must yield");
        previous = count;
        if step.complete {
            break;
        }
    }
    assert_eq!(previous, 3);
    f.db.call(|db| {
        for id in ["live-target", "live-sender", "reply-live-sender", "pending"] {
            let payload: Option<String> =
                db.query_row("SELECT payload_json FROM node WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            assert!(payload.is_some(), "receipt root {id}");
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM node WHERE id LIKE 'receipt-%' AND payload_json IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_gc_cleanup_is_resumable_for_many_marks() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<300) INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) SELECT 'retained-'||i,'turn','user','now','{}',2 FROM n", [])?;
        Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    let mut cleanups = 0;
    let mut completed = false;
    for _ in 0..100 {
        let step = f.gc.history_step(&job).await.unwrap();
        if step.stage == "cleanup" {
            cleanups += 1;
        }
        if step.complete {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert!(cleanups >= 3, "cleanup must advance in bounded units");
    f.db.call(|db| {
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM node WHERE id LIKE 'retained-%' AND payload_json IS NOT NULL"
            )?,
            300
        );
        assert_eq!(
            scalar(
                db,
                "SELECT COUNT(*) FROM gc_job WHERE id='job' AND state='done'"
            )?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_gc_budget_measurement_with_concurrent_writes() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) SELECT 'budget-root-'||i,'turn','user','now','{}',2 FROM n", [])?;
        Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    // A deterministic expensive per-row operation makes the initial cleanup
    // batch exceed the worker's real SQLite budget under foreground demand.
    f.db.call(|db| {
        db.execute_batch("CREATE TEMP TRIGGER gc_budget_fixture AFTER DELETE ON shellboy_gc_mark BEGIN SELECT SUM(payload_bytes) FROM node; END;")?;
        Ok(())
    }).await.unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicUsize::new(0));
    let writer = {
        let db = f.db.clone();
        let stop = stop.clone();
        let writes = writes.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                db.call(|db| { db.execute("UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'", [])?; Ok(()) }).await.unwrap();
                writes.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        })
    };
    let started = std::time::Instant::now();
    let (mut steps, mut deferred, mut scanned, mut complete) = (0, 0, 0, false);
    let deadline = crate::test_watchdog::Deadline::new("history GC under sustained writes");
    while !deadline.expired() && steps < 20000 {
        let step = f.gc.history_step(&job).await.unwrap();
        steps += 1;
        deferred += usize::from(step.deferred);
        scanned += step.scanned;
        if step.complete {
            complete = true;
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();
    println!(
        "BUDGET_MEASURE complete={complete} steps={steps} deferred={deferred} scanned={scanned} writes={} elapsed_ms={}",
        writes.load(Ordering::Relaxed),
        started.elapsed().as_millis()
    );
    f.db.shutdown().await.unwrap();
    assert!(
        complete,
        "history GC did not finish under concurrent writes"
    );
    assert!(deferred > 0, "fixture did not trigger the VM-step budget");
    assert!(
        writes.load(Ordering::Relaxed) > 0,
        "foreground writer made no progress"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_gc_keeps_single_over_budget_reachability_candidate() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        let tx = db.transaction()?;
        tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('oversized-source','user','now','{}',2)", [])?;
        for i in 0..5000 {
            let id = format!("consumer-{i}");
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES(?1,'summary','now','{}',2)", [&id])?;
            tx.execute("INSERT INTO replacement VALUES(?1,?1,'input','1','1','cached','now')", [&id])?;
            tx.execute("INSERT INTO replacement_source VALUES(?1,0,'oversized-source',0,NULL)", [&id])?;
        }
        tx.commit()?; Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicUsize::new(0));
    let writer = {
        let db = f.db.clone();
        let stop = stop.clone();
        let writes = writes.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                db.call(|db| { db.execute("UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'", [])?; Ok(()) }).await.unwrap();
                writes.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        })
    };
    let started = std::time::Instant::now();
    let (mut deferrals, mut steps, mut kept, mut complete) = (0, 0, 0, false);
    let deadline = crate::test_watchdog::Deadline::new("history GC under sustained writes");
    while !deadline.expired() && steps < 20000 {
        let step = f.gc.history_step(&job).await.unwrap();
        steps += 1;
        deferrals += usize::from(step.deferred);
        kept = f.db.call(|db| Ok(db.query_row("SELECT COALESCE(json_extract(cursor_json,'$.kept_count'),json_extract(error_json,'$.kept_over_budget'),0) FROM gc_job WHERE id='job'", [], |r| r.get::<_, i64>(0))?)).await.unwrap();
        if step.complete {
            complete = true;
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();
    println!(
        "OVERSIZED_CANDIDATE complete={complete} kept={kept} deferrals={deferrals} steps={steps} writes={} elapsed_ms={}",
        writes.load(Ordering::Relaxed),
        started.elapsed().as_millis()
    );
    f.db.call(|db| {
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM node WHERE id='oversized-source' AND payload_json IS NOT NULL")?, 1);
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM gc_job WHERE id='job' AND COALESCE(json_extract(cursor_json,'$.kept_ids[0]'),json_extract(error_json,'$.kept_ids[0]'))='oversized-source'")?, 1);
        Ok(())
    }).await.unwrap();
    f.db.shutdown().await.unwrap();
    assert!(complete && kept > 0 && deferrals > 0);
    assert!(
        writes.load(Ordering::Relaxed) > 0,
        "foreground writer made no progress"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_gc_keeps_node_when_atomic_tombstone_exceeds_budget() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('expensive-tombstone','user','now','{}',2)", [])?;
        db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<500) INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) SELECT 'fixture-'||i,'turn','user','now','{}',2 FROM n", [])?;
        db.execute_batch("CREATE TEMP TRIGGER costly_tombstone BEFORE UPDATE OF payload_json ON node WHEN OLD.id='expensive-tombstone' BEGIN SELECT count(*) FROM node a CROSS JOIN node b; END;")?;
        Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = f.db.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                db.call(|db| { db.execute("UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'", [])?; Ok(()) }).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
    };
    let deadline = crate::test_watchdog::Deadline::new("history GC single-item retention");
    let (mut deferred, mut complete) = (0, false);
    while !deadline.expired() {
        let step = f.gc.history_step(&job).await.unwrap();
        deferred += usize::from(step.deferred);
        if step.complete {
            complete = true;
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();
    f.db.call(|db| {
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM node WHERE id='expensive-tombstone' AND payload_json IS NOT NULL AND deleted_reason IS NULL")?, 1);
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM gc_job WHERE id='job' AND json_extract(error_json,'$.kept_ids[0]')='expensive-tombstone'")?, 1);
        Ok(())
    }).await.unwrap();
    f.db.shutdown().await.unwrap();
    assert!(
        complete && deferred > 0,
        "complete={complete}, deferred={deferred}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_gc_skips_a_single_over_budget_mark_cleanup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<500) INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) SELECT 'mark-'||i,'turn','user','now','{}',2 FROM n", [])?;
        Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    f.db.call(|db| {
        db.execute_batch("CREATE TEMP TRIGGER costly_mark BEFORE DELETE ON shellboy_gc_mark WHEN OLD.node_id='mark-1' BEGIN SELECT count(*) FROM node a CROSS JOIN node b; END;")?;
        Ok(())
    }).await.unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = f.db.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                db.call(|db| { db.execute("UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'", [])?; Ok(()) }).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
    };
    let deadline = crate::test_watchdog::Deadline::new("history GC single-item retention");
    let (mut deferred, mut complete) = (0, false);
    while !deadline.expired() {
        let step = f.gc.history_step(&job).await.unwrap();
        deferred += usize::from(step.deferred);
        if step.complete {
            complete = true;
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();
    f.db.call(|db| {
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM gc_job WHERE id='job' AND json_extract(error_json,'$.kept_marks')=1")?, 1);
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM node WHERE id='mark-1' AND payload_json IS NOT NULL")?, 1);
        Ok(())
    }).await.unwrap();
    f.db.shutdown().await.unwrap();
    assert!(
        complete && deferred > 0,
        "complete={complete}, deferred={deferred}"
    );
}

#[tokio::test]
async fn history_gc_discovers_pending_job_after_many_done_jobs_in_batches() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO gc_job(id,kind,root_revision,state) SELECT 'done-'||i,'history',0,'done' FROM n", [])?;
        Ok(())
    }).await.unwrap();
    f.job().await;
    let mut pending_scans = 0;
    let job = loop {
        match f.gc.start_history().await.unwrap() {
            Some(job) => break job,
            None => pending_scans += 1,
        }
        assert!(pending_scans < 20);
    };
    assert_eq!(job, "job");
    assert!(pending_scans >= 7);
    assert_eq!(f.gc.start_history().await.unwrap().as_deref(), Some("job"));
    f.db.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_gc_keeps_candidate_if_cursor_update_exceeds_budget() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('cursor-expensive','user','now','{}',2)", [])?;
        db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<500) INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) SELECT 'cursor-fixture-'||i,'turn','user','now','{}',2 FROM n", [])?;
        db.execute_batch("CREATE TEMP TRIGGER costly_gc_cursor AFTER UPDATE OF cursor_json ON gc_job WHEN EXISTS(SELECT 1 FROM node WHERE id='cursor-expensive' AND payload_json IS NULL) BEGIN SELECT count(*) FROM node a CROSS JOIN node b; END;")?;
        Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = f.db.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                db.call(|db| { db.execute("UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'", [])?; Ok(()) }).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
    };
    let deadline = crate::test_watchdog::Deadline::new("history GC cursor interruption");
    let (mut deferred, mut complete) = (0, false);
    while !deadline.expired() {
        let step = f.gc.history_step(&job).await.unwrap();
        deferred += usize::from(step.deferred);
        if step.complete {
            complete = true;
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();
    f.db.call(|db| {
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM node WHERE id='cursor-expensive' AND payload_json IS NOT NULL AND deleted_reason IS NULL")?, 1);
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM gc_job WHERE id='job' AND json_extract(error_json,'$.kept_ids[0]')='cursor-expensive'")?, 1);
        Ok(())
    }).await.unwrap();
    f.db.shutdown().await.unwrap();
    assert!(
        complete && deferred > 0,
        "complete={complete}, deferred={deferred}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_gc_full_retention_fallback_counts_nodes_only_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) VALUES('root','turn','user','now','{}',2)", [])?;
        db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<500) INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) SELECT 'retained-'||i,'user','now','{}',2 FROM n", [])?;
        Ok(())
    }).await.unwrap();
    f.job().await;
    let job = f.gc.start_history().await.unwrap().unwrap();
    let setup_deadline = crate::test_watchdog::Deadline::new("history GC provenance setup");
    while f.gc.history_step(&job).await.unwrap().stage != "provenance" {
        setup_deadline.check();
    }
    f.db.call(|db| {
        db.execute("UPDATE gc_job SET cursor_json=json_set(cursor_json,'$.kept_count',1,'$.kept_ids',json_array('root')) WHERE id='job'", [])?;
        db.execute_batch("CREATE TEMP TRIGGER costly_expansion BEFORE UPDATE OF expanded ON shellboy_gc_mark WHEN OLD.node_id='root' BEGIN SELECT count(*) FROM node a CROSS JOIN node b; END;")?;
        Ok(())
    }).await.unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = f.db.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                db.call(|db| { db.execute("UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'", [])?; Ok(()) }).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
    };
    let deadline = crate::test_watchdog::Deadline::new("history GC full retention fallback");
    let (mut deferred, mut complete) = (0, false);
    while !deadline.expired() {
        let step = f.gc.history_step(&job).await.unwrap();
        deferred += usize::from(step.deferred);
        if step.complete {
            complete = true;
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();
    f.db.call(|db| {
        assert_eq!(scalar(db, "SELECT COUNT(*) FROM node WHERE payload_json IS NOT NULL")?, 501);
        assert_eq!(scalar(db, "SELECT CAST(json_extract(error_json,'$.kept_over_budget') AS INTEGER) FROM gc_job WHERE id='job'")?, 501);
        assert_eq!(scalar(db, "SELECT COUNT(*)=COUNT(DISTINCT value) AND COUNT(*)=8 FROM gc_job,json_each(error_json,'$.kept_ids') WHERE gc_job.id='job'")?, 1);
        Ok(())
    }).await.unwrap();
    f.db.shutdown().await.unwrap();
    assert!(
        complete && deferred > 0,
        "complete={complete}, deferred={deferred}"
    );
}
