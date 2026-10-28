//! Hidden-source summaries reuse the regular bounded summary executor.
use super::tests::Fixture;
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Duration, timeout},
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
        stream_idle_timeout_ms: 2_000,
        ..Default::default()
    };
    let (send, received) = mpsc::channel(16);
    let (release, hold) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut hold = Some(hold);
        for part in 0..parts {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
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
async fn omitted_middle_summary_uses_original_data_and_retains_prefix_suffix_and_raw_provenance() {
    let (fixture, mut requests, _release, server) = configured_fixture(1, false).await;
    let marker = omit_middle(&fixture).await;
    let snapshot_before = fixture.compactor.snapshot("t", false).await.unwrap();
    let expected_invalidated = estimated_suffix_tokens(&snapshot_before, 1);
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
        result.cache_invalidated_tokens, expected_invalidated,
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
    assert_eq!(raw_parts(&fixture).await, 1);
    assert_eq!(
        timeout(Duration::from_secs(5), server)
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
async fn hidden_summary_late_marker_revision_or_selection_change_rejects_adoption_but_keeps_raw_part()
 {
    for selection_change in [false, true] {
        let (fixture, mut requests, release, server) = configured_fixture(1, true).await;
        let marker = omit_middle(&fixture).await;
        let before = fixture.path().await;
        let compactor = fixture.compactor.clone();
        let args = summary_args("middle-a", "middle-b");
        let request =
            tokio::spawn(async move { compactor.edit("t", &args, CancellationToken::new()).await });
        timeout(Duration::from_secs(5), requests.recv())
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
        let error = timeout(Duration::from_secs(5), request)
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
        assert_eq!(raw_parts(&fixture).await, 1);
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
            timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap(),
            1
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn explicit_tool_span_summary_reuses_whole_group_sixty_four_thousand_character_partitioning()
{
    let (fixture, mut requests, _release, server) = configured_fixture(2, false).await;
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
    assert_eq!(result.compact.created_parts, 2);
    assert_eq!(result.compact.source_nodes, 4);
    let summary = result.compact.summary_node_id.unwrap();
    assert_eq!(
        fixture.path().await,
        vec!["before".to_owned(), summary, "after".into()]
    );
    for group in ["g-a", "g-b"] {
        let request = requests.recv().await.unwrap();
        let text = request["input"][1]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains(&format!("{group}-call")) && text.contains(&format!("{group}-result"))
        );
        assert!(text.chars().count() < SEGMENT_CHARS);
    }
    assert_eq!(raw_parts(&fixture).await, 2);
    assert_eq!(
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap(),
        2
    );
    fixture.close().await;
}

#[tokio::test]
async fn default_disconnect_trim_does_not_modify_a_small_context() {
    let fixture = Fixture::new().await;
    for id in ["old", "request", "latest"] {
        fixture.text(id, id).await;
    }
    let before = fixture.path().await;
    let result = fixture
        .compactor
        .trim_disconnect("t", None, CancellationToken::new())
        .await
        .unwrap();
    assert!(result.compact.no_target);
    assert_eq!(result.before_tokens, result.after_tokens);
    assert_eq!(fixture.path().await, before);
    assert_eq!(raw_parts(&fixture).await, 0);
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
