use std::sync::mpsc as sync_mpsc;
use std::time::Duration;

use rusqlite::{Connection, params};
use shellboy::storage::{DbWorker, SCHEMA_VERSION, StorageError};
use tempfile::TempDir;

async fn database() -> (TempDir, DbWorker) {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("shellboy.sqlite3"))
        .await
        .unwrap();
    (directory, database)
}

fn seed_thread(connection: &Connection, id: &str) -> Result<(), StorageError> {
    connection.execute(
        "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,'/tmp','/tmp/AGENTS.md','test','fixture','2026-10-24T00:00:00Z')",
        [id],
    )?;
    Ok(())
}

#[tokio::test]
async fn production_migration_pragmas_and_reopen() {
    let (directory, database) = database().await;
    database.call(|connection| {
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, SCHEMA_VERSION);
        let tables: i64 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |row| row.get(0))?;
        assert_eq!(tables, 36);
        let journal: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        assert_eq!(journal, "wal");
        let synchronous: i64 = connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
        assert_eq!(synchronous, 2);
        let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        assert_eq!(foreign_keys, 1);
        let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        assert_eq!(integrity, "ok");
        assert!(!connection.prepare("PRAGMA foreign_key_check")?.exists([])?);
        seed_thread(connection, "persisted")?;
        Ok(())
    }).await.unwrap();
    database.shutdown().await.unwrap();
    let database = DbWorker::open(directory.path().join("shellboy.sqlite3"))
        .await
        .unwrap();
    database
        .call(|connection| {
            let thread: String =
                connection.query_row("SELECT id FROM thread", [], |row| row.get(0))?;
            assert_eq!(thread, "persisted");
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipt_commit_is_durable_unique_and_assignment_immutable() {
    let (directory, database) = database().await;
    database.call(|connection| {
        let tx = connection.transaction()?;
        seed_thread(&tx, "thread")?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('run','thread','live','now')", [])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('turn','thread','run','preparing','now')", [])?;
        tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json) VALUES('message','turn','user','now','{\"text\":\"fixture\"}')", [])?;
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at) VALUES('receipt','op','{}','thread','message','run','turn','accepted','now')", [])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let duplicate = database.call(|connection| {
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO node(id,kind,created_at) VALUES('rolled_back','user','now')", [])?;
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at) VALUES('second','op','{}','thread','rolled_back','accepted','now')", [])?;
        tx.commit()?;
        Ok(())
    }).await;
    assert!(matches!(duplicate, Err(StorageError::Sqlite(_))));
    database
        .call(|connection| {
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM node WHERE id='rolled_back'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            assert!(
                connection
                    .execute("UPDATE receipt SET turn_id=NULL WHERE id='receipt'", [])
                    .is_err()
            );
            let changes = connection.execute(
                "UPDATE turn SET state='running' WHERE id='turn' AND generation=7",
                [],
            )?;
            assert_eq!(changes, 0, "late prepare must fail generation CAS");
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
    let database = DbWorker::open(directory.path().join("shellboy.sqlite3"))
        .await
        .unwrap();
    database
        .call(|connection| {
            let result: (String, String, String) = connection.query_row(
                "SELECT message_node_id,run_id,turn_id FROM receipt WHERE client_operation_id='op'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(result, ("message".into(), "run".into(), "turn".into()));
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn dag_and_run_ownership_cycles_and_duplicate_active_runs_are_rejected() {
    let (_directory, database) = database().await;
    database.call(|connection| {
        for id in ["a","b","c"] {
            seed_thread(connection,id)?;
            connection.execute("INSERT INTO node(id,kind,created_at) VALUES(?1,'user','now')", [id])?;
            connection.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?1,'live','now')", [id])?;
        }
        connection.execute("INSERT INTO edge VALUES('a','b'),('b','c')", [])?;
        assert!(connection.execute("INSERT INTO edge VALUES('c','a')", []).is_err());
        assert!(connection.execute("INSERT INTO edge VALUES('a','a')", []).is_err());
        // Updating an existing edge cannot bypass the insertion trigger.
        assert!(connection.execute("UPDATE edge SET src='c' WHERE src='a'", []).is_err());
        connection.execute("UPDATE run SET owner_run_id='a' WHERE id='b'", [])?;
        connection.execute("UPDATE run SET owner_run_id='b' WHERE id='c'", [])?;
        assert!(connection.execute("UPDATE run SET owner_run_id='c' WHERE id='a'", []).is_err());
        assert!(connection.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('duplicate','a','stopping','now')", []).is_err());
        connection.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('a',0,'a'),('a',1,'b'),('a',2,'c')", [])?;
        assert!(connection.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('a',3,'b')", []).is_err());
        Ok(())
    }).await.unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsupported_schemas_are_rejected_without_changing_database_bytes() {
    for schema in [
        "CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES('preserve'); PRAGMA user_version=99;",
        "CREATE TABLE unversioned(value TEXT); INSERT INTO unversioned VALUES('preserve');",
        "CREATE TABLE schema_migration(version INTEGER,name TEXT); INSERT INTO schema_migration VALUES(1,'wrong'); PRAGMA user_version=1;",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(schema).unwrap();
        connection.close().unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            DbWorker::open(&path).await,
            Err(StorageError::Schema(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("sqlite3-wal").exists());
    }
}

#[tokio::test]
async fn vec_source_insert_and_delete_rollback_together() {
    let (_directory, database) = database().await;
    database.call(|connection| {
        let version: String = connection.query_row("SELECT vec_version()", [], |row| row.get(0))?;
        assert_eq!(version,"v0.1.9");
        connection.execute_batch("CREATE VIRTUAL TABLE test_vec USING vec0(embedding float[3]);")?;
        {
            let tx = connection.transaction()?;
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('source','assistant','now','{}')", [])?;
            tx.execute("INSERT INTO test_vec(rowid,embedding) VALUES(1,'[1,0,0]')", [])?;
            tx.rollback()?;
        }
        assert_eq!(connection.query_row("SELECT count(*) FROM node", [], |row| row.get::<_,i64>(0))?,0);
        assert_eq!(connection.query_row("SELECT count(*) FROM test_vec", [], |row| row.get::<_,i64>(0))?,0);
        connection.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('source','assistant','now','{}')", [])?;
        connection.execute("INSERT INTO test_vec(rowid,embedding) VALUES(1,'[1,0,0]')", [])?;
        {
            let tx = connection.transaction()?;
            tx.execute("UPDATE node SET payload_json=NULL,deleted_reason='expired',payload_revision=payload_revision+1 WHERE id='source'", [])?;
            tx.execute("DELETE FROM test_vec WHERE rowid=1", [])?;
            tx.rollback()?;
        }
        let payload: Option<String> = connection.query_row("SELECT payload_json FROM node WHERE id='source'", [], |row| row.get(0))?;
        assert_eq!(payload.as_deref(),Some("{}"));
        let nearest: i64 = connection.query_row("SELECT rowid FROM test_vec WHERE embedding MATCH '[1,0,0]' AND k=1 ORDER BY distance", [], |row| row.get(0))?;
        assert_eq!(nearest,1);
        Ok(())
    }).await.unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_admission_and_shared_shutdown_drain_accepted_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open_with_capacity(directory.path().join("db"), 1)
        .await
        .unwrap();
    let (started_send, started_receive) = tokio::sync::oneshot::channel();
    let (release_send, release_receive) = sync_mpsc::channel();
    let first = database
        .try_call(move |connection| {
            started_send.send(()).unwrap();
            release_receive
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            connection.execute(
                "INSERT INTO app_meta(key,value) VALUES('first','committed')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    started_receive.await.unwrap();
    let second = database
        .try_call(|connection| {
            connection.execute(
                "INSERT INTO app_meta(key,value) VALUES('second','committed')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        database.try_call(|_| Ok(())),
        Err(StorageError::QueueFull)
    ));
    let clone = database.clone();
    let shutdown = tokio::spawn(async move { clone.shutdown().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(database.try_call(|_| Ok(())), Err(StorageError::Closed)) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the actual writer"
    );
    release_send.send(()).unwrap();
    first.receive().await.unwrap();
    second.receive().await.unwrap();
    shutdown.await.unwrap().unwrap();
    database.shutdown().await.unwrap();
    let reopened = DbWorker::open(directory.path().join("db")).await.unwrap();
    reopened
        .call(|connection| {
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM app_meta WHERE key IN ('first','second')",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropped_observer_does_not_cancel_commit_and_uncommitted_jobs_are_rolled_back() {
    let (_directory, database) = database().await;
    let reply = database
        .try_call(|connection| {
            connection.execute(
                "INSERT INTO app_meta(key,value) VALUES('unobserved','durable')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    drop(reply);
    let result = database
        .call(|connection| {
            connection.execute_batch(
                "BEGIN; INSERT INTO app_meta(key,value) VALUES('uncommitted','must rollback')",
            )?;
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(StorageError::UncommittedTransaction)));
    let result = database
        .call::<(), _>(|connection| {
            connection.execute_batch(
                "BEGIN; INSERT INTO app_meta(key,value) VALUES('panic','must rollback')",
            )?;
            panic!("test fault injection");
        })
        .await;
    assert!(matches!(result, Err(StorageError::JobPanicked)));
    database
        .call(|connection| {
            let committed: String = connection.query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                params!["unobserved"],
                |row| row.get(0),
            )?;
            assert_eq!(committed, "durable");
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM app_meta WHERE key IN ('uncommitted','panic')",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_shutdown_observer_still_closes_and_joins_writer() {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("db")).await.unwrap();
    let (started_send, started_receive) = tokio::sync::oneshot::channel();
    let (release_send, release_receive) = sync_mpsc::channel();
    let reply = database
        .try_call(move |connection| {
            started_send.send(()).unwrap();
            release_receive
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            connection.execute(
                "INSERT INTO app_meta(key,value) VALUES('shutdown_cancelled','committed')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    started_receive.await.unwrap();
    let clone = database.clone();
    let observer = tokio::spawn(async move { clone.shutdown().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(database.try_call(|_| Ok(())), Err(StorageError::Closed)) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    release_send.send(()).unwrap();
    reply.receive().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), database.shutdown())
        .await
        .unwrap()
        .unwrap();
    let reopened = DbWorker::open(directory.path().join("db")).await.unwrap();
    reopened
        .call(|connection| {
            let value: String = connection.query_row(
                "SELECT value FROM app_meta WHERE key='shutdown_cancelled'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(value, "committed");
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_guard_outlives_all_callers_until_actual_writer_close() {
    use std::{fs::OpenOptions, sync::Arc};
    let directory = tempfile::tempdir().unwrap();
    let lock_path = directory.path().join("owner.lock");
    let owner = Arc::new(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap(),
    );
    fs2::FileExt::try_lock_exclusive(owner.as_ref()).unwrap();
    let database = DbWorker::open_owned(directory.path().join("db"), owner.clone())
        .await
        .unwrap();
    let (started_send, started_receive) = tokio::sync::oneshot::channel();
    let (release_send, release_receive) = sync_mpsc::channel();
    let reply = database
        .try_call(move |connection| {
            started_send.send(()).unwrap();
            release_receive
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            connection.execute(
                "INSERT INTO app_meta(key,value) VALUES('owner_lifetime','committed')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    started_receive.await.unwrap();
    drop(database);
    drop(owner);
    let competitor = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(
        fs2::FileExt::try_lock_exclusive(&competitor).is_err(),
        "dropped callers must not release a live writer's owner lock"
    );
    release_send.send(()).unwrap();
    reply.receive().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while fs2::FileExt::try_lock_exclusive(&competitor).is_err() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let reopened = DbWorker::open(directory.path().join("db")).await.unwrap();
    reopened
        .call(|connection| {
            let value: String = connection.query_row(
                "SELECT value FROM app_meta WHERE key='owner_lifetime'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(value, "committed");
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn version_one_upgrade_preserves_data_and_adds_gc_reverse_indexes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("v1.db");
    let connection = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    connection.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('saved','/tmp','/tmp/AGENTS.md','fixture','fixture','now')",[]).unwrap();
    connection.execute_batch("COMMIT").unwrap();
    connection.close().unwrap();
    let database = DbWorker::open(&path).await.unwrap();
    database.call(|db|{
        assert_eq!(db.query_row("SELECT id FROM thread",[],|r|r.get::<_,String>(0))?,"saved");
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='index' AND name IN ('receipt_message_node','replacement_output_node','tool_result_node','output_item_node')",[],|r|r.get::<_,i64>(0))?,4);
        assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?,SCHEMA_VERSION);Ok(())
    }).await.unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn version_two_upgrade_preserves_context_and_indexes_only_unconsumed_observations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("v2.db");
    let connection = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../migrations/0002_gc_indexes.sql"))
        .unwrap();
    connection.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('saved','/tmp','/tmp/AGENTS.md','fixture','fixture','now'); INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at) VALUES('pending','context_changed','{}','saved','{}','now');").unwrap();
    connection.execute_batch("COMMIT").unwrap();
    connection.close().unwrap();
    let database = DbWorker::open(&path).await.unwrap();
    database.call(|db|{
        assert_eq!(db.query_row("SELECT id FROM event",[],|r|r.get::<_,String>(0))?,"pending");
        let plan:String=db.query_row("EXPLAIN QUERY PLAN SELECT id FROM event WHERE target_thread_id='saved' AND kind='context_changed' AND target_run_id IS NULL ORDER BY created_at,id",[],|r|r.get(3))?;
        assert!(plan.contains("context_pending_by_thread"),"{plan}");
        assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?,SCHEMA_VERSION);
        Ok(())
    }).await.unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn version_three_attachment_backfill_preserves_known_source_and_quarantines_unknown() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("v3.db");
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/0002_gc_indexes.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/0003_context_indexes.sql"))
        .unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/tmp','/tmp/AGENTS.md','p','m','now');INSERT INTO node(id,kind,created_at,payload_json)VALUES('human','user','now','{\"role\":\"user\",\"content\":\"original\"}');INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('t',0,'human');INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('known','human','/tmp/a','file','external','now'),('unknown',NULL,'/tmp/b','file','external','now');INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('t','known','active','now',0),('t','unknown','active','now',1);").unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('fork','/tmp','/tmp/AGENTS.md','p','m','now');INSERT INTO node(id,kind,created_at,payload_json)VALUES('old-assistant','assistant','now','{\"type\":\"function_call\",\"name\":\"shell\",\"call_id\":\"call\",\"arguments\":\"{}\"}');INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('assistant-origin','old-assistant','/tmp/c','file','external','now');INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('fork','unknown','active','now',0),('t','assistant-origin','active','now',2),('fork','assistant-origin','active','now',1);").unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    let db = DbWorker::open(&path).await.unwrap();
    db.call(|db|{
        let source:String=db.query_row("SELECT source_node_id FROM attachment_ref WHERE id='known'",[],|r|r.get(0))?;
        assert_eq!(source,"human");
        let payload:String=db.query_row("SELECT payload_json FROM node WHERE id='human'",[],|r|r.get(0))?;
        let payload:serde_json::Value=serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["content"][0]["text"],"original");
        assert_eq!(payload["content"][1]["attachment_id"],"known");
        let (source,status):(String,String)=db.query_row("SELECT a.source_node_id,d.status FROM attachment_ref a JOIN attachment_delivery d ON d.node_id=a.source_node_id WHERE a.id='unknown'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_ne!(source,"human");assert_eq!(status,"accepted");
        assert_eq!(db.query_row("SELECT count(*) FROM attachment_delivery WHERE node_id=?1 AND status='accepted'",[&source],|r|r.get::<_,i64>(0))?,2);
        let previous:Option<String>=db.query_row("SELECT json_extract(n.payload_json,'$.attachment_operation.previous_source_node_id') FROM attachment_ref a JOIN node n ON n.id=a.source_node_id WHERE a.id='assistant-origin'",[],|r|r.get(0))?;
        assert_eq!(previous.as_deref(),Some("old-assistant"));
        let content:String=db.query_row("SELECT json_extract(n.payload_json,'$.content[0].text') FROM attachment_ref a JOIN node n ON n.id=a.source_node_id WHERE a.id='unknown'",[],|r|r.get(0))?;
        assert!(content.contains("not a new human message"));
        assert_eq!(db.query_row("SELECT count(*) FROM attachment_delivery WHERE status='accepted'",[],|r|r.get::<_,i64>(0))?,4);
        assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE node_id<>'human'",[],|r|r.get::<_,i64>(0))?,0);
        assert!(db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('invalid',NULL,'/tmp/c','file','external','now')",[]).is_err());
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
    let db = DbWorker::open(&path).await.unwrap();
    db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM node WHERE kind='attachment'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            2
        );
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn version_three_compacted_attachment_tombstones_do_not_create_pending_replay_sources() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("v3-compacted.db");
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/0002_gc_indexes.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/0003_context_indexes.sql"))
        .unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('compacted','/tmp','/tmp/AGENTS.md','p','m','now'),('shared','/tmp','/tmp/AGENTS.md','p','m','now');INSERT INTO node(id,kind,created_at,payload_json,deleted_reason)VALUES('retired','user','now',NULL,'compacted');INSERT INTO node(id,kind,created_at,payload_json)VALUES('summary','summary','now','{\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"Retained summary without attachment binaries\"}]}');INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('compacted',0,'summary');INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('retired-ref','retired','/unavailable/a','file','external','now'),('unknown-ref',NULL,'/unavailable/b','file','external','now');INSERT INTO attachment_selection(thread_id,attachment_id,state,reason,selected_at,selected_order)VALUES('compacted','retired-ref','excluded','context_replacement','now',0),('compacted','unknown-ref','excluded','context_replacement','now',1),('shared','unknown-ref','active',NULL,'now',0);").unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    let db = DbWorker::open(&path).await.unwrap();
    db.call(|db|{
        assert_eq!(db.query_row("SELECT source_node_id FROM attachment_ref WHERE id='retired-ref'",[],|r|r.get::<_,String>(0))?,"retired");
        assert_eq!(db.query_row("SELECT count(*) FROM attachment_delivery WHERE node_id='retired'",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT count(*) FROM attachment_delivery WHERE thread_id='compacted' AND status IN ('accepted','held')",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT status FROM attachment_delivery WHERE thread_id='compacted'",[],|r|r.get::<_,String>(0))?,"cancelled");
        assert_eq!(db.query_row("SELECT status FROM attachment_delivery WHERE thread_id='shared'",[],|r|r.get::<_,String>(0))?,"accepted");
        let retained:Option<String>=db.query_row("SELECT payload_json FROM node WHERE id='retired'",[],|r|r.get(0))?;
        assert!(retained.is_none());
        assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE thread_id='compacted' AND node_id<>'summary'",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    let service = shellboy::attachments::AttachmentService::new(
        db.clone(),
        &shellboy::config::Config::default(),
        root.path().to_owned(),
        "migration-test".into(),
    );
    let prepared = service
        .prepare(
            "compacted",
            tokio_util::sync::CancellationToken::new(),
            false,
        )
        .await
        .unwrap();
    assert!(prepared.placements.is_empty());
    assert!(prepared.manifest.is_empty());
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn typed_domain_failure_preserves_details_and_rolls_back_without_poisoning_worker() {
    use shellboy::protocol::{ApiError, ErrorCode};
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("typed-error.db"))
        .await
        .unwrap();
    let expected = ApiError::new(
        ErrorCode::InputLimit,
        "Protected attachments exceed the limit",
    )
    .hint("Remove one attachment and retry the retained request")
    .details(serde_json::json!({"protected_bytes":42,"limit_bytes":20}));
    let transported = expected.clone();
    let result: Result<(), StorageError> = database
        .call(move |connection| {
            let tx = connection.transaction()?;
            tx.execute(
                "INSERT INTO app_meta(key,value) VALUES('must_rollback','value')",
                [],
            )?;
            Err(transported.into())
        })
        .await;
    assert_eq!(result.unwrap_err().into_api_error(), expected);
    let absent: bool = database
        .call(|connection| {
            Ok(!connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key='must_rollback')",
                [],
                |row| row.get::<_, bool>(0),
            )?)
        })
        .await
        .unwrap();
    assert!(absent);
    database
        .call(|connection| {
            connection.execute(
                "INSERT INTO app_meta(key,value) VALUES('still_usable','value')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_sql_failure_remains_fatal_but_admission_and_domain_errors_do_not() {
    use shellboy::protocol::ErrorCode;
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("sql-error.db"))
        .await
        .unwrap();
    let result = database
        .call(|connection| {
            connection.execute("INSERT INTO nonexistent_table(value) VALUES(1)", [])?;
            Ok(())
        })
        .await;
    assert_eq!(
        result.unwrap_err().into_api_error().code,
        ErrorCode::SaveFailed
    );
    for failure in [
        StorageError::QueueFull,
        StorageError::Closed,
        StorageError::InvalidInput("stale source".into()),
    ] {
        assert_eq!(failure.into_api_error().code, ErrorCode::InvalidState);
    }
    // NoRows has no global target identity: only the query owner can decide
    // whether the missing row was a Thread, node, shell, or a storage invariant.
    assert_eq!(
        StorageError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
            .into_api_error()
            .code,
        ErrorCode::SaveFailed
    );
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn version_four_removes_only_origin_credentials_preserving_results_raw_and_invalid_metadata()
{
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("v4.db");
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    for migration in [
        include_str!("../migrations/0001_initial.sql"),
        include_str!("../migrations/0002_gc_indexes.sql"),
        include_str!("../migrations/0003_context_indexes.sql"),
        include_str!("../migrations/0004_attachment_provenance.sql"),
    ] {
        db.execute_batch(migration).unwrap();
    }
    seed_thread(&db, "thread").unwrap();
    db.execute_batch(r#"
    INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','completed','now');
    INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','completed','now');
    INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('shell','run','turn','shell-op','exited','/tmp','{}','UTF-8','UTF-8','closed','now');
    INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('shell',0,'stdout',0,x'7072697661746500ff','now');
    INSERT INTO node(id,kind,created_at,payload_json)VALUES('message','user','now','{"text":"raw token marker stays","token":"user-content"}');
    INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES('receipt','send','{"instance_id":"old","execution_id":"shell","token":"private"}','thread','message','accepted','now','{"token":"user-result"}');
    INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json)VALUES('send','old','{"execution_id":"shell","token":"private"}','unchanged-hash','completed','now','{"result":{"token":"user-result"}}'),('invalid','old','malformed','hash','pending','now',NULL);
    INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at)VALUES('message','thread','held','attach','{"kind":"shell","execution_id":"shell","token":"private"}','now');
    INSERT INTO app_meta VALUES('wait.pending.valid','{"caller":{"execution_id":"shell","token":"private"},"conditions":[]}'),('wait.pending.invalid','malformed'),('unrelated','{"caller":{"token":"user-content"}}');
    "#).unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    for _ in 0..2 {
        let worker = DbWorker::open(&path).await.unwrap();
        worker.call(|db| {
            for sql in ["SELECT json_type(origin_json,'$.token') FROM operation_receipt WHERE operation_id='send'", "SELECT json_type(sender_json,'$.token') FROM receipt", "SELECT json_type(origin_json,'$.token') FROM attachment_delivery", "SELECT json_type(value,'$.caller.token') FROM app_meta WHERE key='wait.pending.valid'"] {
                assert_eq!(db.query_row(sql, [], |r| r.get::<_,Option<String>>(0))?, None);
            }
            assert_eq!(db.query_row("SELECT bytes FROM output_chunk WHERE execution_id='shell'", [], |r| r.get::<_,Vec<u8>>(0))?, b"private\0\xff");
            let cached:i64=db.query_row("SELECT logical_bytes FROM quota_counter WHERE category='operation_receipt'",[],|r|r.get(0))?;
            let scanned:i64=db.query_row("SELECT SUM(512+octet_length(operation_id)+octet_length(instance_id)+octet_length(origin_json)+octet_length(request_hash)+octet_length(created_at)+COALESCE(octet_length(result_ref_json),0)) FROM operation_receipt",[],|r|r.get(0))?;
            assert_eq!(cached,scanned,"Schema6 initializes derived usage from retained legacy rows exactly once");
            assert_eq!(db.query_row("SELECT request_hash FROM operation_receipt WHERE operation_id='send'", [], |r| r.get::<_,String>(0))?, "unchanged-hash");
            assert_eq!(db.query_row("SELECT json_extract(origin_json,'$.execution_id') FROM operation_receipt WHERE operation_id='send'", [], |r| r.get::<_,String>(0))?, "shell");
            assert_eq!(db.query_row("SELECT result_ref_json FROM operation_receipt WHERE operation_id='send'", [], |r| r.get::<_,String>(0))?, r#"{"result":{"token":"user-result"}}"#);
            assert_eq!(db.query_row("SELECT payload_json FROM node WHERE id='message'", [], |r| r.get::<_,String>(0))?, r#"{"text":"raw token marker stays","token":"user-content"}"#);
            for sql in ["SELECT origin_json FROM operation_receipt WHERE operation_id='invalid'", "SELECT value FROM app_meta WHERE key='wait.pending.invalid'"] {
                assert_eq!(db.query_row(sql, [], |r| r.get::<_,String>(0))?, "malformed");
            }
            assert_eq!(db.query_row("SELECT json_extract(value,'$.caller.token') FROM app_meta WHERE key='unrelated'", [], |r| r.get::<_,String>(0))?, "user-content");
            Ok(())
        }).await.unwrap();
        worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn version_six_byte_and_source_indexes_are_atomic_and_preserve_legacy_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy-six.sqlite");
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    for migration in [
        include_str!("../migrations/0001_initial.sql"),
        include_str!("../migrations/0002_gc_indexes.sql"),
        include_str!("../migrations/0003_context_indexes.sql"),
        include_str!("../migrations/0004_attachment_provenance.sql"),
        include_str!("../migrations/0005_origin_credentials.sql"),
        include_str!("../migrations/0006_receipt_budget.sql"),
    ] {
        db.execute_batch(migration).unwrap();
    }
    db.execute_batch(r#"INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('old','message','now','{"text":"日本語🌿"}',0);
INSERT INTO app_meta(key,value)VALUES('index.bootstrap.invalid','not JSON'),('index.auto.invalid','not JSON'),('foreign.invalid','not JSON'),('index.bootstrap.valid','{"witness":{"node_id":"old"}}');
CREATE TABLE index_auto_source(blocker INTEGER);"#).unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    // Fail after the other indexes were created; none may escape migration rollback.
    assert!(matches!(
        DbWorker::open(&path).await,
        Err(StorageError::Sqlite(_))
    ));
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type='index' AND (name LIKE 'gc_bytes_%' OR name='index_bootstrap_source')",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    db.execute("DROP TABLE index_auto_source", []).unwrap();
    db.close().unwrap();
    for _ in 0..2 {
        let worker = DbWorker::open(&path).await.unwrap();
        worker.call(|db| {
            assert_eq!(db.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0))?,SCHEMA_VERSION);
            assert_eq!(db.query_row("SELECT payload_json,payload_bytes FROM node WHERE id='old'",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?,(r#"{"text":"日本語🌿"}"#.into(),0));
            assert_eq!(db.query_row("SELECT COUNT(*) FROM app_meta WHERE value='not JSON'",[],|row|row.get::<_,i64>(0))?,3);
            assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type='index' AND (name LIKE 'gc_bytes_%' OR name IN ('index_bootstrap_source','index_auto_source'))",[],|row|row.get::<_,i64>(0))?,15);
            Ok(())
        }).await.unwrap();
        worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn intermediate_and_final_v6_and_v7_upgrade_receipt_accounting_without_replaying_intents() {
    // Frozen b8ae7db v6 SQL: the original development binary only added a column.
    const EARLY_SIX: &str = "ALTER TABLE operation_receipt ADD COLUMN reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes>=0); INSERT INTO schema_migration(version,name)VALUES(6,'0006_receipt_budget'); PRAGMA user_version=6;";
    for (early, seven) in [(true, false), (false, false), (true, true), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("receipt-compat.sqlite");
        let db = Connection::open(&path).unwrap();
        // Batch historical fixture setup; the production upgrade below still commits normally.
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        for migration in [
            include_str!("../migrations/0001_initial.sql"),
            include_str!("../migrations/0002_gc_indexes.sql"),
            include_str!("../migrations/0003_context_indexes.sql"),
            include_str!("../migrations/0004_attachment_provenance.sql"),
            include_str!("../migrations/0005_origin_credentials.sql"),
            if early {
                EARLY_SIX
            } else {
                include_str!("../migrations/0006_receipt_budget.sql")
            },
        ] {
            db.execute_batch(migration).unwrap();
        }
        db.execute_batch("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes)VALUES('pending','old','{\"origin\":\"human\"}','same-hash','pending','now',NULL,8192),('done','old','{}','done-hash','completed','now','{\"text\":\"日本語🌿\"}',0); INSERT INTO app_meta(key,value)VALUES('preserve','untouched');").unwrap();
        if seven {
            db.execute_batch(include_str!(
                "../migrations/0007_usage_and_source_indexes.sql"
            ))
            .unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
        db.close().unwrap();
        for reopen in 0..2 {
            let worker = DbWorker::open(&path).await.unwrap();
            worker.call(move|db| {
                assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?,SCHEMA_VERSION);
                assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('operation_receipt_budget_insert','operation_receipt_budget_delete','operation_receipt_budget_update','operation_receipt_pending_budget')",[],|r|r.get::<_,i64>(0))?,4);
                assert_eq!(db.query_row("SELECT state,request_hash,reserved_bytes,result_ref_json FROM operation_receipt WHERE operation_id='pending'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,Option<String>>(3)?)))?,("pending".into(),"same-hash".into(),8192,None));
                assert_eq!(db.query_row("SELECT value FROM app_meta WHERE key='preserve'",[],|r|r.get::<_,String>(0))?,"untouched");
                fn compare(db:&Connection)->Result<(),StorageError> {
                    for (category,predicate) in [("operation_receipt","true"),("operation_receipt.pending_actual","state='pending'")] {
                        let sql=format!("SELECT COALESCE(SUM(512+length(CAST(operation_id AS BLOB))+length(CAST(instance_id AS BLOB))+length(CAST(origin_json AS BLOB))+length(CAST(request_hash AS BLOB))+length(CAST(created_at AS BLOB))+COALESCE(length(CAST(result_ref_json AS BLOB)),0)),0) FROM operation_receipt WHERE {predicate}");
                        let expected:i64=db.query_row(&sql,[],|r|r.get(0))?;
                        let actual:i64=db.query_row("SELECT logical_bytes FROM quota_counter WHERE category=?1",[category],|r|r.get(0))?;
                        assert_eq!(actual,expected);
                    }Ok(())
                }
                compare(db)?;
                if reopen==0 {
                    let tx=db.transaction()?;
                    tx.execute("UPDATE operation_receipt SET result_ref_json='null' WHERE operation_id='done'",[])?;
                    compare(&tx)?;tx.rollback()?;compare(db)?;
                    db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json)VALUES('new','current','{}','new-hash','pending','now',NULL)",[])?;compare(db)?;
                    db.execute("UPDATE operation_receipt SET state='completed',result_ref_json='{}' WHERE operation_id='new'",[])?;compare(db)?;
                    db.execute("DELETE FROM operation_receipt WHERE operation_id='done'",[])?;compare(db)?;
                }
                Ok(())
            }).await.unwrap();
            worker.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn receipt_accounting_compat_upgrade_rolls_back_as_one_version() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("compat-rollback.sqlite");
    let worker = DbWorker::open(&path).await.unwrap();
    worker.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute_batch("DROP INDEX node_turn; DROP INDEX IF EXISTS receipt_turn_source; DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DELETE FROM schema_migration WHERE version>=8; DROP INDEX IF EXISTS shell_active_owner; DROP INDEX IF EXISTS run_thread_created; DROP INDEX IF EXISTS shell_terminal_time; PRAGMA user_version=7; UPDATE quota_counter SET logical_bytes=9999 WHERE category IN ('operation_receipt','operation_receipt.pending_actual'); CREATE TRIGGER compat_fail BEFORE INSERT ON quota_counter WHEN NEW.category='operation_receipt.pending_actual' BEGIN SELECT RAISE(ABORT,'injected v8 failure'); END;").unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    assert!(matches!(
        DbWorker::open(&path).await,
        Err(StorageError::Sqlite(_))
    ));
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM schema_migration WHERE version=8",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM quota_counter WHERE category IN ('operation_receipt','operation_receipt.pending_actual') AND logical_bytes=9999",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    db.execute("DROP TRIGGER compat_fail", []).unwrap();
    db.close().unwrap();
    let worker = DbWorker::open(&path).await.unwrap();
    worker.call(|db| {
        assert_eq!(db.query_row("SELECT SUM(logical_bytes) FROM quota_counter WHERE category IN ('operation_receipt','operation_receipt.pending_actual')",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn turn_seed_index_upgrade_is_atomic_preserves_sources_and_reopens_without_rebuilding() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("turn-index-upgrade.sqlite");
    let worker = DbWorker::open(&path).await.unwrap();
    worker.call(|db| {
        seed_thread(db,"saved")?;
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','saved','completed','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','saved','r','completed','now');
            INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('source','u','user','now','{\"text\":\"日本語🌿\"}',37,'preserve-hash');
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at)VALUES('input','input-op','{}','saved','source','r','u','applied','now');
            INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes)VALUES('old:pending','old','null','preserve-request','pending','now',8192);")?;
        Ok(())
    }).await.unwrap();
    worker.shutdown().await.unwrap();
    // Recreate the exact previous schema, then collide with the second index
    // name so the first CREATE INDEX must also roll back with version/ledger.
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute_batch(
        "DROP INDEX node_turn; DROP INDEX IF EXISTS receipt_turn_source;
        DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DELETE FROM schema_migration WHERE version>=9; DROP INDEX IF EXISTS shell_active_owner; DROP INDEX IF EXISTS run_thread_created; DROP INDEX IF EXISTS shell_terminal_time; PRAGMA user_version=8;
        CREATE TABLE receipt_turn_source(blocker INTEGER);",
    )
    .unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    assert!(matches!(
        DbWorker::open(&path).await,
        Err(StorageError::Sqlite(_))
    ));
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        8
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM schema_migration WHERE version=9",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='index' AND name='node_turn'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT payload_json FROM node WHERE id='source'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "{\"text\":\"日本語🌿\"}"
    );
    db.execute("DROP TABLE receipt_turn_source", []).unwrap();
    db.close().unwrap();
    for reopen in 0..2 {
        let worker = DbWorker::open(&path).await.unwrap();
        worker.call(move |db| {
            assert_eq!(db.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0))?,SCHEMA_VERSION);
            assert_eq!(db.query_row("SELECT name FROM schema_migration WHERE version=9",[],|row|row.get::<_,String>(0))?,"0009_compaction_turn_indexes");
            for (index, expected) in [("node_turn",vec!["turn_id","id"]),("receipt_turn_source",vec![])] {
                let columns=db.prepare(&format!("PRAGMA index_info({index})"))?.query_map([],|row|row.get::<_,String>(2))?.collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(columns,expected);
            }
            assert_eq!(db.query_row("SELECT payload_json,payload_bytes,body_hash,turn_id FROM node WHERE id='source'",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?,("{\"text\":\"日本語🌿\"}".into(),37,"preserve-hash".into(),"u".into()));
            assert_eq!(db.query_row("SELECT run_id,turn_id,state FROM receipt WHERE id='input'",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?,("r".into(),"u".into(),"applied".into()));
            assert_eq!(db.query_row("SELECT state,reserved_bytes,request_hash FROM operation_receipt WHERE operation_id='old:pending'",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,String>(2)?)))?,("pending".into(),8192,"preserve-request".into()));
            if reopen==0 {
                // If migration9 were executed again, these names would collide.
                assert_eq!(db.query_row("SELECT COUNT(*) FROM schema_migration WHERE version=9",[],|row|row.get::<_,i64>(0))?,1);
            }
            Ok(())
        }).await.unwrap();
        worker.shutdown().await.unwrap();
    }
}

/// Public databases began at v8. Every supported public starting version must
/// converge to exactly the same schema, preserve rows, and reopen idempotently.
#[tokio::test]
async fn public_schema_versions_converge_to_fresh_schema_without_data_loss() {
    fn schema(db: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
        db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' AND sql IS NOT NULL ORDER BY type,name")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect()
    }
    let fresh_root = tempfile::tempdir().unwrap();
    let fresh_path = fresh_root.path().join("fresh.sqlite");
    let fresh = DbWorker::open(&fresh_path).await.unwrap();
    fresh.shutdown().await.unwrap();
    let expected = schema(&Connection::open(&fresh_path).unwrap()).unwrap();
    let migrations = [
        include_str!("../migrations/0001_initial.sql"),
        include_str!("../migrations/0002_gc_indexes.sql"),
        include_str!("../migrations/0003_context_indexes.sql"),
        include_str!("../migrations/0004_attachment_provenance.sql"),
        include_str!("../migrations/0005_origin_credentials.sql"),
        include_str!("../migrations/0006_receipt_budget.sql"),
        include_str!("../migrations/0007_usage_and_source_indexes.sql"),
        include_str!("../migrations/0008_receipt_accounting_compat.sql"),
        include_str!("../migrations/0009_compaction_turn_indexes.sql"),
        include_str!("../migrations/0010_hot_query_indexes.sql"),
        include_str!("../migrations/0011_current_index_metadata.sql"),
        include_str!("../migrations/0012_revisit_metadata.sql"),
    ];
    for version in [8, 9, 10, 11, 12] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("upgrade.sqlite");
        let db = Connection::open(&path).unwrap();
        // Historical fixture creation is setup, not an upgrade under test.
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        for sql in &migrations[..version] {
            db.execute_batch(sql).unwrap();
        }
        seed_thread(&db, "keep").unwrap();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES('keep-node','user','now','{\"text\":\"preserved\"}',20,'preserved-hash')", []).unwrap();
        db.execute_batch("COMMIT").unwrap();
        db.close().unwrap();
        for _ in 0..2 {
            let writer = DbWorker::open(&path).await.unwrap();
            writer.shutdown().await.unwrap();
            let db = Connection::open(&path).unwrap();
            assert_eq!(schema(&db).unwrap(), expected, "starting v{version}");
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                SCHEMA_VERSION
            );
            assert_eq!(
                db.query_row(
                    "SELECT body_hash FROM node WHERE id='keep-node'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "preserved-hash"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread WHERE id='keep'", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }
}

#[tokio::test]
async fn schema_eleven_cleans_only_noncurrent_derived_failure_counters_once() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("metadata.sqlite");
    let writer = DbWorker::open(&path).await.unwrap();
    writer.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute_batch("CREATE INDEX receipt_turn_source ON receipt(turn_id,target_thread_id,message_node_id); DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DELETE FROM schema_migration WHERE version>=11; PRAGMA user_version=10;").unwrap();
    for (name, value) in [
        ("integer", "0"),
        ("invalid", "not json"),
        ("array", "[]"),
        ("null", "null"),
        ("badcause", r#"{"cause":3,"count":1}"#),
        ("badcount", r#"{"cause":"transient","count":"1"}"#),
        ("negative", r#"{"cause":"transient","count":-1}"#),
        ("current", r#"{"cause":"transient","count":2}"#),
    ] {
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
            params![format!("index.common_failures.{name}"), value],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO app_meta(key,value)VALUES('index.pause.saved','preserve pause')",
        [],
    )
    .unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    let writer = DbWorker::open(&path).await.unwrap();
    writer.call(|db| {
        let rows = db.prepare("SELECT key,value FROM app_meta WHERE key GLOB 'index.common_failures.*' ORDER BY key")?.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(rows, vec![("index.common_failures.current".into(),r#"{"cause":"transient","count":2}"#.into())]);
        assert_eq!(db.query_row("SELECT value FROM app_meta WHERE key='index.pause.saved'",[],|r|r.get::<_,String>(0))?,"preserve pause");
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE name='receipt_turn_source'",[],|r|r.get::<_,i64>(0))?,0);
        // A damaged current-format DB must not be silently reset on every open.
        db.execute("INSERT INTO app_meta(key,value)VALUES('index.common_failures.damaged','0')",[])?;
        Ok(())
    }).await.unwrap();
    writer.shutdown().await.unwrap();
    let writer = DbWorker::open(&path).await.unwrap();
    writer
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT value FROM app_meta WHERE key='index.common_failures.damaged'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "0"
            );
            Ok(())
        })
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema_twelve_extends_revisit_byte_accounting_without_rewriting_metadata() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("revisit.sqlite");
    let writer = DbWorker::open(&path).await.unwrap();
    writer.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    // Batch historical fixture setup; the production upgrade below still commits normally.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute_batch("DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DELETE FROM schema_migration WHERE version>=12; PRAGMA user_version=11; DROP INDEX gc_bytes_app_meta_history; CREATE INDEX gc_bytes_app_meta_history ON app_meta(octet_length(value)) WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%'; INSERT INTO app_meta(key,value)VALUES('revisit.fixture','preserve bounded metadata');").unwrap();
    db.execute_batch("COMMIT").unwrap();
    db.close().unwrap();
    let writer = DbWorker::open(&path).await.unwrap();
    writer.call(|db| {
        assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?,SCHEMA_VERSION);
        assert_eq!(db.query_row("SELECT value FROM app_meta WHERE key='revisit.fixture'",[],|r|r.get::<_,String>(0))?,"preserve bounded metadata");
        let index:String=db.query_row("SELECT sql FROM sqlite_schema WHERE name='gc_bytes_app_meta_history'",[],|r|r.get(0))?;
        assert!(index.contains("revisit.%"));
        let size:i64=db.query_row("SELECT sum(octet_length(value)) FROM app_meta INDEXED BY gc_bytes_app_meta_history WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%'",[],|r|r.get(0))?;
        assert_eq!(size,"preserve bounded metadata".len() as i64);
        Ok(())
    }).await.unwrap();
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema_thirteen_records_positive_admission_evidence_and_keeps_uncertain_inputs_unknown() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("admission.sqlite");
    let writer = DbWorker::open(&path).await.unwrap();
    writer.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DELETE FROM schema_migration WHERE version>=13; PRAGMA user_version=12;").unwrap();
    seed_thread(&db, "t").unwrap();
    db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','stopped','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','stopped','0');").unwrap();
    for (id, state) in [
        ("applied", "applied"),
        ("path", "held"),
        ("manifest", "accepted"),
        ("unproven", "held"),
    ] {
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','user','0','{}')",[id]).unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at)VALUES(?1,?1,'null','t',?1,'r','u',?2,'0')",params![id,state]).unwrap();
    }
    db.execute_batch("INSERT INTO thread_path VALUES('t',0,'path'); UPDATE thread SET cursor_node_id='path' WHERE id='t'; INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','u',0,0,'p','m',0,'incomplete','{\"receipts\":[\"manifest\"]}','0'); COMMIT;").unwrap();
    db.close().unwrap();
    for _ in 0..2 {
        let writer = DbWorker::open(&path).await.unwrap();
        writer.call(|db| {
            for id in ["applied","path","manifest"] {
                assert_eq!(db.query_row("SELECT path_admission FROM receipt WHERE id=?1",[id],|r|r.get::<_,String>(0))?,"admitted");
            }
            assert_eq!(db.query_row("SELECT path_admission FROM receipt WHERE id='unproven'",[],|r|r.get::<_,String>(0))?,"unknown");
            assert_eq!(db.query_row("SELECT run_id,turn_id FROM receipt WHERE id='unproven'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?,("r".into(),"u".into()));
            db.execute("INSERT OR IGNORE INTO node(id,kind,created_at,payload_json)VALUES('fresh','user','0','{}')",[])?;
            db.execute("INSERT OR IGNORE INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('fresh','fresh','null','t','fresh','accepted','0')",[])?;
            assert_eq!(db.query_row("SELECT path_admission FROM receipt WHERE id='fresh'",[],|r|r.get::<_,String>(0))?,"unadmitted");
            Ok(())
        }).await.unwrap();
        writer.shutdown().await.unwrap();
    }
}
