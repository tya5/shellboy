use serde_json::{Value, json};
use shellboy::protocol::*;

#[test]
fn context_edit_requires_unambiguous_ranges_and_bounded_ids() {
    for (action, from, to, valid) in [
        (ContextEditAction::Spill, Some("a"), Some("b"), true),
        (ContextEditAction::Summary, Some("a"), Some("b"), true),
        (ContextEditAction::Omit, Some("a"), Some("b"), true),
        (ContextEditAction::Trim, None, Some("b"), true),
        (ContextEditAction::Restore, None, None, true),
        (ContextEditAction::Restore, Some("marker"), None, true),
        (ContextEditAction::Spill, None, Some("b"), false),
        (ContextEditAction::Summary, Some("a"), None, false),
        (ContextEditAction::Omit, None, Some("b"), false),
        (ContextEditAction::Omit, Some("a"), None, false),
        (ContextEditAction::Trim, Some("a"), Some("b"), false),
        (ContextEditAction::Trim, None, None, false),
        (ContextEditAction::Restore, None, Some("b"), false),
    ] {
        let command = Command::ContextEdit(ContextEditArgs {
            thread_id: Some("t".into()),
            action,
            from_node_id: from.map(str::to_owned),
            to_node_id: to.map(str::to_owned),
        });
        assert_eq!(command.validate().is_ok(), valid, "{command:?}");
        assert!(!command.is_read_only());
    }
    for id in [" ".into(), "x".repeat(1025)] {
        assert!(
            Command::ContextEdit(ContextEditArgs {
                thread_id: None,
                action: ContextEditAction::Restore,
                from_node_id: Some(id),
                to_node_id: None,
            })
            .validate()
            .is_err()
        );
    }
}

#[test]
fn all_public_commands_have_typed_roundtrip_fixtures() {
    let fixtures = [
        ("status", json!({})),
        ("model.list", json!({})),
        ("thread.list", json!({})),
        ("thread.get", json!({"thread_id":"t"})),
        ("thread.create", json!({})),
        ("thread.fork", json!({"thread_id":"t","node_id":"n"})),
        ("thread.delete", json!({"thread_id":"t"})),
        (
            "thread.delete_preview",
            json!({"thread_id":"t","descendants":true}),
        ),
        ("thread.set", json!({"thread_id":"t","effort":""})),
        (
            "message.send",
            json!({"thread_id":"t","text":"hello 日本語\n"}),
        ),
        ("message.get", json!({"message_id":"m"})),
        ("operation.get", json!({"operation_id":"old"})),
        ("run.get", json!({"run_id":"r"})),
        ("run.retry", json!({"run_id":"r"})),
        ("run.stop", json!({"run_id":"r"})),
        ("turn.get", json!({"turn_id":"u"})),
        (
            "wait",
            json!({"conditions":[{"type":"run_ended","run_id":"r"}]}),
        ),
        ("history.list", json!({"thread_id":"t"})),
        ("history.get", json!({"node_id":"n"})),
        ("history.search", json!({"query":"query"})),
        (
            "history.relations",
            json!({"node_id":"n","relation":"previous"}),
        ),
        ("shell.get", json!({"execution_id":"x"})),
        ("shell.read", json!({"execution_id":"x"})),
        ("shell.write", json!({"execution_id":"x","text":"stdin"})),
        ("shell.eof", json!({"execution_id":"x"})),
        ("shell.stop", json!({"execution_id":"x"})),
        (
            "attachment.add",
            json!({"thread_id":"t","path":"./file","kind":"file"}),
        ),
        ("attachment.list", json!({"thread_id":"t"})),
        (
            "attachment.remove",
            json!({"thread_id":"t","attachment_id":"a"}),
        ),
        ("compact", json!({"thread_id":"t"})),
        (
            "context.edit",
            json!({"thread_id":"t","action":"spill","from_node_id":"a","to_node_id":"b"}),
        ),
        ("index.status", json!({})),
        ("index.preview", json!({"scope":"all"})),
        ("index.backfill", json!({"scope":"all"})),
        ("index.retry", json!({})),
        ("index.rebuild", json!({"thread_ids":["t"]})),
        ("storage.status", json!({})),
        ("storage.collect", json!({})),
    ];
    assert_eq!(fixtures.len(), 38);
    for (command, args) in fixtures {
        let request = Request::from_value(json!({"protocol_version":1,"operation_id":"instance:nonce","command":command,"args":args})).unwrap_or_else(|e| panic!("{command}: {e}"));
        assert_eq!(request.command.name(), command);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["command"], command);
        let again: Request = serde_json::from_value(value).unwrap();
        assert_eq!(request, again);
        assert!(request.origin.is_none());
    }
}

#[test]
fn rejects_unknown_fields_and_invalid_limits_without_echoing_inputs() {
    for value in [
        json!({"protocol_version":1,"operation_id":"op","command":"thread.list","args":{"limit":201}}),
        json!({"protocol_version":1,"operation_id":"op","command":"history.get","args":{"node_id":"n","limit_bytes":65537}}),
        json!({"protocol_version":1,"operation_id":"op","command":"history.search","args":{"query":"q","limit":51}}),
        json!({"protocol_version":1,"operation_id":"op","command":"thread.list","args":{"secret":"do not echo"}}),
        json!({"protocol_version":1,"operation_id":"op","command":"thread.list","args":{},"unknown":"do not echo"}),
        json!({"protocol_version":1,"operation_id":"op","command":"run.stop","args":{"run_id":""}}),
        json!({"protocol_version":1,"operation_id":"op","command":"wait","args":{"conditions":[]}}),
        json!({"protocol_version":1,"operation_id":"op","command":"index.backfill","args":{}}),
        json!({"protocol_version":1,"operation_id":"op","command":"thread.set","args":{}}),
    ] {
        let error = Request::from_value(value).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(!error.message.contains("do not echo"));
    }
    let bad_version = Request::from_value(
        json!({"protocol_version":2,"operation_id":"op","command":"thread.list","args":{}}),
    )
    .unwrap_err();
    assert_eq!(bad_version.code, ErrorCode::ProtocolMismatch);
    assert!(serde_json::from_value::<Request>(json!({"protocol_version":1,"operation_id":"op","command":"thread.list","args":{},"unknown":true})).is_err());
}

#[test]
fn success_and_error_envelopes_match_contract_and_omit_absent_fields() {
    let message = MessageAccepted {
        message_id: "m".into(),
        thread_id: "t".into(),
        delivery_status: DeliveryStatus::Held,
        assignment: Assignment::Pending,
        run_id: None,
        turn_id: None,
    };
    message.validate().unwrap();
    let response = Response::success("op", message);
    response.validate().unwrap();
    let value = serde_json::to_value(response).unwrap();
    assert!(value.get("error").is_none());
    assert!(value["result"].get("run_id").is_none());
    assert!(value["result"].get("turn_id").is_none());
    assert_eq!(value["result"]["delivery_status"], "held");
    let response: Response<Value> = Response::failure(
        "op",
        ApiError::new(ErrorCode::ThreadNotFound, "missing")
            .hint("list threads")
            .details(json!({"thread_id":"t"})),
    );
    response.validate().unwrap();
    let value = serde_json::to_value(response).unwrap();
    assert!(value.get("result").is_none());
    assert_eq!(value["error"]["code"], "thread_not_found");
    let error = serde_json::to_value(ApiError::new(ErrorCode::SaveFailed, "save failed")).unwrap();
    assert!(error.get("hint").is_none());
    assert!(error.get("details").is_none());
}

#[test]
fn assignment_never_exposes_fabricated_or_partial_ids() {
    for (assignment, run_id, turn_id) in [
        (Assignment::Pending, Some("r".into()), Some("t".into())),
        (Assignment::NewRun, None, None),
        (Assignment::ExistingTurn, Some("r".into()), None),
        (Assignment::NewTurn, Some("".into()), Some("t".into())),
    ] {
        assert!(
            MessageAccepted {
                message_id: "m".into(),
                thread_id: "t".into(),
                delivery_status: DeliveryStatus::Accepted,
                assignment,
                run_id,
                turn_id
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn wait_conditions_and_timeout_are_typed_and_any() {
    let request = Request::from_value(json!({"protocol_version":1,"operation_id":"op","command":"wait","args":{"conditions":[{"type":"turn_completed","turn_id":"t"},{"type":"run_ended","run_id":"r"},{"type":"shell_ended","execution_id":"e"},{"type":"shell_output","execution_id":"e","cursor":"c"}],"timeout_ms":0}})).unwrap();
    match request.command {
        Command::Wait(a) => {
            assert_eq!(a.timeout_ms, 0);
            assert_eq!(a.conditions.len(), 4);
        }
        _ => panic!(),
    }
    assert!(
        serde_json::from_value::<WaitCondition>(
            json!({"type":"message_assigned","message_id":"m"})
        )
        .is_err()
    );
    let args: WaitArgs = serde_json::from_value(json!({"conditions":[]})).unwrap();
    assert_eq!(args.timeout_ms, 30_000);
}

#[test]
fn cursor_binds_query_revision_encoding_and_watermark() {
    let cursor = PageCursor {
        version: 1,
        scope: "history:t".into(),
        filter: "".into(),
        last_sort_key: "42".into(),
        watermark: 100,
        revision: 3,
        encoding: Some("UTF-8".into()),
    };
    let token = cursor.encode().unwrap();
    assert_eq!(
        PageCursor::decode(&token, "history:t", "", 3, Some("UTF-8")).unwrap(),
        cursor
    );
    for (scope, filter, revision) in [
        ("shell:e", "", 3),
        ("history:t", "changed", 3),
        ("history:t", "", 4),
    ] {
        assert_eq!(
            PageCursor::decode(&token, scope, filter, revision, Some("UTF-8"))
                .unwrap_err()
                .code,
            ErrorCode::CursorExpired
        );
    }
    assert_eq!(
        PageCursor::decode(&token, "history:t", "", 3, Some("Shift_JIS"))
            .unwrap_err()
            .code,
        ErrorCode::CursorEncodingMismatch
    );
    assert!(PageCursor::decode("invalid!", "history:t", "", 3, None).is_err());
}

#[test]
fn shell_defaults_and_origin_debug_do_not_leak_token() {
    let args: ShellToolArgs = serde_json::from_value(json!({"command":"printf hi"})).unwrap();
    args.validate().unwrap();
    assert!(!args.background && !args.stdin_open);
    assert_eq!(args.input_encoding, "UTF-8");
    assert_eq!(args.output_encoding, "UTF-8");
    assert!(serde_json::from_value::<ShellToolArgs>(json!({"command":"hi","cwd":"/"})).is_err());
    let origin = Origin {
        instance_id: "i".into(),
        thread_id: "t".into(),
        run_id: "r".into(),
        turn_id: "u".into(),
        execution_id: "e".into(),
        token: "never-log-this".into(),
    };
    assert!(!format!("{origin:?}").contains("never-log-this"));
    let identity = serde_json::to_value(origin.identity()).unwrap();
    assert_eq!(
        identity,
        json!({"instance_id":"i","thread_id":"t","run_id":"r","turn_id":"u","execution_id":"e"})
    );
    assert!(!format!("{:?}", origin.identity()).contains("never-log-this"));
    assert!(
        serde_json::from_value::<Origin>(identity).is_err(),
        "provenance cannot authenticate"
    );
    let wire = serde_json::to_value(&origin).unwrap();
    assert_eq!(
        wire["token"], "never-log-this",
        "IPC must carry the credential"
    );
    assert_eq!(serde_json::from_value::<Origin>(wire).unwrap(), origin);
}

#[tokio::test]
async fn frames_roundtrip_and_reject_truncation_oversize_and_invalid_json() {
    let mut buffer = Vec::new();
    write_frame(&mut buffer, &json!({"text":"日本語"}))
        .await
        .unwrap();
    let mut input = buffer.as_slice();
    let output: Value = read_frame(&mut input).await.unwrap().unwrap();
    assert_eq!(output["text"], "日本語");
    assert!(read_frame::<_, Value>(&mut input).await.unwrap().is_none());
    let mut joined = buffer.clone();
    write_frame(&mut joined, &json!({"next": true}))
        .await
        .unwrap();
    let mut input = joined.as_slice();
    assert_eq!(
        read_frame::<_, Value>(&mut input).await.unwrap().unwrap(),
        output
    );
    assert_eq!(
        read_frame::<_, Value>(&mut input).await.unwrap().unwrap(),
        json!({"next":true})
    );
    assert!(read_frame::<_, Value>(&mut input).await.unwrap().is_none());
    for bytes in [
        &[0_u8][..],
        &buffer[..buffer.len() - 1],
        &[0, 0, 0, 1, b'!'][..],
    ] {
        assert!(read_frame::<_, Value>(&mut &*bytes).await.is_err());
    }
    let oversized = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
    assert_eq!(
        read_frame::<_, Value>(&mut oversized.as_slice())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InputLimit
    );
    assert_eq!(
        write_frame(&mut Vec::new(), &"x".repeat(MAX_FRAME_BYTES))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InputLimit
    );
}

#[test]
fn fixed_error_codes_and_persisted_state_values_are_closed() {
    let codes = [
        "invalid_argument",
        "config_invalid",
        "app_not_running",
        "protocol_mismatch",
        "thread_not_found",
        "target_unavailable",
        "operation_conflict",
        "operation_expired",
        "operation_not_found",
        "invalid_state",
        "wait_cycle",
        "cursor_expired",
        "cursor_encoding_mismatch",
        "output_expired",
        "input_limit",
        "storage_limit",
        "save_failed",
        "io_failed",
    ];
    for code in codes {
        let parsed: ErrorCode = serde_json::from_value(json!(code)).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), code);
    }
    assert!(serde_json::from_value::<ErrorCode>(json!("model_failed")).is_err());
    assert!(serde_json::from_value::<RunState>(json!("recovery_wait")).is_err());
    assert_eq!(
        serde_json::from_value::<TurnState>(json!("failed")).unwrap(),
        TurnState::Failed
    );
    assert_eq!(
        serde_json::to_value(TurnState::RecoveryWait).unwrap(),
        "recovery_wait"
    );
    assert_eq!(
        serde_json::to_value(AttemptState::Incomplete).unwrap(),
        "incomplete"
    );
    assert_eq!(
        serde_json::to_value(ManagedFileState::Deleting).unwrap(),
        "deleting"
    );
}

#[test]
fn connection_profile_changes_are_independent_and_shell_nul_is_preflight_error() {
    for command in ["thread.create", "thread.set"] {
        Request::from_value(json!({"protocol_version":1,"operation_id":"op","command":command,"args":{"connection":"home-proxy"}})).unwrap();
        assert!(Request::from_value(json!({"protocol_version":1,"operation_id":"op","command":command,"args":{"connection":""}})).is_err());
    }
    for field in ["command", "input_encoding", "output_encoding"] {
        let mut value = json!({"command":"printf hi"});
        value[field] = json!("a\u{0000}b");
        assert!(
            serde_json::from_value::<ShellToolArgs>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    serde_json::from_value::<ShellToolArgs>(
        json!({"command":"printf hi\nprintf done","initial_stdin":"a\u{0000}b"}),
    )
    .unwrap()
    .validate()
    .unwrap();
}

#[test]
fn search_filter_validation_rejects_ambiguous_times_and_unbounded_lists() {
    let validate = |args: Value| {
        Request::from_value(
            json!({"protocol_version":1,"operation_id":"op","command":"history.search","args":args}),
        )
    };
    for args in [
        json!({"query":"q","thread_id":""}),
        json!({"query":"q","kinds":[""]}),
        json!({"query":"q","exclude_kinds":["x".repeat(65)]}),
        json!({"query":"q","kinds":vec!["user";33]}),
        json!({"query":"q","from":"2026-10-25T12:00:00"}),
        json!({"query":"q","to":"garbage"}),
        json!({"query":"q","from":"2026-01-01T00:00:00Z","to":"2026-01-01T09:00:00+09:00"}),
    ] {
        assert_eq!(validate(args).unwrap_err().code, ErrorCode::InvalidArgument);
    }
    validate(json!({"query":"q","from":"2026-01-01T00:00:00Z","to":"2026-01-02T00:00:00Z","kinds":["future_kind"]})).unwrap();
}

#[tokio::test]
async fn advertised_frame_limit_does_not_allocate_the_whole_payload_before_receiving() {
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, ReadBuf};
    struct PrefixOnly {
        position: usize,
        requested: Arc<AtomicUsize>,
    }
    impl AsyncRead for PrefixOnly {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let prefix = (MAX_FRAME_BYTES as u32).to_be_bytes();
            if self.position < prefix.len() {
                let count = output.remaining().min(prefix.len() - self.position);
                output.put_slice(&prefix[self.position..self.position + count]);
                self.position += count;
                Poll::Ready(Ok(()))
            } else {
                self.requested.store(output.remaining(), Ordering::SeqCst);
                Poll::Pending
            }
        }
    }
    let requested = Arc::new(AtomicUsize::new(0));
    let mut reader = PrefixOnly {
        position: 0,
        requested: requested.clone(),
    };
    let frame = read_frame::<_, Value>(&mut reader);
    tokio::pin!(frame);
    assert!(futures_util::poll!(&mut frame).is_pending());
    let initial = requested.load(Ordering::SeqCst);
    assert!(
        initial > 0 && initial <= 8192,
        "initial payload read buffer was {initial} bytes"
    );
}

#[test]
fn shared_enum_names_are_exact_serde_wire_values_and_closed_parsers() {
    macro_rules! check {
        ($ty:ty, $names:expr) => {{
            assert_eq!(<$ty>::NAMES, $names);
            assert_eq!(<$ty>::VALUES.len(), <$ty>::NAMES.len());
            for value in <$ty>::VALUES {
                let name = value.as_str();
                assert_eq!(serde_json::to_value(value).unwrap(), name);
                assert_eq!(<$ty>::parse(name), Some(*value));
                assert_eq!(serde_json::from_value::<$ty>(json!(name)).unwrap(), *value);
            }
            assert!(<$ty>::parse("unknown").is_none());
            assert!(serde_json::from_value::<$ty>(json!("unknown")).is_err());
        }};
    }
    check!(AttachmentKind, &["image", "file"]);
    check!(IndexScope, &["threads", "all"]);
    check!(
        StorageCategory,
        &["raw", "managed_images", "history", "index", "all"]
    );
    check!(
        HistoryRelation,
        &["previous", "next", "sources", "replacements"]
    );
    assert!(StorageCategory::parse("managed-images").is_none());
}

#[test]
fn both_request_entry_points_keep_top_level_and_nested_unknown_fields_closed() {
    let valid = json!({"protocol_version":1,"operation_id":"op","command":"thread.list","args":{},"origin":null});
    assert!(Request::from_value(valid.clone()).is_ok());
    assert!(serde_json::from_value::<Request>(valid.clone()).is_ok());
    let mut top = valid.clone();
    top["PRIVATE_FIELD"] = json!("PRIVATE_VALUE");
    let mut nested = valid;
    nested["args"]["extra"] = json!("PRIVATE_VALUE");
    let condition = json!({"protocol_version":1,"operation_id":"op","command":"wait","args":{"conditions":[{"type":"run_ended","run_id":"r","extra":"PRIVATE_VALUE"}]}});
    for value in [top, nested, condition, json!([])] {
        let api = Request::from_value(value.clone()).unwrap_err();
        assert_eq!(api.code, ErrorCode::InvalidArgument);
        assert!(!api.to_string().contains("PRIVATE_"));
        let serde = serde_json::from_value::<Request>(value).unwrap_err();
        assert!(!serde.to_string().contains("PRIVATE_VALUE"));
    }
    assert_eq!(
        Request::from_value(
            json!({"protocol_version":2,"operation_id":"op","command":"thread.list","args":{}})
        )
        .unwrap_err()
        .code,
        ErrorCode::ProtocolMismatch
    );
    assert_eq!(Request::from_value(json!({"protocol_version":2,"operation_id":"op","command":"thread.list","args":{"unknown":1}})).unwrap_err().message, "invalid request shape");
}

#[test]
fn argument_local_validation_keeps_first_error_order_and_distinct_units() {
    for (command, message) in [
        (
            json!({"command":"status","args":{"operation_id":"","limit":0}}),
            "operation_id must not be empty",
        ),
        (
            json!({"command":"thread.set","args":{"connection":"","title":"\n"}}),
            "connection must not be empty",
        ),
        (
            json!({"command":"history.search","args":{"query":"","limit":0,"from":"invalid"}}),
            "query must not be empty",
        ),
        (
            json!({"command":"history.search","args":{"query":"q","limit":0,"thread_id":""}}),
            "limit must be in 1..=50",
        ),
        (
            json!({"command":"history.get","args":{"node_id":"","encoding":"","limit_bytes":0}}),
            "node_id must not be empty",
        ),
        (
            json!({"command":"history.list","args":{"thread_id":"","limit":0,"cursor":""}}),
            "thread_id must not be empty",
        ),
        (
            json!({"command":"wait","args":{"conditions":[{"type":"shell_output","execution_id":"","cursor":""}]}}),
            "execution_id must not be empty",
        ),
    ] {
        let command: Command = serde_json::from_value(command).unwrap();
        let error = command.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.message, message);
    }
    let search = Command::HistorySearch(HistorySearchArgs {
        query: "q".into(),
        kinds: vec!["界".repeat(64)],
        ..Default::default()
    });
    search.validate().unwrap();
    let status = Command::Status(StatusArgs {
        state: vec!["界".repeat(64)],
        ..Default::default()
    });
    assert_eq!(
        status.validate().unwrap_err().message,
        "state accepts at most 32 nonempty values of at most 64 bytes"
    );
    Command::Status(StatusArgs {
        state: vec!["x".repeat(64)],
        ..Default::default()
    })
    .validate()
    .unwrap();
    assert!(
        Command::HistorySearch(HistorySearchArgs {
            query: "q".into(),
            kinds: vec!["界".repeat(65)],
            ..Default::default()
        })
        .validate()
        .is_err()
    );
}

#[test]
fn history_filter_preflight_rejects_invalid_time_without_changing_first_error_precedence() {
    for since in ["yesterday", "2026-10-25", "2026-10-25T07:00:00"] {
        let command: Command = serde_json::from_value(
            json!({"command":"history.list","args":{"thread_id":"t","since":since}}),
        )
        .unwrap();
        let error = command.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.message, "since must be RFC3339 with timezone");
    }
    for (args, message) in [
        (
            json!({"thread_id":"","since":"bad","state":"bad"}),
            "thread_id must not be empty",
        ),
        (
            json!({"thread_id":"t","limit":0,"since":"bad","state":"bad"}),
            "limit must be in 1..=200",
        ),
        (
            json!({"thread_id":"t","cursor":"","since":"bad","state":"bad"}),
            "cursor must not be empty",
        ),
        (
            json!({"thread_id":"t","since":"","state":"bad"}),
            "since must not be empty",
        ),
        (
            json!({"thread_id":"t","since":"bad","state":""}),
            "state must not be empty",
        ),
        (
            json!({"thread_id":"t","since":"bad","state":"bad"}),
            "status must be a saved shell execution state: starting, running, stopping, exited, failed, stopped",
        ),
    ] {
        let command: Command =
            serde_json::from_value(json!({"command":"history.list","args":args})).unwrap();
        assert_eq!(command.validate().unwrap_err().message, message);
    }
    let valid: Command = serde_json::from_value(json!({"command":"history.list","args":{"thread_id":"t","since":"2026-10-25T16:00:00+09:00","state":"running"}})).unwrap();
    valid.validate().unwrap();
}

#[test]
fn optional_operation_defaults_keep_empty_wire_and_explicit_index_target_semantics() {
    use shellboy::protocol::{IndexArgs, ThreadSetArgs};
    assert_eq!(
        serde_json::to_value(ThreadSetArgs::default()).unwrap(),
        json!({"connection":null,"thread_id":null,"cwd":null,"model":null,"effort":null})
    );
    assert_eq!(
        serde_json::to_value(IndexArgs::default()).unwrap(),
        json!({"scope":null,"space_id":null,"thread_ids":[],"source_ids":[]})
    );
    assert!(
        Command::IndexStatus(IndexArgs::default())
            .validate()
            .is_ok()
    );
    assert!(
        Command::IndexRebuild(IndexArgs::default())
            .validate()
            .is_err()
    );
    assert!(
        Command::IndexPreview(IndexArgs::default())
            .validate()
            .is_err()
    );
    let preview = Command::IndexPreview(IndexArgs {
        scope: Some(IndexScope::All),
        ..Default::default()
    });
    assert!(preview.validate().is_ok());
    assert!(preview.is_read_only());
}
