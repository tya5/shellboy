//! Hidden-source summaries reuse the regular bounded summary executor.
use super::tests::Fixture;
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

async fn summary_server(
    parts: usize,
    gate: bool,
) -> (
    crate::config::Connection,
    mpsc::Receiver<Value>,
    oneshot::Sender<()>,
    JoinHandle<usize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = crate::config::Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        stream_idle_timeout_ms: 0,
        response_header_timeout_ms: 120_000,
        ..Default::default()
    };
    let (send, received) = mpsc::channel(16);
    let (release, hold) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut hold = Some(hold);
        for part in 0..parts {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 8192];
            loop {
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..split]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() < split + 4 + length {
                    continue;
                }
                let request: Value =
                    serde_json::from_slice(&bytes[split + 4..split + 4 + length]).unwrap();
                assert_eq!(request["store"], false);
                assert_eq!(request["tools"], json!([]));
                send.send(request).await.unwrap();
                break;
            }
            if gate && part == 0 {
                hold.take().unwrap().await.unwrap();
            }
            let raw = json!({"id":format!("synthetic-summary-{part}"),"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("synthetic summary {part}")}]}]});
            let body = raw.to_string();
            stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        parts
    });
    (connection, received, release, server)
}
async fn configured_fixture(
    parts: usize,
    gate: bool,
) -> (
    Fixture,
    mpsc::Receiver<Value>,
    oneshot::Sender<()>,
    JoinHandle<usize>,
) {
    let (connection, requests, release, server) = summary_server(parts, gate).await;
    let mut fixture = Fixture::new().await;
    let config = Config {
        connections: std::collections::BTreeMap::from([("p".into(), connection)]),
        ..Default::default()
    };
    fixture.compactor = Compactor::new(fixture.db.clone(), Arc::new(config));
    (fixture, requests, release, server)
}
fn summary_args(from: &str, to: &str) -> ContextEditArgs {
    ContextEditArgs {
        max_chars: None,
        thread_id: Some("t".into()),
        action: ContextEditAction::Summary,
        from_node_id: Some(from.into()),
        to_node_id: Some(to.into()),
    }
}
async fn omit_middle(fixture: &Fixture) -> String {
    for id in ["before", "middle-a", "middle-b", "after"] {
        fixture.text(id, &format!("original text {id}")).await;
    }
    let snapshot = fixture.compactor.snapshot("t", true).await.unwrap();
    fixture
        .compactor
        .omit_range(snapshot, 1..3, CancellationToken::new())
        .await
        .unwrap()
        .marker_node_id
        .unwrap()
}
async fn raw_parts(fixture: &Fixture) -> i64 {
    fixture.db.call(|db|Ok(db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'summary.part.%' AND json_extract(value,'$.raw_response.status')='completed'",[],|row|row.get(0))?)).await.unwrap()
}

#[tokio::test]
async fn omitted_middle_summary_uses_original_data_and_retains_prefix_suffix_and_provenance() {
    let (fixture, mut requests, _release, server) = configured_fixture(1, false).await;
    let marker = omit_middle(&fixture).await;
    let snapshot_before = fixture.compactor.snapshot("t", false).await.unwrap();
    let expected_invalidated = suffix_characters(&snapshot_before, 1);
    let result = fixture
        .compactor
        .edit(
            "t",
            &summary_args("middle-a", "middle-b"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.changed_suffix_characters, expected_invalidated,
        "cache suffix is measured on the currently selected marker path, not its hidden originals"
    );
    let summary = result.compact.summary_node_id.unwrap();
    assert_eq!(
        fixture.path().await,
        vec!["before".to_owned(), summary.clone(), "after".into()]
    );
    assert!(!fixture.path().await.contains(&marker));
    let request = requests.recv().await.unwrap();
    let text = request["input"][1]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("original text middle-a") && text.contains("original text middle-b"));
    assert!(!text.contains("Shellboy context notice"));
    assert_eq!(result.compact.original_node_ids, ["middle-a", "middle-b"]);
    fixture.db.call(move|db| {
        let sources=db.prepare("SELECT s.source_node_id FROM replacement_source s JOIN replacement r ON r.id=s.replacement_id WHERE r.output_node_id=?1 ORDER BY s.ordinal")?.query_map([summary],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(sources,["middle-a","middle-b"]);
        for id in ["before","after"] {
            let payload:String=db.query_row("SELECT payload_json FROM node WHERE id=?1",[id],|row|row.get(0))?;
            assert_eq!(serde_json::from_str::<Value>(&payload).unwrap(),json!({"role":"user","content":format!("original text {id}")}));
        }Ok(())
    }).await.unwrap();
    assert_eq!(raw_parts(&fixture).await, 0); // raw part cache retired (§9)
    assert_eq!(
        crate::test_watchdog::watchdog("src/compaction/edit/summary_tests.rs: omitted_middle_summary_uses_original_data_and_retains_prefix_suffix_and_provenance", server)
            .await
            .unwrap()
            .unwrap(),
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn partial_hidden_summary_is_rejected_with_boundary_candidates_without_http() {
    let fixture = Fixture::new().await;
    let marker = omit_middle(&fixture).await;
    let before = fixture.path().await;
    let error = fixture
        .compactor
        .edit(
            "t",
            &summary_args("middle-a", "middle-a"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(
        error
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("complete first-to-last"))
    );
    assert!(
        error
            .details
            .as_ref()
            .and_then(|details| details.get("valid_boundaries"))
            .and_then(Value::as_array)
            .is_some_and(|values| !values.is_empty())
    );
    assert_eq!(
        error.details.as_ref().unwrap()["omitted_ranges"],
        json!([{"marker_node_id":marker,"from_node_id":"middle-a","to_node_id":"middle-b"}])
    );
    assert_eq!(fixture.path().await, before);
    assert!(before.contains(&marker));
    assert_eq!(raw_parts(&fixture).await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn hidden_summary_late_marker_revision_or_selection_change_rejects_adoption_and_keeps_the_original_path()
 {
    for selection_change in [false, true] {
        let (fixture, mut requests, release, server) = configured_fixture(1, true).await;
        let marker = omit_middle(&fixture).await;
        let before = fixture.path().await;
        let compactor = fixture.compactor.clone();
        let args = summary_args("middle-a", "middle-b");
        let request =
            tokio::spawn(async move { compactor.edit("t", &args, CancellationToken::new()).await });
        crate::test_watchdog::watchdog("src/compaction/edit/summary_tests.rs: hidden_summary_late_marker_revision_or_selection_change_rejects_adoption_and_keeps_the_original_path", requests.recv())
            .await
            .unwrap()
            .unwrap();
        if selection_change {
            fixture.text("later", "racing appended message").await;
        } else {
            fixture
                .db
                .call(move |db| {
                    db.execute(
                        "UPDATE node SET payload_revision=payload_revision+1 WHERE id=?1",
                        [marker],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        release.send(()).unwrap();
        let error = crate::test_watchdog::watchdog("src/compaction/edit/summary_tests.rs: hidden_summary_late_marker_revision_or_selection_change_rejects_adoption_and_keeps_the_original_path", request)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(error.message.contains("changed"));
        let mut expected = before;
        if selection_change {
            expected.push("later".into());
        }
        assert_eq!(fixture.path().await, expected);
        assert_eq!(raw_parts(&fixture).await, 0); // raw part cache retired (§9)
        fixture
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM replacement WHERE format_version=?1",
                        [FORMAT],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            crate::test_watchdog::watchdog("src/compaction/edit/summary_tests.rs: hidden_summary_late_marker_revision_or_selection_change_rejects_adoption_and_keeps_the_original_path", server)
                .await
                .unwrap()
                .unwrap(),
            1
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn explicit_tool_span_summary_sends_complete_groups_in_one_request() {
    let (fixture, mut requests, _release, server) = configured_fixture(1, false).await;
    fixture.text("before", "keep prefix").await;
    fixture.db.call(|db| {
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','u',0,0,'p','m',0,'completed','{}','now');")?;Ok(())
    }).await.unwrap();
    for group in ["g-a", "g-b"] {
        let saved = group.to_owned();
        fixture.db.call(move|db| {
            db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,'attempt','u','completed','now')",[&saved])?;
            db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,?1,0,'shell','{}','completed')",[saved])?;Ok(())
        }).await.unwrap();
        fixture.append(&format!("{group}-call"),Some("u"),json!({"group_id":group,"output":[{"type":"function_call","name":"shell","call_id":group,"arguments":"{\"command\":\"printf synthetic\"}"}]})).await;
        let result = format!("{group}-result");
        fixture.append(&result,Some("u"),json!({"group_id":group,"item":{"type":"function_call_output","call_id":group,"output":"x".repeat(35_000)}})).await;
        let saved = group.to_owned();
        fixture.db.call(move|db| {db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?1,?2,'completed')",params![saved,result])?;Ok(())}).await.unwrap();
    }
    fixture.text("after", "keep suffix").await;
    let result = fixture
        .compactor
        .edit(
            "t",
            &summary_args("g-a-call", "g-b-result"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.compact.model_requests, Some(1));
    assert_eq!(result.compact.source_nodes, 4);
    let summary = result.compact.summary_node_id.unwrap();
    assert_eq!(
        fixture.path().await,
        vec!["before".to_owned(), summary, "after".into()]
    );
    let request = requests.recv().await.unwrap();
    let text = request["input"][1]["content"][0]["text"].as_str().unwrap();
    for group in ["g-a", "g-b"] {
        assert!(
            text.contains(&format!("{group}-call")) && text.contains(&format!("{group}-result"))
        );
    }
    assert_eq!(raw_parts(&fixture).await, 0);
    assert_eq!(
        crate::test_watchdog::watchdog("src/compaction/edit/summary_tests.rs: explicit_tool_span_summary_sends_complete_groups_in_one_request", server)
            .await
            .unwrap()
            .unwrap(),
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn stale_restore_selection_and_turn_generation_never_replace_the_current_path_or_originals() {
    let fixture = Fixture::new().await;
    let marker = omit_middle(&fixture).await;
    let original_payloads=fixture.db.call(|db| {
        Ok(db.prepare("SELECT id,payload_json FROM node WHERE id IN ('before','middle-a','middle-b','after') ORDER BY id")?
            .query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap();
    let stale = fixture.compactor.snapshot("t", true).await.unwrap();
    fixture.text("later", "keep a newer accepted message").await;
    let path = fixture.path().await;
    let error = fixture
        .compactor
        .restore_marker(stale, Some(&marker), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(fixture.path().await, path);
    fixture.db.call(|db| {
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('current-run','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('current-turn','t','current-run','running','now');")?;
        Ok(())
    }).await.unwrap();
    fixture
        .append(
            "current-request",
            Some("current-turn"),
            json!({"role":"user","content":"keep current request"}),
        )
        .await;
    let stale = fixture.compactor.snapshot("t", true).await.unwrap();
    let path = fixture.path().await;
    fixture
        .db
        .call(|db| {
            db.execute(
                "UPDATE turn SET generation=generation+1 WHERE id='current-turn'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = fixture
        .compactor
        .restore_marker(stale, Some(&marker), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("generation changed"));
    assert_eq!(fixture.path().await, path);
    let final_payloads=fixture.db.call(|db| {
        Ok(db.prepare("SELECT id,payload_json FROM node WHERE id IN ('before','middle-a','middle-b','after') ORDER BY id")?
            .query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap();
    assert_eq!(final_payloads, original_payloads);
    fixture.close().await;
}

async fn large_context_trim_marker(fixture: &Fixture) -> (String, Vec<String>) {
    let ids = (0..1077)
        .map(|i| format!("00000000-0000-4000-8000-{i:012}"))
        .collect::<Vec<_>>();
    let seeded = ids.clone();
    fixture.db.call(move |db| {
        let tx=db.transaction()?;
        for (i,id) in seeded.iter().enumerate() {
            history::append_node(&tx,"t",id,None,"message",&json!({"role":"user","content":format!("original synthetic {i}: {}","x".repeat(256))}),"now")?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    let result = fixture
        .compactor
        .trim_activated("t", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.omitted_nodes, 1076);
    (result.marker_node_id.unwrap(), ids)
}
async fn saved_context_trim_marker(fixture: &Fixture, marker: &str) -> (String, String, i64) {
    let marker = marker.to_owned();
    fixture
        .db
        .call(move |db| {
            Ok(db.query_row(
                "SELECT payload_json,body_hash,payload_revision FROM node WHERE id=?1",
                [marker],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap()
}
#[tokio::test]
async fn context_trim_large_marker_range_summarizes_short_notice_and_preserves_originals() {
    crate::test_watchdog::watchdog("large context trim marker summary", async {
        let (fixture, mut requests, _release, server) = configured_fixture(1, false).await;
        let (marker, ids) = large_context_trim_marker(&fixture).await;
        let original = saved_context_trim_marker(&fixture, &marker).await;
        assert!(original.0.chars().count() > 80_000);
        let payload: Value = serde_json::from_str(&original.0).unwrap();
        assert_eq!(payload["context_trim"]["reason"], "failure");
        assert_eq!(
            payload["context_trim"]["original_order"]
                .as_array()
                .unwrap()
                .len(),
            1076
        );
        let input = fixture
            .db
            .call(|db| {
                Ok(crate::control::replay::ReplayCache::new(1_000_000)
                    .load(db, "t")?
                    .materialize())
            })
            .await
            .unwrap();
        assert_eq!(input[0], payload["item"]);
        assert!(!serde_json::to_string(&input).unwrap().contains(&ids[537]));
        let result = fixture
            .compactor
            .edit(
                "t",
                &summary_args(&marker, ids.last().unwrap()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.compact.source_nodes, 2);
        assert_eq!(
            result.compact.original_node_ids,
            vec![marker.clone(), ids.last().unwrap().clone()]
        );
        assert_eq!(saved_context_trim_marker(&fixture, &marker).await, original);
        let request = requests.recv().await.unwrap();
        let text = request["input"][1]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Shellboy context notice"));
        assert!(text.chars().count() < 4096);
        assert!(
            !text.contains("original_order")
                && !text.contains("source_node_ids")
                && !text.contains(&ids[537])
        );
        assert_eq!(
            crate::test_watchdog::watchdog("one local summary request", server)
                .await
                .unwrap()
                .unwrap(),
            1
        );
        let original_id = ids[537].clone();
        let body = crate::history::History::new(fixture.db.clone())
            .get(crate::protocol::HistoryGetArgs {
                end_message_only: false,
                parts: false,
                node_id: original_id,
                limit_bytes: 65536,
                cursor: None,
                encoding: "UTF-8".into(),
            })
            .await
            .unwrap();
        assert!(body.text.contains("original synthetic 537"));
        assert!(body.tombstone_reason.is_none());
        eprintln!(
            "context_trim summary fixture saved={} summary_input={}",
            original.0.chars().count(),
            text.chars().count()
        );
        fixture.close().await;
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn context_trim_summary_projection_does_not_change_exact_restore_or_marker_payload() {
    crate::test_watchdog::watchdog("context trim exact restore after render", async {
        let fixture = Fixture::new().await;
        let (marker, ids) = large_context_trim_marker(&fixture).await;
        let original = saved_context_trim_marker(&fixture, &marker).await;
        let snapshot = fixture.compactor.snapshot("t", false).await.unwrap();
        assert!(
            render(&snapshot.groups[0].sources[0], snapshot.epoch)
                .chars()
                .count()
                < 4096
        );
        assert_eq!(saved_context_trim_marker(&fixture, &marker).await, original);
        fixture
            .compactor
            .edit(
                "t",
                &ContextEditArgs {
                    thread_id: Some("t".into()),
                    action: ContextEditAction::Restore,
                    from_node_id: Some(marker.clone()),
                    to_node_id: None,
                    max_chars: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(fixture.path().await, ids);
        assert_eq!(saved_context_trim_marker(&fixture, &marker).await, original);
        fixture.close().await;
    })
    .await
    .unwrap();
}
