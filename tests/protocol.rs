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
            max_chars: None,
            max_percent: None,
            segment_chars: None,
            reduce_rounds: None,
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
                max_chars: None,
                max_percent: None,
                segment_chars: None,
                reduce_rounds: None,
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
        ("index.probe", json!({"dimensions":768})),
        ("index.shrink", json!({"dry_run":true})),
        ("index.remove", json!({"space_id":"s","dry_run":true})),
        ("storage.status", json!({})),
        ("storage.collect", json!({})),
    ];
    assert_eq!(fixtures.len(), 41);
    for (command, args) in fixtures {
        let request = Request::from_value(json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"instance:nonce","command":command,"args":args})).unwrap_or_else(|e| panic!("{command}: {e}"));
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
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"thread.list","args":{"limit":201}}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"history.get","args":{"node_id":"n","limit_bytes":65537}}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"history.search","args":{"query":"q","limit":51}}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"thread.list","args":{"secret":"do not echo"}}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"thread.list","args":{},"unknown":"do not echo"}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"run.stop","args":{"run_id":""}}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"wait","args":{"conditions":[]}}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"index.backfill","args":{}}),
        json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"thread.set","args":{}}),
    ] {
        let error = Request::from_value(value).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(!error.message.contains("do not echo"));
    }
    let bad_version = Request::from_value(
        json!({"protocol_version":0,"operation_id":"op","command":"thread.list","args":{}}),
    )
    .unwrap_err();
    assert_eq!(bad_version.code, ErrorCode::ProtocolMismatch);
    assert!(serde_json::from_value::<Request>(json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"thread.list","args":{},"unknown":true})).is_err());
}

#[test]
fn success_and_error_envelopes_match_contract_and_omit_absent_fields() {
    let message = MessageAccepted {
        to_human: false,
        expect_reply: false,
        deliver: Deliver::Next,
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
                to_human: false,
                expect_reply: false,
                deliver: Deliver::Next,
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
    let request = Request::from_value(json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"wait","args":{"conditions":[{"type":"turn_completed","turn_id":"t"},{"type":"run_ended","run_id":"r"},{"type":"shell_ended","execution_id":"e"},{"type":"shell_output","execution_id":"e","cursor":"c"}],"timeout_ms":0}})).unwrap();
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
        "self_wait",
        "config_invalid",
        "app_not_running",
        "protocol_mismatch",
        "thread_not_found",
        "target_unavailable",
        "operation_conflict",
        "operation_expired",
        "operation_not_found",
        "invalid_state",
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
        Request::from_value(json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":command,"args":{"connection":"home-proxy"}})).unwrap();
        assert!(Request::from_value(json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":command,"args":{"connection":""}})).is_err());
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
            json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"history.search","args":args}),
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
    let valid = json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"thread.list","args":{},"origin":null});
    assert!(Request::from_value(valid.clone()).is_ok());
    assert!(serde_json::from_value::<Request>(valid.clone()).is_ok());
    let mut top = valid.clone();
    top["PRIVATE_FIELD"] = json!("PRIVATE_VALUE");
    let mut nested = valid;
    nested["args"]["extra"] = json!("PRIVATE_VALUE");
    let condition = json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"op","command":"wait","args":{"conditions":[{"type":"run_ended","run_id":"r","extra":"PRIVATE_VALUE"}]}});
    for value in [top, nested, condition, json!([])] {
        let api = Request::from_value(value.clone()).unwrap_err();
        assert_eq!(api.code, ErrorCode::InvalidArgument);
        assert!(!api.to_string().contains("PRIVATE_"));
        let serde = serde_json::from_value::<Request>(value).unwrap_err();
        assert!(!serde.to_string().contains("PRIVATE_VALUE"));
    }
    assert_eq!(
        Request::from_value(
            json!({"protocol_version":0,"operation_id":"op","command":"thread.list","args":{}})
        )
        .unwrap_err()
        .code,
        ErrorCode::ProtocolMismatch
    );
    assert_eq!(Request::from_value(json!({"protocol_version":0,"operation_id":"op","command":"thread.list","args":{"unknown":1}})).unwrap_err().message, "invalid request shape");
}

#[test]
fn argument_local_validation_keeps_first_error_order_and_distinct_units() {
    for (command, message) in [
        (
            json!({"command":"status","args":{"limit":0}}),
            "limit must be in 1..=200",
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
            .is_ok()
    );
    let preview = Command::IndexPreview(IndexArgs {
        dimensions: None,
        scope: Some(IndexScope::All),
        ..Default::default()
    });
    assert!(preview.validate().is_ok());
    assert!(preview.is_read_only());
}

#[test]
fn history_excerpt_legacy_default_and_new_bounds_are_validated() {
    for (value, expected) in [(None, 512), (Some(0), 0), (Some(4096), 4096)] {
        let mut args = json!({"thread_id":"t"});
        if let Some(value) = value {
            args["excerpt_chars"] = json!(value);
        }
        let command: Command =
            serde_json::from_value(json!({"command":"history.list","args":args})).unwrap();
        command.validate().unwrap();
        let Command::HistoryList(value) = command else {
            panic!("history list")
        };
        assert_eq!(value.excerpt_chars, expected);
    }
    let command: Command = serde_json::from_value(
        json!({"command":"history.list","args":{"thread_id":"t","excerpt_chars":4097}}),
    )
    .unwrap();
    assert!(command.validate().is_err());
}

#[test]
fn old_status_operation_wire_argument_is_rejected() {
    let value = json!({"protocol_version": PROTOCOL_VERSION, "operation_id": "request:lookup", "command": "status", "args": {"operation_id": "target:receipt"}});
    assert_eq!(
        Request::from_value(value).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn history_body_requires_provenance_from_current_owner() {
    let mut body = json!({"node_id":"n","kind":"user","text":"synthetic","context":{"state":"present","node_id":"n","created_at":"2026-10-25T00:00:00Z"},"source_node_ids":[],"previous_node_ids":[],"next_node_ids":[]});
    assert!(serde_json::from_value::<HistoryBody>(body.clone()).is_err());
    body["provenance"] =
        json!({"kind":"user","created_at":"2026-10-25T00:00:00Z","source_revision":0});
    assert!(serde_json::from_value::<HistoryBody>(body.clone()).is_ok());
    body.as_object_mut().unwrap().remove("context");
    assert!(serde_json::from_value::<HistoryBody>(body).is_err());
}

#[test]
fn current_lifecycle_details_require_observed_context() {
    for (kind, mut value) in [
        (
            "message",
            json!({"message_id":"m","thread_id":"t","delivery_status":"accepted","assignment":"existing_turn","sender":{"kind":"unknown"}}),
        ),
        (
            "run",
            json!({"run_id":"r","thread_id":"t","state":"live","owned_execution_ids":[]}),
        ),
        (
            "turn",
            json!({"turn_id":"u","run_id":"r","thread_id":"t","state":"running"}),
        ),
    ] {
        let missing = match kind {
            "message" => serde_json::from_value::<MessageDetails>(value.clone()).is_err(),
            "run" => serde_json::from_value::<RunDetails>(value.clone()).is_err(),
            _ => serde_json::from_value::<TurnDetails>(value.clone()).is_err(),
        };
        assert!(missing, "{kind} must carry its current observation context");
        let state = match kind {
            "message" => "accepted",
            "run" => "live",
            _ => "running",
        };
        value["context"] =
            json!({"state":state,"thread_id":"t","created_at":"2026-10-25T00:00:00Z"});
        match kind {
            "message" => {
                serde_json::from_value::<MessageDetails>(value.clone()).unwrap();
                value.as_object_mut().unwrap().remove("sender");
                assert!(
                    serde_json::from_value::<MessageDetails>(value).is_err(),
                    "sender facts are required, never inferred from absent fields"
                );
            }
            "run" => {
                serde_json::from_value::<RunDetails>(value).unwrap();
            }
            _ => {
                serde_json::from_value::<TurnDetails>(value).unwrap();
            }
        }
    }
}

#[test]
fn current_output_and_history_results_require_observation_fields() {
    let output = json!({"execution_id":"x","text":"synthetic","encoding":"UTF-8","cursor":"c","raw_bytes":9,"output_complete":false,"decode_errors":false});
    serde_json::from_value::<ShellOutput>(output.clone()).unwrap();
    let mut missing_decode = output;
    missing_decode
        .as_object_mut()
        .unwrap()
        .remove("decode_errors");
    assert!(serde_json::from_value::<ShellOutput>(missing_decode).is_err());
    let snippet = json!({"node_id":"n","kind":"user","excerpt":"[removed]","payload_chars":null,
        "provenance":{"kind":"user","created_at":"2026-10-25T00:00:00Z","source_revision":1},
        "context":{"state":"tombstone","node_id":"n","created_at":"2026-10-25T00:00:00Z","tombstone_reason":"removed"},
        "selection":{"thread_id":"t","ordinal":0,"current_cwd":"/synthetic"},
        "execution_count":0,"executions_truncated":false});
    let parsed = serde_json::from_value::<HistorySnippet>(snippet.clone()).unwrap();
    assert!(
        parsed.payload_chars.is_none(),
        "tombstone length is unknown"
    );
    assert!(
        parsed.executions.is_empty(),
        "current wire omits empty execution lists"
    );
    for field in [
        "provenance",
        "context",
        "selection",
        "execution_count",
        "executions_truncated",
    ] {
        let mut missing = snippet.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<HistorySnippet>(missing).is_err(),
            "{field}"
        );
    }
}

#[test]
fn message_resume_recovery_is_optional_and_round_trips() {
    let command = Command::MessageSend(MessageSendArgs {
        to_human: false,
        human: false,
        resume_recovery: true,
        expect_reply: false,
        sender_label: None,
        thread_id: None,
        text: "x".into(),
        deliver: Deliver::Next,
    });
    let value = serde_json::to_value(&command).unwrap();
    assert_eq!(value["args"]["resume_recovery"], true);
    assert_eq!(serde_json::from_value::<Command>(value).unwrap(), command);
    let old = serde_json::json!({"command":"message.send","args":{"text":"x"}});
    let decoded: Command = serde_json::from_value(old).unwrap();
    assert!(matches!(decoded, Command::MessageSend(args) if !args.resume_recovery));
}

#[test]
fn message_delivery_timing_is_one_optional_wire_field() {
    let command = |deliver| {
        Command::MessageSend(MessageSendArgs {
            resume_recovery: false,
            human: false,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            thread_id: None,
            text: "x".into(),
            deliver,
        })
    };
    let value = serde_json::to_value(command(Deliver::Next)).unwrap();
    assert!(value["args"].get("deliver").is_none(), "{value}");
    for (deliver, wire) in [(Deliver::Now, "now"), (Deliver::Complete, "complete")] {
        let value = serde_json::to_value(command(deliver)).unwrap();
        assert_eq!(value["args"]["deliver"], wire, "{value}");
        assert_eq!(
            serde_json::from_value::<Command>(value).unwrap(),
            command(deliver)
        );
    }
    let mut legacy = serde_json::to_value(command(Deliver::Next)).unwrap();
    legacy["args"]["now"] = serde_json::json!(true);
    assert!(
        serde_json::from_value::<Command>(legacy).is_err(),
        "--now has no alias"
    );
    for removed in ["revisit.set", "revisit.get", "revisit.cancel"] {
        assert!(
            serde_json::from_value::<Command>(serde_json::json!({"command":removed,"args":{}}))
                .is_err()
        );
    }
}

#[test]
fn index_dimensions_are_explicit_bounded_and_rebuild_only() {
    assert!(
        Command::IndexPreview(IndexArgs::default())
            .validate()
            .is_ok()
    );
    for n in [0, 8193] {
        assert!(
            Command::IndexProbe(IndexProbeArgs {
                dimensions: Some(n)
            })
            .validate()
            .is_err()
        );
    }
    for n in [1, 8192] {
        assert!(
            Command::IndexProbe(IndexProbeArgs {
                dimensions: Some(n)
            })
            .validate()
            .is_ok()
        );
        let args = IndexArgs {
            scope: Some(IndexScope::All),
            dimensions: Some(n),
            ..Default::default()
        };
        assert!(Command::IndexRebuild(args.clone()).validate().is_ok());
        assert!(Command::IndexBackfill(args.clone()).validate().is_err());
        assert!(Command::IndexRetry(args.clone()).validate().is_err());
        assert!(Command::IndexStatus(args.clone()).validate().is_err());
        assert!(Command::IndexPreview(args).validate().is_err());
    }
}

#[test]
fn summary_limits_are_checked_at_the_owner_boundary() {
    let mut a = ContextEditArgs {
        thread_id: None,
        action: ContextEditAction::Summary,
        from_node_id: Some("a".into()),
        to_node_id: Some("b".into()),
        max_chars: Some(1),
        max_percent: None,
        segment_chars: None,
        reduce_rounds: None,
    };
    assert!(a.validate().is_ok());
    a.max_chars = Some(0);
    assert!(a.validate().is_err());
    a.max_chars = None;
    for p in [0.0, -1.0, 100.1, f64::NAN, f64::INFINITY] {
        a.max_percent = Some(p);
        assert!(a.validate().is_err());
    }
    a.max_percent = Some(5.0);
    a.action = ContextEditAction::Omit;
    assert!(a.validate().is_err());
}

#[test]
fn index_status_additive_policy_defaults_do_not_change_config_defaults() {
    let base = json!({
        "pending":0,"running":0,"done":0,"failed":0,"paused":false,
        "paused_at":null,"pause_id":null,"spaces":[],"spaces_total":0,
        "spaces_truncated":false,"probes":[],"auto_kinds":[]
    });
    let absent: IndexStatus = serde_json::from_value(base.clone()).unwrap();
    assert!(!absent.auto_history);
    assert_eq!(absent.held_auto_history, 0);
    assert!(shellboy::config::Config::default().indexing.auto_history);
    for enabled in [false, true] {
        let mut value = base.clone();
        value["auto_history"] = json!(enabled);
        value["held_auto_history"] = json!(3);
        let parsed: IndexStatus = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.auto_history, enabled);
        assert_eq!(parsed.held_auto_history, 3);
        let wire = serde_json::to_value(&parsed).unwrap();
        assert_eq!(wire["auto_history"], enabled);
        assert_eq!(wire["held_auto_history"], 3);
    }
    for (key, invalid) in [
        ("auto_history", json!("off")),
        ("auto_history", Value::Null),
        ("held_auto_history", json!(-1)),
    ] {
        let mut value = base.clone();
        value[key] = invalid;
        assert!(serde_json::from_value::<IndexStatus>(value).is_err());
    }
    let mut missing_pending = base;
    missing_pending.as_object_mut().unwrap().remove("pending");
    assert!(serde_json::from_value::<IndexStatus>(missing_pending).is_err());
}

#[test]
fn attempt_usage_wire_validation_rejects_aggregate_options_and_blank_ids() {
    use shellboy::protocol::{Command, UsageGetArgs};
    assert!(
        Command::UsageGet(UsageGetArgs {
            attempt_id: Some("a".into()),
            ..Default::default()
        })
        .validate()
        .is_ok()
    );
    for args in [
        UsageGetArgs {
            attempt_id: Some("".into()),
            ..Default::default()
        },
        UsageGetArgs {
            attempt_id: Some("a".into()),
            thread_id: Some("t".into()),
            ..Default::default()
        },
        UsageGetArgs {
            attempt_id: Some("a".into()),
            group_by: Some("thread".into()),
            ..Default::default()
        },
        UsageGetArgs {
            attempt_id: Some("a".into()),
            include_children: true,
            ..Default::default()
        },
        UsageGetArgs {
            attempt_id: Some("a".into()),
            since: Some(Box::from("2026-11-01T00:00:00Z")),
            ..Default::default()
        },
    ] {
        assert!(Command::UsageGet(args).validate().is_err());
    }
}

#[test]
fn usage_since_node_conflicts_with_since_and_shell_exit_grouping() {
    use shellboy::protocol::{Command, UsageGetArgs};
    assert!(
        Command::UsageGet(UsageGetArgs {
            since_node_id: Some(Box::from("node")),
            since: Some(Box::from("2026-11-01T00:00:00Z")),
            ..Default::default()
        })
        .validate()
        .is_err()
    );
    assert!(
        Command::UsageGet(UsageGetArgs {
            since_node_id: Some(Box::from("node")),
            until: Some("2026-11-02T00:00:00Z".into()),
            ..Default::default()
        })
        .validate()
        .is_ok()
    );
    assert!(
        Command::UsageGet(UsageGetArgs {
            since_node_id: Some(Box::from("node")),
            group_by: Some("shell-exit".into()),
            ..Default::default()
        })
        .validate()
        .is_ok()
    );
    assert!(
        Command::UsageGet(UsageGetArgs {
            since_node_id: Some(Box::from(" ")),
            ..Default::default()
        })
        .validate()
        .is_err()
    );
    assert!(
        Command::UsageGet(UsageGetArgs {
            since_node_id: Some(Box::from("node")),
            group_by: Some("thread".into()),
            cursor: Some("saved-page".into()),
            ..Default::default()
        })
        .validate()
        .is_ok()
    );
}

#[test]
fn exported_schema_matches_committed_protocol_schema() {
    let generated = shellboy::protocol::schema_json();
    let committed = include_str!("../docs/reference/protocol.schema.json");
    assert_eq!(
        generated.replace("\r\n", "\n").trim(),
        committed.replace("\r\n", "\n").trim(),
        "run `shellboy protocol schema > docs/reference/protocol.schema.json`",
    );
}

#[test]
fn exported_schema_local_references_resolve() {
    fn visit(value: &Value, document: &Value, path: &str) {
        match value {
            Value::Object(object) => {
                assert!(
                    path == "#" || !object.contains_key("definitions"),
                    "schema fragments must use the document root definitions at {path}"
                );
                if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                    && let Some(pointer) = reference.strip_prefix("#/")
                {
                    let mut current = document;
                    for token in pointer.split('/') {
                        let token = token.replace("~1", "/").replace("~0", "~");
                        current = if let Some(object) = current.as_object() {
                            object.get(&token)
                        } else if let (Some(array), Ok(index)) =
                            (current.as_array(), token.parse::<usize>())
                        {
                            array.get(index)
                        } else {
                            None
                        }
                        .unwrap_or_else(|| panic!("unresolved {reference} at {path}"));
                    }
                }
                for (key, child) in object {
                    visit(child, document, &format!("{path}/{key}"));
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    visit(child, document, &format!("{path}/{index}"));
                }
            }
            _ => {}
        }
    }
    let schema: Value = serde_json::from_str(&shellboy::protocol::schema_json()).unwrap();
    visit(&schema, &schema, "#");
}

#[test]
fn protocol_v5_handshake_and_typed_results_cover_existing_wire_modes() {
    let welcome: Value = serde_json::from_str(
        r#"{"protocol_version":5,"instance_id":"i","capabilities":["events.reasoning_activity"],"future_metadata":true}"#,
    ).unwrap();
    let decoded: shellboy::ipc::Welcome = serde_json::from_value(welcome).unwrap();
    assert_eq!(decoded.protocol_version, PROTOCOL_VERSION);
    assert_eq!(decoded.capabilities, ["events.reasoning_activity"]);
    let schema: Value = serde_json::from_str(&shellboy::protocol::schema_json()).unwrap();
    assert_eq!(schema["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(
        schema["capabilities"],
        serde_json::json!(["events.reasoning_activity"])
    );
    assert_eq!(schema["operation_results"].as_object().unwrap().len(), 63);
    for operation in ["status", "shell.list", "usage.get", "config.get"] {
        assert_ne!(schema["operation_results"][operation], Value::Bool(true));
    }
    assert!(schema["operation_results"]["status"]["anyOf"].is_array());
    assert!(schema["operation_results"]["usage.get"]["anyOf"].is_array());
    assert_eq!(schema["operation_results"]["shell.list"]["type"], "object");
    assert_eq!(schema["operation_results"]["config.get"]["type"], "object");
    assert_eq!(
        schema["definitions"]["ConfigLoadedView"]["properties"]["values"],
        Value::Bool(true)
    );
    assert!(
        schema["definitions"]["StatusSnapshot"]["properties"]["items"]["items"]["$ref"].is_string()
    );
    assert!(
        schema["definitions"]["StatusMemoryResult"]["properties"]["caches"]["type"] == "object"
    );
    assert_eq!(
        schema["definitions"]["UsageBreakdownResult"]["properties"]["items"]["items"],
        Value::Bool(true)
    );
    assert_eq!(
        schema["definitions"]["ShellListItem"]["properties"]["command_characters"]["type"],
        "integer"
    );
}

#[test]
fn typed_status_preserves_both_existing_modes_and_rejects_ambiguous_union_matches() {
    let activity: Value = serde_json::from_str(r###"{
        "instance_id":"i", "observed_at":"now", "scope":"durable_only",
        "consistency":"single_sqlite_read_transaction", "fingerprint":"f",
        "filters":{"memory":false,"thread_id":null,"run_id":null,"since":null,"state":[],"all":false,"limit":50,"cursor":null,"future_filter":{"x":1}},
        "items":[{"kind":"shell","id":"s","state":"running","thread_id":"t","thread_title":"title",
            "parent_thread_id":null,"current_connection":"c","current_model":"m","run_id":"r","owner_run_id":"r",
            "turn_id":null,"execution_id":"s","message_id":null,"attempt_id":null,"operation_id":null,
            "end_message_node_id":null,"response_nodes":[],"response_nodes_truncated":false,
            "created_at":null,"started_at":"now","ended_at":null,"updated_at":null,
            "activity_at":"now","elapsed_ms":null,"last_output_at":null,"attempt_connection":null,"attempt_model":null,
            "reason":null,"targets":[],"reply":{"delivered":null,"accepted":false,"future_reply_field":1},"stdin_open":true,"stdin_last_write_at":null}],
        "receipt_budget":{"accounted_bytes":1,"limit_bytes":2,"max_outcome_bytes":3,"result_retention_seconds":4,
            "result_retention":"24h_or_pressure","expired_replay":"operation_expired","control_headroom_bytes":5,
            "control_limit_bytes":6,"control_available_bytes":5,"protected_old_pending_bytes":0,"at_limit":false,
            "available_bytes":1,"full_result_admission_blocked":true,"recovery":"replay","future_budget":7},
        "future_snapshot":{"kept":true}}"###).unwrap();
    let decoded: StatusResult = serde_json::from_value(activity.clone()).unwrap();
    let StatusResult::Activity(snapshot) = decoded else {
        panic!("activity shape misclassified")
    };
    assert_eq!(snapshot.items[0].stdin_open, Some(true));
    let Some(Some(StatusReply::Object(reply))) = snapshot.items[0].reply.as_ref() else {
        panic!("structured reply shape misclassified");
    };
    assert_eq!(reply.delivered, Some(None));
    assert_eq!(reply.accepted, Some(Some(false)));
    assert_eq!(reply.additional_properties["future_reply_field"], 1);
    assert_eq!(
        serde_json::to_value(StatusResult::Activity(snapshot.clone())).unwrap(),
        activity
    );
    let raw_reply: StatusReply = serde_json::from_value(json!(["legacy", 1])).unwrap();
    assert_eq!(
        serde_json::to_value(raw_reply).unwrap(),
        json!(["legacy", 1])
    );
    let mut explicit_null = activity.clone();
    explicit_null["items"][0]["reply"] = Value::Null;
    let decoded: StatusResult = serde_json::from_value(explicit_null.clone()).unwrap();
    let StatusResult::Activity(snapshot) = decoded else {
        panic!("activity shape misclassified")
    };
    assert_eq!(snapshot.items[0].reply, Some(None));
    assert_eq!(
        serde_json::to_value(StatusResult::Activity(snapshot)).unwrap(),
        explicit_null
    );
    let mut missing = activity.clone();
    missing["items"][0].as_object_mut().unwrap().remove("reply");
    let decoded: StatusResult = serde_json::from_value(missing).unwrap();
    let StatusResult::Activity(snapshot) = decoded else {
        panic!("activity shape misclassified")
    };
    assert_eq!(snapshot.items[0].reply, None);
    assert!(
        serde_json::to_value(StatusResult::Activity(snapshot.clone())).unwrap()["items"][0]
            .get("reply")
            .is_none()
    );
    assert_eq!(
        snapshot.filters.additional_properties["future_filter"],
        json!({"x":1})
    );

    let memory: Value = serde_json::from_str(r###"{
        "process":{"pid":9,"platform":"linux","observed_at":"now","metrics":{
            "rss":{"bytes":null,"unavailable_reason":"not provided","future_metric_flag":true}},"future_process":"kept"},
        "caches":{"body":{"retained_bytes":null,"limit_bytes":10,"entries":null,"unavailable_reason":"unavailable"},
            "attachment":{"retained_bytes":4,"limit_bytes":10,"entries":1,"entry_unit":"path","unavailable_reason":null,"live_encoded_bytes":2,"pinned_bytes":1}},
        "accounting":"budget only", "future_memory_mode":"kept"}"###).unwrap();
    let decoded: StatusResult = serde_json::from_value(memory.clone()).unwrap();
    let StatusResult::Memory(observed) = decoded else {
        panic!("memory shape misclassified")
    };
    assert_eq!(
        observed.process.additional_properties["future_process"],
        "kept"
    );
    let encoded = serde_json::to_value(StatusResult::Memory(observed)).unwrap();
    assert_eq!(
        encoded, memory,
        "memory wire shape and null-vs-omitted fields roundtrip"
    );

    let ambiguous = json!({"instance_id":"i","observed_at":"now","scope":"s","consistency":"c","fingerprint":"f",
        "filters":{},"items":[],"receipt_budget":{},"process":{},"caches":{},"accounting":"a"});
    assert!(
        serde_json::from_value::<StatusResult>(ambiguous).is_err(),
        "union must not choose a lossy variant by order"
    );
}

#[test]
fn typed_result_records_tolerate_open_leaves_and_pre_283_missing_fields() {
    let shell: ShellListResult = serde_json::from_value(json!({
        "items":[{"execution_id":"e","run_id":"r","thread_id":"t","turn_id":null,
            "started_at":null,"ended_at":null,"state":"exited","exit_code":0,"command_excerpt":null,
            "command_characters":0,"command_excerpt_redacted":false,"command_excerpt_truncated":false,
            "started_at_known":false,"stdin_open":false,"stdin_last_write_at":null}],
        "next_cursor":null,"scanned":1,"scan_limit":2000,"scan_budget_reached":false,
        "period":{"since":null,"until":null,"bounds":"[since,until)","durations":"whole","unknown_starts":"excluded"},
        "ordering":"saved start","redaction":"conservative"
    })).unwrap();
    assert_eq!(shell.items[0].stdin_open, Some(false));
    assert_eq!(shell.items[0].stdin_last_write_at, None);
    let encoded = serde_json::to_value(shell).unwrap();
    assert_eq!(encoded["items"][0]["stdin_open"], false);

    let old_shell: ShellListResult = serde_json::from_value(json!({
        "items":[{"execution_id":"e","run_id":"r","thread_id":"t","started_at":null,"ended_at":null,
            "state":"exited","exit_code":0,"command_excerpt":null,"command_characters":0,
            "command_excerpt_redacted":false,"command_excerpt_truncated":false,"started_at_known":false}],
        "next_cursor":null,"scanned":0,"scan_limit":2000,"scan_budget_reached":false,
        "period":{"since":null,"until":null,"bounds":"[since,until)","durations":"whole","unknown_starts":"excluded"},
        "ordering":"saved start","redaction":"conservative"
    })).unwrap();
    assert!(
        old_shell.items[0].additional_properties.is_empty(),
        "pre-283 result remains decodable"
    );

    let config: ConfigGetResult = serde_json::from_value(json!({
        "config_path":null,
        "loaded":{"present":true,"content_hash":null,"values":{"connections":{"open_leaf":{"future_key":[1,null]}}},
            "redacted_fields":[],"credential_env_set":{"p":false},"credential_source":{"p":"none"},"future_loaded":1},
        "file":{"present":false,"content_hash":null,"valid":false,"error":"absent","values":null,
            "redacted_fields":[],"credential_source":null},
        "credentials_file":{"path":null,"present":false,"valid":true,"error":null},
        "differs":false,"differing_fields":null,"restart":{"all_fields_require_restart":false}
    })).unwrap();
    assert_eq!(
        config.loaded.values["connections"]["open_leaf"]["future_key"],
        json!([1, null])
    );
    assert!(config.file.values.is_none());
}

#[test]
fn typed_usage_result_preserves_null_omission_and_unknown_provider_usage_leaves() {
    let attempt = json!({"attempt_id":"a","thread_id":"t","run_id":"r","turn_id":"u","state":"completed",
        "usage":{"input_tokens":8,"input_tokens_details":{"cached_tokens":3,"provider_extra":"kept"},
            "provider_usage_extension":{"x":1}},
        "prompt_cache_diagnostics":{"type":"cache_miss","future_diag":true},
        "input_bytes":null});
    let decoded: UsageGetResult = serde_json::from_value(attempt.clone()).unwrap();
    let UsageGetResult::Attempt(value) = decoded else {
        panic!("attempt usage shape misclassified")
    };
    assert_eq!(
        value.usage.additional_properties["provider_usage_extension"],
        json!({"x":1})
    );
    assert_eq!(
        value
            .usage
            .input_tokens_details
            .as_ref()
            .unwrap()
            .additional_properties["provider_extra"],
        "kept"
    );
    assert_eq!(
        serde_json::to_value(UsageGetResult::Attempt(value)).unwrap(),
        attempt
    );
}

#[test]
fn shell_write_result_matches_encoded_runtime_result_for_all_statuses() {
    use shellboy::shell::{StdinWriteResult, StdinWriteStatus};

    for (status, bytes_written, stdin_open, reason) in [
        (StdinWriteStatus::Complete, 3, true, None),
        (StdinWriteStatus::Complete, 0, true, None),
        (StdinWriteStatus::Timeout, 2, true, Some("deadline")),
        (StdinWriteStatus::Closed, 0, false, None),
        (StdinWriteStatus::Stopped, 1, false, None),
        (StdinWriteStatus::Failed, 1, false, Some("pipe failed")),
    ] {
        let producer = StdinWriteResult {
            bytes_written,
            status,
            stdin_open,
            reason: reason.map(str::to_owned),
        };
        let actual_wire = serde_json::to_value(&producer).unwrap();
        let typed: ShellWriteResult = serde_json::from_value(actual_wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), actual_wire);
    }

    let additive = json!({
        "bytes_written": 3,
        "status": "complete",
        "stdin_open": true,
        "reason": null,
        "future_status_note": "preserve",
        "execution_id": "legacy-extra",
        "state": "running"
    });
    let typed: ShellWriteResult = serde_json::from_value(additive.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), additive);

    let old_only_shape = json!({"execution_id":"e","bytes_written":3,"state":"running"});
    assert!(serde_json::from_value::<ShellWriteResult>(old_only_shape).is_err());

    let schema: Value = serde_json::from_str(&shellboy::protocol::schema_json()).unwrap();
    let write_schema = &schema["operation_results"]["shell.write"];
    assert_eq!(write_schema["type"], "object");
    assert_eq!(
        write_schema["properties"]["status"]["$ref"],
        "#/definitions/ShellWriteStatus"
    );
    assert_eq!(write_schema["properties"]["stdin_open"]["type"], "boolean");
    assert!(write_schema["properties"].get("execution_id").is_none());
    assert!(write_schema["properties"].get("state").is_none());
}

#[test]
fn shell_write_result_required_fields_and_numeric_bounds_reject_malformed_values() {
    let good = json!({"bytes_written":0,"status":"complete","stdin_open":true,"reason":null});
    for field in ["bytes_written", "status", "stdin_open"] {
        let mut absent = good.clone();
        absent.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ShellWriteResult>(absent).is_err(),
            "missing {field}"
        );
        let mut null = good.clone();
        null[field] = Value::Null;
        assert!(
            serde_json::from_value::<ShellWriteResult>(null).is_err(),
            "null {field}"
        );
    }
    for bad in [
        json!(-1),
        json!(1.5),
        json!("0"),
        json!(true),
        serde_json::from_str::<Value>("18446744073709551616").unwrap(),
    ] {
        let mut value = good.clone();
        value["bytes_written"] = bad;
        assert!(serde_json::from_value::<ShellWriteResult>(value).is_err());
    }
    for (field, bad) in [
        ("status", json!("unknown")),
        ("status", json!(1)),
        ("stdin_open", json!("true")),
        ("reason", json!(42)),
    ] {
        let mut value = good.clone();
        value[field] = bad;
        assert!(serde_json::from_value::<ShellWriteResult>(value).is_err());
    }
    let mut maximum = good;
    maximum["bytes_written"] = json!(u64::MAX);
    let typed: ShellWriteResult = serde_json::from_value(maximum.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), maximum);
}

#[test]
fn shell_write_result_absent_reason_normalizes_separately_from_actual_null_wire() {
    let absent = json!({"bytes_written":0,"status":"complete","stdin_open":true});
    let typed: ShellWriteResult = serde_json::from_value(absent.clone()).unwrap();
    assert!(typed.reason.is_none());
    let mut normalized = absent;
    normalized["reason"] = Value::Null;
    assert_eq!(serde_json::to_value(typed).unwrap(), normalized);
    let additive = json!({"bytes_written":0,"status":"complete","stdin_open":true,"reason":null,"future_null":null,"future_object":{"nested":true}});
    let typed: ShellWriteResult = serde_json::from_value(additive.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), additive);
    // Known fields remain typed: duplicate keys do not become flattened extras.
    assert!(serde_json::from_str::<ShellWriteResult>(r#"{"bytes_written":0,"bytes_written":1,"status":"complete","stdin_open":true,"reason":null}"#).is_err());
}

#[test]
fn shell_write_failed_status_is_result_not_operation_error_and_legacy_value_survives() {
    use shellboy::shell::{StdinWriteResult, StdinWriteStatus};
    let producer = StdinWriteResult {
        bytes_written: 1,
        status: StdinWriteStatus::Failed,
        stdin_open: false,
        reason: Some("synthetic failure".into()),
    };
    let wire = serde_json::to_value(producer).unwrap();
    let success = Response::success("op", wire.clone());
    let encoded = serde_json::to_value(&success).unwrap();
    let legacy: Response<Value> = serde_json::from_value(encoded.clone()).unwrap();
    legacy.validate().unwrap();
    assert_eq!(legacy.result.as_ref().unwrap(), &wire);
    // Decode the typed payload through the existing generic envelope; requiring
    // Default on the result DTO would change production solely for this fixture.
    let typed: ShellWriteResult = serde_json::from_value(legacy.result.clone().unwrap()).unwrap();
    let rebuilt = Response::success("op", serde_json::to_value(typed).unwrap());
    assert_eq!(serde_json::to_value(rebuilt).unwrap(), encoded);
    let error: Response<Value> = Response::failure(
        "op",
        ApiError::new(ErrorCode::TargetUnavailable, "synthetic missing execution"),
    );
    error.validate().unwrap();
    let encoded_error = serde_json::to_value(error).unwrap();
    assert!(serde_json::from_value::<ShellWriteResult>(encoded_error.clone()).is_err());
    assert!(encoded_error.get("result").is_none());
}

#[test]
fn shell_write_status_exported_enum_is_exact() {
    let schema: Value = serde_json::from_str(&schema_json()).unwrap();
    assert_eq!(
        schema["definitions"]["ShellWriteStatus"]["enum"],
        json!(["complete", "timeout", "closed", "stopped", "failed"])
    );
}
