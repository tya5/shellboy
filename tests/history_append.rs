use rusqlite::params;
use shellboy::{
    history::{append_existing_nodes, select_path},
    storage::DbWorker,
};

async fn fixture() -> (tempfile::TempDir, DbWorker) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("history.sqlite"))
        .await
        .unwrap();
    db.call(|db| {
        for id in ["full", "tail"] {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/AGENTS.md','p','m','before')", [id])?;
        }
        for id in ["a", "b", "c", "d"] {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'user','before','{}',2)", [id])?;
        }
        Ok(())
    }).await.unwrap();
    (root, db)
}

#[tokio::test]
async fn tail_adoption_matches_full_selection_and_skips_selected_duplicates() {
    let (_root, db) = fixture().await;
    db.call(|db| {
        let tx = db.transaction()?;
        for thread in ["full", "tail"] {
            select_path(&tx, thread, 0, &["a".into(), "b".into()], "before")?;
        }
        select_path(&tx, "full", 1, &["a".into(), "b".into(), "c".into(), "d".into()], "after")?;
        assert_eq!(append_existing_nodes(&tx, "tail", 1, &["a".into(), "c".into(), "c".into(), "d".into()], "after")?, 2);
        let mut states = Vec::new();
        for thread in ["full", "tail"] {
            let path = tx.prepare("SELECT ordinal,node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
                .query_map([thread], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let state = tx.query_row("SELECT cursor_node_id,selection_revision,revision,updated_at FROM thread WHERE id=?1", [thread], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)))?;
            states.push((path, state));
        }
        assert_eq!(states[0], states[1]);
        let edges = tx.prepare("SELECT src,dst FROM edge ORDER BY src,dst")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(edges, vec![("a".into(), "b".into()), ("b".into(), "c".into()), ("c".into(), "d".into())]);
        let root: String = tx.query_row("SELECT value FROM app_meta WHERE key='root_revision'", [], |r| r.get(0))?;
        assert_eq!(root, "4");
        assert_eq!(append_existing_nodes(&tx, "tail", 2, &["a".into(), "d".into()], "noop")?, 0);
        assert_eq!(tx.query_row("SELECT value FROM app_meta WHERE key='root_revision'", [], |r| r.get::<_, String>(0))?, root);
        assert!(append_existing_nodes(&tx, "tail", 1, &[], "stale").is_err());
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn duplicate_edges_never_enter_insert_triggers_but_new_cycles_are_rejected() {
    let (_root, db) = fixture().await;
    db.call(|db| {
        let tx = db.transaction()?;
        select_path(&tx, "tail", 0, &["a".into(), "b".into()], "before")?;
        tx.execute_batch("CREATE TEMP TRIGGER fail_duplicate_edge BEFORE INSERT ON edge WHEN EXISTS(SELECT 1 FROM edge WHERE src=NEW.src AND dst=NEW.dst) BEGIN SELECT RAISE(ABORT,'duplicate edge reached INSERT'); END;")?;
        // Both full selection and append can encounter an already stored edge.
        select_path(&tx, "full", 0, &["a".into(), "b".into()], "same")?;
        tx.execute("INSERT INTO edge(src,dst)VALUES('b','d')", [])?;
        assert_eq!(append_existing_nodes(&tx, "tail", 1, &["d".into()], "after")?, 1);
        tx.execute("INSERT INTO edge(src,dst)VALUES('c','a')", [])?;
        let error = append_existing_nodes(&tx, "tail", 2, &["c".into()], "cycle").unwrap_err();
        assert!(error.to_string().contains("conversation edge cycle"));
        tx.rollback()?;
        assert_eq!(db.query_row("SELECT COUNT(*) FROM edge", [], |r| r.get::<_, i64>(0))?, 0);
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn cursor_mismatch_and_missing_node_refuse_without_partial_commit() {
    let (_root, db) = fixture().await;
    db.call(|db| {
        let tx = db.transaction()?;
        tx.execute("UPDATE thread SET cursor_node_id='a' WHERE id='tail'", [])?;
        assert!(append_existing_nodes(&tx, "tail", 0, &["b".into()], "bad").is_err());
        tx.rollback()?;
        let tx = db.transaction()?;
        assert!(
            append_existing_nodes(&tx, "tail", 0, &["a".into(), "missing".into()], "bad").is_err()
        );
        tx.rollback()?;
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        let tx = db.transaction()?;
        tx.execute("UPDATE thread SET deleted_at='now' WHERE id='tail'", [])?;
        assert!(append_existing_nodes(&tx, "tail", 0, &["a".into()], "bad").is_err());
        tx.rollback()?;
        let tx = db.transaction()?;
        assert_eq!(
            append_existing_nodes(&tx, "tail", 0, &["a".into()], "valid")?,
            1
        );
        assert_eq!(
            tx.query_row(
                "SELECT ordinal FROM thread_path WHERE thread_id=?1 AND node_id=?2",
                params!["tail", "a"],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}
