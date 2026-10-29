//! Real SQLite adoption, restore and rollback boundaries for deterministic edits.
use super::*;

pub(super) struct Fixture {
    root: tempfile::TempDir,
    pub(super) db: DbWorker,
    pub(super) compactor: Compactor,
}
impl Fixture {
    pub(super) async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("context.db"))
            .await
            .unwrap();
        db.call(|db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now')", [])?;
            Ok(())
        }).await.unwrap();
        let compactor = Compactor::new(db.clone(), Arc::new(Config::default()));
        Self {
            root,
            db,
            compactor,
        }
    }
    pub(super) async fn append(&self, id: &str, turn: Option<&str>, payload: Value) {
        let (id, turn) = (id.to_owned(), turn.map(str::to_owned));
        self.db
            .call(move |db| {
                let tx = db.transaction()?;
                history::append_node(&tx, "t", &id, turn.as_deref(), "message", &payload, "now")?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
    }
    pub(super) async fn text(&self, id: &str, text: &str) {
        self.append(id, None, json!({"role":"user","content":text}))
            .await;
    }
    pub(super) async fn path(&self) -> Vec<String> {
        self.db
            .call(|db| {
                Ok(db
                    .prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal",
                    )?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
            .unwrap()
    }
    async fn attachment(&self, source: &str) {
        let source = source.to_owned();
        self.db.call(move |db| {
            db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('attachment',?1,'/synthetic.txt','file','external','now')", [source])?;
            db.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('t','attachment','active','now',0)", [])?;
            Ok(())
        }).await.unwrap();
    }
    async fn tools(&self) {
        self.db.call(|db| {
            db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('old-run','t','completed','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('old-turn','t','old-run','completed','now');
                INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('old-attempt','old-turn',0,0,'p','m',0,'completed','{}','now');
                INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g-old','old-attempt','old-turn','completed','now'),('g-new','old-attempt','old-turn','completed','now');")?;
            Ok(())
        }).await.unwrap();
        for (group, size) in [("g-old", 40_000), ("g-new", 10)] {
            self.append(&format!("{group}-call"), Some("old-turn"), json!({"group_id":group,"output":[{"type":"function_call","name":"shell","call_id":group,"arguments":"{\"command\":\"printf fixture\"}"}]})).await;
            self.append(&format!("{group}-result"), Some("old-turn"), json!({"group_id":group,"item":{"type":"function_call_output","call_id":group,"output":"x".repeat(size)}})).await;
            let group = group.to_owned();
            self.db.call(move |db| {
                db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,?1,0,'shell','{}','completed')", [&group])?;
                db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?1,?2,'completed')", params![group,format!("{group}-result")])?;
                Ok(())
            }).await.unwrap();
        }
    }
    async fn replacement_count(&self) -> i64 {
        self.db
            .call(|db| Ok(db.query_row("SELECT count(*) FROM replacement", [], |row| row.get(0))?))
            .await
            .unwrap()
    }
    async fn collect_history(&self) {
        let config = Arc::new(Config::default());
        let attachments = Arc::new(crate::attachments::AttachmentService::new(
            self.db.clone(),
            &config,
            self.root.path().to_owned(),
            "fixture".into(),
        ));
        let gc = crate::gc::GcService::new(
            self.db.clone(),
            config,
            self.root.path().to_owned(),
            attachments,
        );
        if let Some(job) = gc.start_history().await.unwrap() {
            for _ in 0..64 {
                if gc.history_step(&job).await.unwrap().complete {
                    return;
                }
            }
            panic!("small fixture history GC did not finish");
        }
    }
    pub(super) async fn close(self) {
        self.db.shutdown().await.unwrap();
        drop(self.root);
    }
}

#[tokio::test]
async fn middle_omission_restore_preserves_later_nodes_and_attachment_exclusion() {
    let fixture = Fixture::new().await;
    for id in ["a", "b", "c", "d", "e"] {
        fixture.text(id, id).await;
    }
    fixture.attachment("b").await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .omit_range(snapshot, 1..3, CancellationToken::new())
        .await
        .unwrap();
    let marker = result.marker_node_id.unwrap();
    assert_eq!(
        fixture.path().await,
        vec!["a".to_owned(), marker.clone(), "d".into(), "e".into()]
    );
    fixture.collect_history().await;
    fixture
        .db
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM node WHERE id IN ('b','c') AND payload_json IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                2,
                "thread replacement provenance must retain omitted sources during GC"
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.text("later", "appended after edit").await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let restored = fixture
        .compactor
        .restore_marker(snapshot, Some(&marker), CancellationToken::new())
        .await
        .unwrap();
    assert!(
        restored
            .warnings
            .iter()
            .any(|warning| warning.contains("1 excluded attachment"))
    );
    assert_eq!(fixture.path().await, ["a", "b", "c", "d", "e", "later"]);
    fixture
        .db
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT state FROM attachment_selection WHERE attachment_id='attachment'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "excluded"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread_replacement", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM replacement_source WHERE source_node_id IN ('b','c')",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn automatic_trim_keeps_current_request_between_omitted_ranges() {
    let fixture = Fixture::new().await;
    fixture.text("old", &"x".repeat(20_000)).await;
    fixture.db.call(|db| {
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','t','run','running','now');")?;
        Ok(())
    }).await.unwrap();
    fixture
        .append(
            "request",
            Some("turn"),
            json!({"role":"user","content":"retain current request"}),
        )
        .await;
    fixture
        .append(
            "middle",
            Some("turn"),
            json!({"role":"assistant","content":"y".repeat(20_000)}),
        )
        .await;
    fixture
        .append(
            "latest",
            Some("turn"),
            json!({"role":"assistant","content":"latest group"}),
        )
        .await;
    let original = fixture.path().await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .trim_maximal(snapshot, CancellationToken::new())
        .await
        .unwrap();
    assert!(!result.compact.no_target);
    assert_eq!(result.omitted_nodes, 2);
    let path = fixture.path().await;
    assert!(path.contains(&"request".to_owned()));
    assert_eq!(path.last().unwrap(), "latest");
    fixture.text("later", "later accepted node").await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    fixture
        .compactor
        .restore_marker(
            snapshot,
            result.marker_node_id.as_deref(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut expected = original;
    expected.push("later".into());
    assert_eq!(fixture.path().await, expected);
    fixture.close().await;
}

#[tokio::test]
async fn maximum_omission_adopts_available_reduction_and_attachment_exclusion() {
    let fixture = Fixture::new().await;
    fixture.tools().await;
    fixture.attachment("g-old-result").await;
    let before = fixture.path().await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .trim_maximal(snapshot, CancellationToken::new())
        .await
        .unwrap();
    assert!(!result.compact.no_target);
    assert_ne!(fixture.path().await, before);
    assert_eq!(result.omitted_nodes, 2);
    assert!(result.target_tokens.is_none());
    assert_eq!(fixture.replacement_count().await, 1);
    fixture
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT state FROM attachment_selection", [], |row| row
                    .get::<_, String>(
                    0
                ))?,
                "excluded"
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn automatic_maximum_omits_old_tool_group_and_keeps_latest_complete_tool_group() {
    let fixture = Fixture::new().await;
    fixture.tools().await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .trim_maximal(snapshot, CancellationToken::new())
        .await
        .unwrap();
    assert!(!result.compact.no_target);
    assert_eq!(result.compact.spilled_groups, 0);
    assert_eq!(result.omitted_nodes, 2);
    assert!(result.marker_node_id.is_some());
    assert!(result.target_tokens.is_none());
    assert!(result.after_tokens < result.before_tokens);
    let path = fixture.path().await;
    assert_eq!(&path[1..], ["g-new-call", "g-new-result"]);
    fixture.close().await;
}

#[tokio::test]
async fn cancelled_stale_selection_and_stopping_run_reject_without_replacements() {
    let fixture = Fixture::new().await;
    fixture.text("a", "old").await;
    fixture.text("b", "middle").await;
    fixture.text("c", "latest").await;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    assert!(
        fixture
            .compactor
            .omit_range(snapshot, 0..1, cancelled)
            .await
            .is_err()
    );
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    fixture.text("later", "racing selection").await;
    assert!(
        fixture
            .compactor
            .omit_range(snapshot, 0..1, CancellationToken::new())
            .await
            .is_err()
    );
    fixture
        .db
        .call(|db| {
            db.execute(
                "INSERT INTO run(id,thread_id,state,created_at)VALUES('stopping','t','live','now')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    fixture
        .db
        .call(|db| {
            db.execute("UPDATE run SET state='stopping' WHERE id='stopping'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        fixture
            .compactor
            .omit_range(snapshot, 0..1, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(fixture.replacement_count().await, 0);
    assert_eq!(fixture.path().await, ["a", "b", "c", "later"]);
    fixture.close().await;
}

#[tokio::test]
async fn estimates_use_replay_unicode_and_same_model_usage_with_current_fixed_overhead() {
    let fixture = Fixture::new().await;
    fixture.text("unicode", "日本語🦀").await;
    fixture
        .append(
            "audit",
            None,
            json!({"source_node_ids":["private-provenance"],"metadata":"z".repeat(10_000)}),
        )
        .await;
    fixture.append("expired-reasoning", None, json!({"reasoning_epoch":99,"output":[{"type":"reasoning","encrypted_content":"z".repeat(10_000)}]})).await;
    fixture.db.call(|db| {
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('past','t','completed','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('past','t','past','completed','now');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES
            ('baseline','past',0,0,'p','m',0,'completed','{\"input_characters\":100,\"history_characters\":80}','{\"input_tokens\":50,\"input_tokens_details\":{\"cached_tokens\":0}}','now'),
            ('other-model','past',0,0,'p','other',0,'completed','{\"input_characters\":100,\"history_characters\":80}','{\"input_tokens\":99999}','now'),
            ('legacy','past',0,0,'p','m',0,'completed','{}','{\"input_tokens\":99999}','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('current','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('current','t','current','running','now');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)
            VALUES('inflight','current',0,(SELECT selection_revision FROM thread WHERE id='t'),'p','m',0,'inflight','{\"input_characters\":200,\"history_characters\":100}','now');")?;
        Ok(())
    }).await.unwrap();
    let snapshot = fixture.compactor.snapshot("t", false).await.unwrap();
    let characters = json!([{"role":"user","content":"日本語🦀"}])
        .to_string()
        .chars()
        .count() as u64;
    assert_eq!(
        history_characters(&snapshot),
        characters,
        "audit metadata and old-epoch reasoning must not count as input"
    );
    assert_eq!(snapshot.estimate.attempt_id.as_deref(), Some("baseline"));
    assert_eq!(snapshot.estimate.overhead(), Some(100));
    assert_eq!(estimated_tokens(&snapshot), (characters + 100).div_ceil(2));
    assert_eq!(
        estimated_suffix_tokens(&snapshot, 0),
        (characters - 1).div_ceil(2),
        "cache suffix excludes fixed system overhead"
    );
    let mut reduced = snapshot.clone();
    reduced.groups.truncate(1);
    assert_eq!(
        estimated_tokens(&reduced),
        estimated_tokens(&snapshot),
        "planning clones share the calibrated baseline"
    );
    fixture
        .db
        .call(|db| {
            db.execute(
                "UPDATE model_attempt SET state='failed' WHERE id='inflight'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", false).await.unwrap();
    assert_eq!(snapshot.estimate.overhead(), Some(20));
    assert_eq!(estimated_tokens(&snapshot), (characters + 20).div_ceil(2));
    fixture
        .db
        .call(|db| {
            db.execute(
                "UPDATE model_attempt SET usage_json='{}' WHERE id='baseline'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", false).await.unwrap();
    assert_eq!(snapshot.estimate.attempt_id, None);
    assert_eq!(snapshot.estimate.overhead(), None);
    assert_eq!(estimated_tokens(&snapshot), characters.div_ceil(4));
    fixture.close().await;
}

#[tokio::test]
async fn missing_or_deleted_context_targets_are_domain_errors_not_storage_failures() {
    let fixture = Fixture::new().await;
    for target in ["missing", "t"] {
        if target == "t" {
            fixture
                .db
                .call(|db| {
                    db.execute("UPDATE thread SET deleted_at='now' WHERE id='t'", [])?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        let error = fixture.compactor.context_metrics(target).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::ThreadNotFound);
        assert!(!error.is_storage_failure());
    }
    fixture.close().await;
}

#[tokio::test]
async fn activation_measures_failed_request_not_later_appended_input() {
    let fixture = Fixture::new().await;
    fixture.text("old", "small failed input").await;
    fixture.db.call(|db| {
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('current','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('current','t','current','running','now');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)
            VALUES('failed-request','current',0,(SELECT selection_revision FROM thread WHERE id='t'),'p','m',0,'inflight','{\"input_characters\":4000,\"history_characters\":100}','now');")?;
        Ok(())
    }).await.unwrap();
    fixture
        .text("new-pending-input", &"x".repeat(400_000))
        .await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    assert!(estimated_tokens(&snapshot) > 75_000);
    let original = fixture.path().await;
    let prepared_characters = fixture.db.call(|db| {
        Ok(db.query_row("SELECT json_extract(input_manifest_json,'$.input_characters') FROM model_attempt WHERE id='failed-request'", [], |row| row.get::<_, i64>(0))?)
    }).await.unwrap();
    assert_eq!(
        fixture
            .compactor
            .input_tokens("t", "p", "m", prepared_characters.try_into().unwrap())
            .await
            .unwrap(),
        1_000,
        "the actual prepared request remains below activation even after a large input is appended later"
    );
    assert_eq!(fixture.path().await, original, "activation is read-only");
    fixture.close().await;
}

#[tokio::test]
async fn maximum_plan_does_not_stop_after_the_first_old_tool_group() {
    let fixture = Fixture::new().await;
    fixture.tools().await;
    fixture.db.call(|db| {
        db.execute_batch("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g-last','old-attempt','old-turn','completed','now');
            INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g-last','g-last',0,'shell','{}','completed');")?;
        let body = json!({"group_id":"g-new","item":{"type":"function_call_output","call_id":"g-new","output":"y".repeat(28_000)}}).to_string();
        db.execute("UPDATE node SET payload_json=?1,payload_bytes=?2 WHERE id='g-new-result'", params![body,body.len() as i64])?;
        Ok(())
    }).await.unwrap();
    fixture.append("g-last-call", Some("old-turn"), json!({"group_id":"g-last","output":[{"type":"function_call","name":"shell","call_id":"g-last","arguments":"{}"}]})).await;
    fixture.append("g-last-result", Some("old-turn"), json!({"group_id":"g-last","item":{"type":"function_call_output","call_id":"g-last","output":"latest"}})).await;
    fixture.db.call(|db| {
        db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g-last','g-last','g-last-result','completed')", [])?;
        Ok(())
    }).await.unwrap();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .trim_maximal(snapshot, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.compact.spilled_groups, 0);
    assert_eq!(result.omitted_nodes, 4);
    let path = fixture.path().await;
    assert!(!path.contains(&"g-old-result".to_owned()));
    assert!(!path.contains(&"g-new-result".to_owned()));
    assert!(path.contains(&"g-last-result".to_owned()));
    fixture.close().await;
}

#[tokio::test]
async fn default_restore_undoes_newest_selected_edit_even_when_earlier_on_path() {
    let fixture = Fixture::new().await;
    for id in ["a", "b", "c", "d", "e", "f"] {
        fixture.text(id, id).await;
    }
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let later_marker = fixture
        .compactor
        .omit_range(snapshot, 3..4, CancellationToken::new())
        .await
        .unwrap()
        .marker_node_id
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let earlier_marker = fixture
        .compactor
        .omit_range(snapshot, 1..2, CancellationToken::new())
        .await
        .unwrap()
        .marker_node_id
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let restored = fixture
        .compactor
        .restore_marker(snapshot, None, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        restored.marker_node_id.as_deref(),
        Some(earlier_marker.as_str())
    );
    assert_eq!(
        fixture.path().await,
        vec![
            "a".into(),
            "b".into(),
            "c".into(),
            later_marker.clone(),
            "e".into(),
            "f".into()
        ]
    );
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let restored = fixture
        .compactor
        .restore_marker(snapshot, None, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        restored.marker_node_id.as_deref(),
        Some(later_marker.as_str())
    );
    assert_eq!(fixture.path().await, ["a", "b", "c", "d", "e", "f"]);
    fixture.close().await;
}

#[tokio::test]
async fn marker_explicitly_reports_current_turn_work_after_the_retained_request() {
    let fixture = Fixture::new().await;
    fixture.text("old", &"x".repeat(20_000)).await;
    fixture.db.call(|db| {
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','t','run','running','now');")?;Ok(())
    }).await.unwrap();
    fixture
        .append(
            "request",
            Some("turn"),
            json!({"role":"user","content":"retain request"}),
        )
        .await;
    fixture
        .append(
            "work",
            Some("turn"),
            json!({"role":"assistant","content":"y".repeat(20_000)}),
        )
        .await;
    fixture
        .append(
            "latest",
            Some("turn"),
            json!({"role":"assistant","content":"latest"}),
        )
        .await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .trim_maximal(snapshot, CancellationToken::new())
        .await
        .unwrap();
    assert!(result.warnings.is_empty());
    let marker = result.marker_node_id.unwrap();
    let payload = fixture
        .db
        .call(move |db| {
            let raw: String = db.query_row(
                "SELECT payload_json FROM node WHERE id=?1",
                [marker],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str::<Value>(&raw).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(payload["context_trim"]["after_current_request_nodes"], 1);
    let text = payload["item"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("1 source nodes AFTER the retained current user request"));
    assert!(text.contains("intermediate work in the current Turn was also omitted"));
    assert!(
        text.contains("have NOT been undone")
            && text.contains("Inspect the current state before repeating")
    );
    assert!(text.contains("compact --restore --from old"));
    assert!(text.contains("rag search --scope history:t --mode keyword"));
    assert!(text.contains("Restoring a large range may reproduce the same failure"));
    fixture.close().await;
}

#[tokio::test]
async fn middle_omission_attachment_notice_and_warning_only_describe_new_exclusions() {
    for attachment_state in [None, Some("active"), Some("excluded")] {
        let fixture = Fixture::new().await;
        for id in ["a", "b", "c", "d"] {
            fixture.text(id, id).await;
        }
        if let Some(state) = attachment_state {
            fixture.attachment("b").await;
            if state == "excluded" {
                fixture.db.call(|db| {db.execute("UPDATE attachment_selection SET state='excluded',reason='explicit_user_exclusion'",[])?;Ok(())}).await.unwrap();
            }
        }
        let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
        let result = fixture
            .compactor
            .omit_range(snapshot, 1..3, CancellationToken::new())
            .await
            .unwrap();
        let active = attachment_state == Some("active");
        assert_eq!(result.warnings.len(), usize::from(active));
        let marker = result.marker_node_id.unwrap();
        let payload = fixture
            .db
            .call(move |db| {
                let raw: String = db.query_row(
                    "SELECT payload_json FROM node WHERE id=?1",
                    [marker],
                    |row| row.get(0),
                )?;
                Ok(serde_json::from_str::<Value>(&raw).unwrap())
            })
            .await
            .unwrap();
        assert_eq!(
            payload["context_trim"]["excluded_attachment_refs"],
            if active { 1 } else { 0 }
        );
        let text = payload["item"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("a middle range of the selected history was omitted"));
        assert_eq!(
            text.contains("active attachment references were excluded"),
            active
        );
        if active {
            assert!(result.warnings[0].contains("1 active attachment"));
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn restore_from_first_omitted_source_targets_that_marker_instead_of_the_latest_edit() {
    let fixture = Fixture::new().await;
    for id in ["a", "b", "c", "d", "e", "f", "g"] {
        fixture.text(id, id).await;
    }
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let old = fixture
        .compactor
        .omit_range(snapshot, 1..3, CancellationToken::new())
        .await
        .unwrap()
        .marker_node_id
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let selected = range::from_nodes(&snapshot, "e", "e").unwrap();
    let newer = fixture
        .compactor
        .omit_range(snapshot, selected, CancellationToken::new())
        .await
        .unwrap()
        .marker_node_id
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .restore_marker(snapshot, Some("b"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.marker_node_id, Some(old));
    assert_eq!(
        fixture.path().await,
        vec![
            "a".to_owned(),
            "b".into(),
            "c".into(),
            "d".into(),
            newer,
            "f".into(),
            "g".into()
        ]
    );
    assert!(result.warnings.is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn ambiguous_first_source_restore_requires_an_explicit_marker_id_without_editing_the_path() {
    let fixture = Fixture::new().await;
    for id in ["a", "b", "c", "d", "e", "f"] {
        fixture.text(id, id).await;
    }
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    fixture
        .compactor
        .omit_range(snapshot, 1..2, CancellationToken::new())
        .await
        .unwrap();
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let selected = range::from_nodes(&snapshot, "d", "d").unwrap();
    let marker = fixture
        .compactor
        .omit_range(snapshot, selected, CancellationToken::new())
        .await
        .unwrap()
        .marker_node_id
        .unwrap();
    fixture.db.call(move|db| {
        db.execute("UPDATE node SET payload_json=json_set(payload_json,'$.context_trim.first_node_id','b'),payload_revision=payload_revision+1 WHERE id=?1",[marker])?;Ok(())
    }).await.unwrap();
    let before = fixture.path().await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let error = fixture
        .compactor
        .restore_marker(snapshot, Some("b"), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("multiple selected markers"));
    assert_eq!(
        error.details.unwrap()["marker_node_ids"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(fixture.path().await, before);
    fixture.close().await;
}

#[tokio::test]
async fn changed_attachment_selection_aborts_without_a_false_exclusion_notice_or_path_change() {
    let fixture = Fixture::new().await;
    for id in ["a", "b", "c"] {
        fixture.text(id, id).await;
    }
    fixture.attachment("a").await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    fixture.db.call(|db| {db.execute("UPDATE attachment_selection SET state='excluded',reason='explicit_user_exclusion',revision=revision+1",[])?;Ok(())}).await.unwrap();
    let before = fixture.path().await;
    let error = fixture
        .compactor
        .omit_range(snapshot, 0..1, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("attachment selection changed"));
    assert_eq!(fixture.path().await, before);
    assert_eq!(fixture.replacement_count().await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn maximum_trim_adopts_a_small_safe_reduction_even_when_retained_suffix_exceeds_half() {
    let fixture = Fixture::new().await;
    fixture.text("old", &"x".repeat(20_000)).await;
    fixture.text("latest", &"y".repeat(400_000)).await;
    let result = fixture
        .compactor
        .trim_activated("t", CancellationToken::new())
        .await
        .unwrap();
    assert!(!result.compact.no_target);
    assert_eq!(result.omitted_nodes, 1);
    assert!(result.target_tokens.is_none());
    assert!(result.after_tokens < result.before_tokens);
    assert!(
        result.after_tokens > result.before_tokens / 2,
        "max omission must not require reaching half"
    );
    assert_eq!(fixture.path().await.last().unwrap(), "latest");
    fixture
        .db
        .call(|db| {
            assert!(db.query_row(
                "SELECT payload_json IS NOT NULL FROM node WHERE id='old'",
                [],
                |r| r.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn no_unprotected_groups_leave_history_unchanged() {
    let fixture = Fixture::new().await;
    fixture.text("latest", &"y".repeat(400_000)).await;
    let before = fixture.path().await;
    let result = fixture
        .compactor
        .trim_activated("t", CancellationToken::new())
        .await
        .unwrap();
    assert!(result.compact.no_target);
    assert_eq!(result.omitted_nodes, 0);
    assert_eq!(fixture.path().await, before);
    assert_eq!(fixture.replacement_count().await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn latest_complete_tool_group_survives_even_with_a_later_assistant_group() {
    let fixture = Fixture::new().await;
    fixture.tools().await;
    fixture.text("latest-answer", "last answer").await;
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    let result = fixture
        .compactor
        .trim_maximal(snapshot, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.omitted_nodes, 2);
    let path = fixture.path().await;
    assert!(!path.contains(&"g-old-call".to_owned()));
    assert!(path.contains(&"g-new-call".to_owned()));
    assert!(path.contains(&"g-new-result".to_owned()));
    assert_eq!(path.last().unwrap(), "latest-answer");
    fixture.close().await;
}

#[tokio::test]
async fn pending_protection_and_incomplete_groups_are_never_automatic_omission_candidates() {
    let fixture = Fixture::new().await;
    fixture.text("pending", &"p".repeat(20_000)).await;
    fixture.text("incomplete", &"i".repeat(20_000)).await;
    fixture.text("latest", "retain latest").await;
    let before = fixture.path().await;
    let mut snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    snapshot.protected.insert("pending".into());
    snapshot.groups[1].complete = false;
    let result = fixture
        .compactor
        .trim_maximal(snapshot, CancellationToken::new())
        .await
        .unwrap();
    assert!(result.compact.no_target);
    assert_eq!(fixture.path().await, before);
    assert_eq!(fixture.replacement_count().await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn activation_uses_the_prepared_request_count_instead_of_resnapshotting_a_changed_path() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture
            .compactor
            .input_tokens("t", "p", "m", 400)
            .await
            .unwrap(),
        100
    );
    fixture.text("later", &"x".repeat(400_000)).await;
    assert_eq!(
        fixture
            .compactor
            .input_tokens("t", "p", "m", 400)
            .await
            .unwrap(),
        100,
        "already prepared small requests must not use later selected history as activation input"
    );
    fixture.close().await;
}
