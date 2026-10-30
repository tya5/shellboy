use shellboy::{
    config::Logging,
    diagnostics::{DiagnosticEvent, DiagnosticIds, Diagnostics},
};

#[tokio::test]
async fn diagnostic_rotation_is_bounded_and_records_only_structured_context() {
    let root = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::start(
        root.path().join("logs"),
        Logging {
            file_bytes: 512,
            files: 3,
        },
    );
    for i in 0..100 {
        diagnostics.record(
            DiagnosticEvent::ModelFailure,
            DiagnosticIds {
                thread_id: Some(format!("thread-{i}")),
                ..Default::default()
            },
        );
    }
    diagnostics.shutdown().await;
    assert!(!diagnostics.has_failed());
    let mut files = 0;
    for entry in std::fs::read_dir(root.path().join("logs")).unwrap() {
        let entry = entry.unwrap();
        files += 1;
        assert!(entry.metadata().unwrap().len() <= 512);
        for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["event"], "model_failure");
            assert!(value.get("message").is_none());
            assert!(value.get("body").is_none());
        }
    }
    assert!(files <= 3);
}

#[tokio::test]
async fn log_save_failure_and_saturated_queue_do_not_block_application_admission() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("not-a-directory");
    std::fs::write(&path, "keep").unwrap();
    let diagnostics = Diagnostics::start(
        path,
        Logging {
            file_bytes: 1024,
            files: 1,
        },
    );
    for _ in 0..1000 {
        diagnostics.record(DiagnosticEvent::Startup, DiagnosticIds::default());
    }
    diagnostics.shutdown().await;
    assert!(diagnostics.has_failed());
    assert!(diagnostics.pending_dropped() > 0);
    assert_eq!(
        std::fs::read_to_string(root.path().join("not-a-directory")).unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn stream_differences_log_only_typed_fields_and_attempt_without_failure_classification() {
    use shellboy::model::{OutputMismatch, OutputMismatchField};
    let root = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::start(
        root.path().into(),
        Logging {
            file_bytes: 4096,
            files: 1,
        },
    );
    for (index, field) in [
        OutputMismatchField::Text,
        OutputMismatchField::Arguments,
        OutputMismatchField::Metadata,
    ]
    .into_iter()
    .enumerate()
    {
        diagnostics.record_model_stream_difference(
            DiagnosticIds {
                thread_id: Some("thread-1".into()),
                run_id: Some("run-1".into()),
                turn_id: Some("turn-1".into()),
                ..Default::default()
            },
            &format!("attempt-\n{index}"),
            &OutputMismatch {
                output_index: index as u64,
                field,
            },
        );
    }
    diagnostics.shutdown().await;
    assert!(!diagnostics.has_failed());
    let log = std::fs::read_to_string(root.path().join("shellboy.log")).unwrap();
    assert!(!log.contains("PRIVATE_"));
    let rows = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row["event"], "model_stream_difference");
        assert!(row.get("failure").is_none());
        assert_eq!(row["ids"]["thread_id"], "thread-1");
        assert_eq!(row["ids"]["run_id"], "run-1");
        assert_eq!(row["ids"]["turn_id"], "turn-1");
        assert_eq!(
            row["stream_difference"],
            serde_json::json!({"reason":"stream_difference", "attempt_id":format!("attempt-{index}"),"output_index":index,"field":(["text","arguments","metadata"][index])})
        );
    }
}

#[tokio::test]
async fn unknown_model_categories_and_provider_text_never_become_diagnostic_values() {
    use shellboy::model::ModelError;
    let root = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::start(
        root.path().into(),
        Logging {
            file_bytes: 4096,
            files: 1,
        },
    );
    for kind in ["transport", "provider", "PRIVATE_UNKNOWN_KIND"] {
        let mut error = ModelError::new(kind, "PRIVATE_MESSAGE".repeat(10000));
        error.provider_code = Some("PRIVATE_PROVIDER_CODE".into());
        diagnostics.record_model_failure(DiagnosticIds::default(), &"a".repeat(512), &error);
    }
    diagnostics.record(DiagnosticEvent::ModelFailure, DiagnosticIds::default());
    diagnostics.shutdown().await;
    assert!(!diagnostics.has_failed());
    let log = std::fs::read_to_string(root.path().join("shellboy.log")).unwrap();
    assert!(!log.contains("PRIVATE_"));
    let rows = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 4);
    for (row, reason) in rows
        .iter()
        .take(3)
        .zip(["transport", "provider", "unknown"])
    {
        assert_eq!(
            row["failure"],
            serde_json::json!({"reason":reason,"attempt_id":"a".repeat(128)})
        );
    }
    assert_eq!(rows[3]["failure"], serde_json::json!({"reason":"recovery"}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_final_difference_is_logged_as_an_observation_and_both_sources_are_preserved() {
    streamed_fixture(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_final_still_recovers_and_logs_failure_with_its_saved_attempt() {
    streamed_fixture(true).await;
}

async fn streamed_fixture(malformed_final: bool) {
    use serde_json::{Value, json};
    use shellboy::{app::DataOwner, control::Control, protocol::*};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{Duration, timeout},
    };
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{}/v1'\n", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let split = loop {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break at + 4;
            }
            assert!(bytes.len() < 65536);
        };
        let length: usize = std::str::from_utf8(&bytes[..split])
            .unwrap()
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|n| n.trim().parse().unwrap())
            })
            .unwrap();
        while bytes.len() < split + length {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        let done = json!({"id":"same-item","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"PRIVATE_ITEM_DONE_BODY","annotations":[]}]});
        let mut final_item = done.clone();
        final_item["content"][0]["text"] = if malformed_final {
            json!(42)
        } else {
            json!("PRIVATE_FINAL_BODY")
        };
        let events = [
            json!({"type":"response.output_text.delta","delta":"PRIVATE_STREAM_DELTA"}),
            json!({"type":"response.reasoning_summary_text.delta","delta":"PRIVATE_REASONING_DELTA"}),
            json!({"type":"response.output_item.done","output_index":0,"item":done}),
            json!({"type":"response.completed","response":{"id":"response-fixture","status":"completed","output":[final_item]}}),
        ];
        let body = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
    });
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let create = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        ))
        .await;
    assert!(create.ok, "{:?}", create.error);
    let thread = create.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sent = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                now: false,
                thread_id: Some(thread.clone()),
                text: "PRIVATE_HUMAN_MESSAGE".into(),
            }),
        ))
        .await;
    assert!(sent.ok, "{:?}", sent.error);
    let accepted: MessageAccepted = serde_json::from_value(sent.result.unwrap()).unwrap();
    let turn = accepted.turn_id.unwrap();
    let check_turn = turn.clone();
    let attempt = timeout(Duration::from_secs(10), async {
        loop {
            let target = check_turn.clone();
            let attempt = control.store.database.call(move |db| {
                use rusqlite::OptionalExtension;
                let state = if malformed_final { "recovery_wait" } else { "completed" };
                db.query_row("SELECT a.id FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.id=?1 AND t.state=?2", [target,state.into()], |row| row.get::<_,String>(0)).optional().map_err(shellboy::storage::StorageError::from)
            }).await.unwrap();
            if let Some(attempt) = attempt { break attempt; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let audit_turn = turn.clone();
    let audit_attempt = attempt.clone();
    control.store.database.call(move |db| {
        let kind = if malformed_final { "incomplete_response" } else { "model_stream_observation" };
        let payload: String = db.query_row("SELECT payload_json FROM node WHERE turn_id=?1 AND kind=?2", [&audit_turn, kind], |row| row.get(0))?;
        let audit: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(audit["attempt_id"], audit_attempt);
        assert_eq!(audit["partial_text"], "PRIVATE_STREAM_DELTA");
        assert_eq!(audit["partial_reasoning"], "PRIVATE_REASONING_DELTA");
        assert_eq!(audit["completed_items"]["0"]["content"][0]["text"], "PRIVATE_ITEM_DONE_BODY");
        if malformed_final {
            assert_eq!(audit["raw_response"]["output"][0]["content"][0]["text"], 42);
            let saved_error: String = db.query_row("SELECT error_json FROM model_attempt WHERE id=?1", [&audit_attempt], |row| row.get(0))?;
            assert_eq!(serde_json::from_str::<Value>(&saved_error).unwrap(), audit, "attempt and original node must share the same witness-bearing body");
        } else {
            assert_eq!(audit["classification"], "stream_difference");
            assert_eq!(audit["differences"], json!([{"output_index":0,"field":"text"}]));
            assert_eq!(audit["delta_differences"], json!(["text", "reasoning_content"]));
            assert_eq!(audit["total_differences"], 3);
            assert_eq!(audit["raw_response"]["output"][0]["content"][0]["text"], "PRIVATE_FINAL_BODY");
            let attempt_state: String = db.query_row("SELECT state FROM model_attempt WHERE id=?1", [&audit_attempt], |row| row.get(0))?;
            assert_eq!(attempt_state, "completed");
            let selected_observations: i64 = db.query_row("SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE n.turn_id=?1 AND n.kind='model_stream_observation'", [&audit_turn], |row| row.get(0))?;
            assert_eq!(selected_observations, 0, "audit observations are not replay input");
            let answer: String = db.query_row("SELECT payload_json FROM node WHERE turn_id=?1 AND kind='assistant_response'", [&audit_turn], |row| row.get(0))?;
            assert!(answer.contains("PRIVATE_FINAL_BODY"));
            assert!(!answer.contains("PRIVATE_ITEM_DONE_BODY"));
        }
        Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
    assert!(!log.contains("PRIVATE_"));
    let rows = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let failures = rows
        .iter()
        .filter(|row| row["event"] == "model_failure")
        .collect::<Vec<_>>();
    let observations = rows
        .iter()
        .filter(|row| row["event"] == "model_stream_difference")
        .collect::<Vec<_>>();
    if malformed_final {
        assert!(observations.is_empty());
        assert_eq!(
            failures.len(),
            1,
            "generic recovery must not duplicate the typed failure"
        );
        assert_eq!(failures[0]["ids"]["thread_id"], thread);
        assert_eq!(failures[0]["ids"]["turn_id"], turn);
        assert_eq!(
            failures[0]["failure"],
            json!({"reason":"protocol","attempt_id":attempt})
        );
    } else {
        assert!(
            failures.is_empty(),
            "valid final differences are not model failures"
        );
        assert_eq!(observations.len(), 3);
        assert_eq!(observations[0]["ids"]["thread_id"], thread);
        assert_eq!(observations[0]["ids"]["turn_id"], turn);
        assert!(observations[0].get("failure").is_none());
        assert_eq!(
            observations[0]["stream_difference"],
            json!({"reason":"stream_difference","attempt_id":attempt,"output_index":0,"field":"text"})
        );
        for (row, field) in observations[1..].iter().zip(["text", "reasoning_content"]) {
            assert!(row.get("failure").is_none());
            assert_eq!(
                row["stream_difference"],
                json!({"reason":"stream_delta_difference","attempt_id":attempt,"field":field})
            );
        }
    }
}

#[tokio::test]
async fn message_send_trace_links_milestones_without_logging_message_content() {
    use shellboy::diagnostics::{MessageSendStage, MessageSendTrace};
    let root = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::start(
        root.path().into(),
        Logging {
            file_bytes: 8192,
            files: 1,
        },
    );
    for stage in [
        MessageSendStage::Queued,
        MessageSendStage::Processing,
        MessageSendStage::Saved,
    ] {
        diagnostics.record_message_send(
            DiagnosticIds {
                thread_id: Some("thread-1".into()),
                ..Default::default()
            },
            MessageSendTrace {
                stage,
                draft_id: "draft-1".into(),
                revision: 42,
                operation_id: "instance:operation-1".into(),
                message_id: matches!(stage, MessageSendStage::Saved).then(|| "message-1".into()),
            },
        );
    }
    diagnostics.shutdown().await;
    assert!(!diagnostics.has_failed());
    let content = std::fs::read_to_string(root.path().join("shellboy.log")).unwrap();
    let records: Vec<serde_json::Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 3);
    for (record, stage) in records.iter().zip(["queued", "processing", "saved"]) {
        assert_eq!(record["event"], "message_send");
        assert_eq!(record["ids"]["thread_id"], "thread-1");
        assert_eq!(record["send"]["stage"], stage);
        assert_eq!(record["send"]["draft_id"], "draft-1");
        assert_eq!(record["send"]["revision"], 42);
        assert_eq!(record["send"]["operation_id"], "instance:operation-1");
        assert!(record.get("failure").is_none());
        assert!(record.get("body").is_none());
        assert!(record.get("text").is_none());
        assert!(record["at"].as_str().is_some());
    }
    assert!(records[0]["send"].get("message_id").is_none());
    assert_eq!(records[2]["send"]["message_id"], "message-1");
}

#[tokio::test]
async fn rejected_send_trace_is_bounded_and_has_a_fixed_stage() {
    use shellboy::diagnostics::{MessageSendStage, MessageSendTrace};
    let root = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::start(
        root.path().into(),
        Logging {
            file_bytes: 4096,
            files: 1,
        },
    );
    for stage in [MessageSendStage::QueueFull, MessageSendStage::Failed] {
        diagnostics.record_message_send(
            DiagnosticIds::default(),
            MessageSendTrace {
                stage,
                draft_id: format!("draft\n{}", "x".repeat(300)),
                revision: u64::MAX,
                operation_id: "instance:\noperation".into(),
                message_id: Some("message\t1".into()),
            },
        );
    }
    diagnostics.shutdown().await;
    let content = std::fs::read_to_string(root.path().join("shellboy.log")).unwrap();
    let records: Vec<serde_json::Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    for (record, stage) in records.iter().zip(["queue_full", "failed"]) {
        assert_eq!(record["send"]["stage"], stage);
        assert_eq!(record["send"]["draft_id"].as_str().unwrap().len(), 128);
        assert_eq!(record["send"]["operation_id"], "instance:operation");
        assert_eq!(record["send"]["message_id"], "message1");
    }
}
