//! Explicit inactive-space GC uses real SQLite vector tables and temporary data.
use rusqlite::params;
use shellboy::{
    attachments::AttachmentService, config::Config, gc::GcService, protocol::StorageCategory,
    storage::DbWorker,
};
use std::sync::Arc;

struct Fixture {
    _temp: tempfile::TempDir,
    db: DbWorker,
    gc: GcService,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("gc.db")).await.unwrap();
        db.call(|db| {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('source','message','now','{}',2)", [])?;
            Ok(())
        }).await.unwrap();
        let config = Arc::new(Config::default());
        let attachments = Arc::new(AttachmentService::new(
            db.clone(),
            &config,
            temp.path().into(),
            "gc-fixture".into(),
        ));
        let gc = GcService::new(db.clone(), config, temp.path().into(), attachments);
        Self {
            _temp: temp,
            db,
            gc,
        }
    }
    async fn space(&self, number: usize, state: &str, running: bool) {
        let state = state.to_owned();
        self.db.call(move |db| {
            let id = format!("space-{number:03}");
            let table = format!("shellboy_vec_{number:032x}");
            db.execute_batch(&format!("CREATE VIRTUAL TABLE {table} USING vec0(embedding float[2]); INSERT INTO {table}(rowid,embedding)VALUES(1,'[1,2]');"))?;
            db.execute("INSERT INTO index_space VALUES(?1,'connection','model',2,'splitter',?2,?3)", params![id,state,table])?;
            db.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state)VALUES(?1,?1,'source',0,'message','hash',0,1,?2)", params![id,if running {"running"} else {"done"}])?;
            db.execute("INSERT INTO embedding_chunk VALUES(?1,?1,?1,'source',0,0,1,'hash',1)", [&id])?;
            for (key,value) in [
                (format!("index.input.{id}"), "input".to_owned()),
                (format!("index.preview.{id}"), "preview".into()),
                (format!("index.query_profile.{id}"), "query".into()),
                (format!("index.space_dimensions.{id}"), "2".into()),
                (format!("index.group.fixture.old-{number}"), id.clone()),
            ] { db.execute("INSERT INTO app_meta VALUES(?1,?2)", params![key,value])?; }
            Ok(())
        }).await.unwrap();
    }
    async fn collect(&self) -> shellboy::gc::CollectionReport {
        self.gc
            .collect(StorageCategory::Index, chrono::Utc::now())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn explicit_gc_collects_multiple_bounded_batches_and_preserves_protected_spaces() {
    let f = Fixture::new().await;
    for n in 0..35 {
        f.space(n, "inactive", false).await;
    }
    f.space(40, "active", false).await;
    f.space(41, "candidate", false).await;
    f.space(42, "candidate_paused", false).await;
    f.space(43, "inactive", true).await;
    f.space(44, "inactive", false).await;
    f.space(45, "inactive", false).await;
    f.db.call(|db| {
        db.execute_batch("INSERT INTO app_meta VALUES('index.active_space','space-044'),('index.rebuild.fixture','current'),('index.group.fixture.current','space-045'),('index.bootstrap.unrelated','{}');")?;
        Ok(())
    }).await.unwrap();
    // Observation and unrelated categories never collect inactive spaces.
    f.gc.details().await.unwrap();
    assert_eq!(
        f.gc.collect(StorageCategory::Raw, chrono::Utc::now())
            .await
            .unwrap()
            .inactive_index_spaces_collected,
        0
    );
    assert_eq!(f.collect().await.inactive_index_spaces_collected, 35);
    f.db.call(|db| {
        let count: i64 = db.query_row("SELECT count(*) FROM index_space", [], |r| r.get(0))?;
        assert_eq!(count, 6);
        for table in ["embedding_job", "embedding_chunk"] {
            let count: i64 = db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            assert_eq!(count, 6);
        }
        let stale: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key IN ('index.input.space-000','index.preview.space-000','index.query_profile.space-000','index.space_dimensions.space-000','index.group.fixture.old-0')", [], |r| r.get(0))?;
        assert_eq!(stale, 0);
        let table: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE name='shellboy_vec_00000000000000000000000000000000'", [], |r| r.get(0))?;
        assert_eq!(table, 0);
        let kept: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key IN ('index.bootstrap.unrelated','index.group.fixture.current','index.active_space')", [], |r| r.get(0))?;
        assert_eq!(kept, 3);
        Ok(())
    }).await.unwrap();
    assert_eq!(f.collect().await.inactive_index_spaces_collected, 0);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_space_failure_rolls_back_vector_ddl_chunks_jobs_and_metadata() {
    let f = Fixture::new().await;
    f.space(0, "inactive", false).await;
    f.space(1, "inactive", false).await;
    f.db.call(|db| {
        db.execute_batch("CREATE TRIGGER fail_space_gc BEFORE DELETE ON index_space WHEN old.id='space-001' BEGIN SELECT RAISE(ABORT,'synthetic space collection failure'); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(
        f.gc.collect(StorageCategory::Index, chrono::Utc::now())
            .await
            .is_err()
    );
    f.db.call(|db| {
        for table in ["index_space", "embedding_chunk", "embedding_job"] {
            let count: i64 =
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            assert_eq!(count, 2);
        }
        for n in 0..2 {
            let table = format!("shellboy_vec_{n:032x}");
            let count: i64 =
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            assert_eq!(count, 1);
        }
        let count: i64 = db.query_row(
            "SELECT count(*) FROM app_meta WHERE key='index.input.space-000'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1);
        db.execute_batch("DROP TRIGGER fail_space_gc;")?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(f.collect().await.inactive_index_spaces_collected, 2);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_vector_table_identifier_is_rejected_without_deleting_space() {
    let f = Fixture::new().await;
    f.space(0, "inactive", false).await;
    f.db.call(|db| {
        db.execute("UPDATE index_space SET vec_table_name='node'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        f.gc.collect(StorageCategory::Index, chrono::Utc::now())
            .await
            .is_err()
    );
    f.db.call(|db| {
        let count: i64 = db.query_row("SELECT count(*) FROM node WHERE id='source'", [], |r| {
            r.get(0)
        })?;
        assert_eq!(count, 1);
        let count: i64 = db.query_row("SELECT count(*) FROM index_space", [], |r| r.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}
