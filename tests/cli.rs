use clap::{Parser, error::ErrorKind};
use shellboy::{
    cli::{Cli, MAX_STDIN_TEXT_BYTES},
    protocol::{Command, Deliver, ErrorCode, MessageSendArgs},
};
#[cfg(unix)]
use std::ffi::OsString;
use std::path::Path;

fn cwd() -> &'static Path {
    if cfg!(windows) {
        Path::new("C:\\cli\\cwd")
    } else {
        Path::new("/cli/cwd")
    }
}
async fn convert(args: &[&str]) -> Command {
    let cli = Cli::try_parse_from(std::iter::once("shellboy").chain(args.iter().copied())).unwrap();
    cli.into_command_with_stdin(cwd(), &mut &b""[..])
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn events_wait_text_and_reasoning_activity_are_independent_opt_ins() {
    for (args, text, activity) in [
        (&["events", "wait"][..], false, false),
        (&["events", "wait", "--text"][..], true, false),
        (&["events", "wait", "--reasoning-activity"][..], false, true),
        (
            &["events", "wait", "--text", "--reasoning-activity"][..],
            true,
            true,
        ),
    ] {
        let Command::EventsWait(parsed) = convert(args).await else {
            panic!("expected events.wait");
        };
        assert_eq!(parsed.include_text, text);
        assert_eq!(parsed.include_reasoning_activity, activity);
    }
}
#[test]
fn usage_since_node_conflicts_with_time_and_attempt() {
    for args in [
        &[
            "shellboy",
            "usage",
            "get",
            "--since-node",
            "n",
            "--since",
            "2026-11-01T00:00:00Z",
        ][..],
        &[
            "shellboy",
            "usage",
            "get",
            "--attempt",
            "a",
            "--since-node",
            "n",
        ][..],
    ] {
        assert_eq!(
            Cli::try_parse_from(args).unwrap_err().kind(),
            ErrorKind::ArgumentConflict
        );
    }
}

#[tokio::test]
async fn all_public_cli_operations_map_to_shared_typed_contracts() {
    let cases: &[(&[&str], &str)] = &[
        (&["status"], "status"),
        (&["usage", "get", "--thread", "t"], "usage.get"),
        (
            &["usage", "get", "--thread", "t", "--since-node", "n"],
            "usage.get",
        ),
        (&["model", "list"], "model.list"),
        (&["shell", "list"], "shell.list"),
        (&["compact", "list", "--thread", "t"], "compact.list"),
        (&["thread", "list"], "thread.list"),
        (&["thread", "get", "--thread", "t"], "thread.get"),
        (&["thread", "create"], "thread.create"),
        (
            &["thread", "delete-preview", "--thread", "t", "--descendants"],
            "thread.delete_preview",
        ),
        (
            &["thread", "fork", "--thread", "t", "--node", "n"],
            "thread.fork",
        ),
        (&["thread", "delete", "--thread", "t"], "thread.delete"),
        (
            &["thread", "set", "--thread", "t", "--model", "m"],
            "thread.set",
        ),
        (
            &["message", "send", "--thread", "t", "--text", "hello"],
            "message.send",
        ),
        (&["message", "get", "--message", "m"], "message.get"),
        (&["operation", "get", "--operation", "op"], "operation.get"),
        (&["run", "get", "--run", "r"], "run.get"),
        (&["run", "retry", "--run", "r"], "run.retry"),
        (&["run", "stop", "--run", "r"], "run.stop"),
        (&["turn", "get", "--turn", "u"], "turn.get"),
        (
            &[
                "wait",
                "--conditions-json",
                r#"[{"type":"run_ended","run_id":"r"}]"#,
            ],
            "wait",
        ),
        (&["history", "list", "--thread", "t"], "history.list"),
        (&["history", "get", "--node", "n"], "history.get"),
        (
            &[
                "history",
                "relations",
                "--node",
                "n",
                "--relation",
                "sources",
            ],
            "history.relations",
        ),
        (
            &["history", "search", "--query", "needle"],
            "history.search",
        ),
        (&["shell", "get", "--execution", "x"], "shell.get"),
        (&["shell", "read", "--execution", "x"], "shell.read"),
        (
            &["shell", "write", "--execution", "x", "--text", "input"],
            "shell.write",
        ),
        (&["shell", "eof", "--execution", "x"], "shell.eof"),
        (&["shell", "stop", "--execution", "x"], "shell.stop"),
        (
            &[
                "attachment",
                "add",
                "--thread",
                "t",
                "--path",
                "file",
                "--kind",
                "file",
            ],
            "attachment.add",
        ),
        (&["attachment", "list", "--thread", "t"], "attachment.list"),
        (
            &["attachment", "remove", "--thread", "t", "--attachment", "a"],
            "attachment.remove",
        ),
        (&["compact", "--thread", "t"], "compact"),
        (
            &["compact", "--spill", "--from", "a", "--to", "b"],
            "context.edit",
        ),
        (
            &["compact", "--summary", "--from", "a", "--to", "b"],
            "context.edit",
        ),
        (&["compact", "--trim", "--to", "b"], "context.edit"),
        (
            &["compact", "--omit", "--from", "a", "--to", "b"],
            "context.edit",
        ),
        (&["compact", "--restore"], "context.edit"),
        (&["index", "status"], "index.status"),
        (&["index", "probe"], "index.probe"),
        (&["index", "backfill", "--scope", "all"], "index.backfill"),
        (&["index", "preview", "--scope", "all"], "index.preview"),
        (&["index", "retry"], "index.retry"),
        (&["index", "rebuild", "--thread", "t"], "index.rebuild"),
        (&["index", "shrink", "--dry-run"], "index.shrink"),
        (&["index", "shrink", "--cancel"], "index.shrink"),
        (
            &["index", "shrink", "--revert", "--dry-run"],
            "index.shrink",
        ),
        (
            &["index", "remove", "--space", "s", "--dry-run"],
            "index.remove",
        ),
        (&["storage", "status"], "storage.status"),
        (
            &["storage", "collect", "--category", "raw"],
            "storage.collect",
        ),
    ];
    assert_eq!(cases.len(), 51);
    for (args, name) in cases {
        let command = convert(args).await;
        assert_eq!(serde_json::to_value(command).unwrap()["command"], *name);
    }
}

#[tokio::test]
async fn compact_edit_modes_keep_exact_range_and_reject_ambiguous_arguments() {
    use shellboy::protocol::{ContextEditAction, ContextEditArgs};
    for (arguments, action, from, to) in [
        (
            vec!["--spill", "--from", "a", "--to", "b"],
            ContextEditAction::Spill,
            Some("a"),
            Some("b"),
        ),
        (
            vec!["--summary", "--from", "a", "--to", "b"],
            ContextEditAction::Summary,
            Some("a"),
            Some("b"),
        ),
        (
            vec!["--trim", "--to", "b"],
            ContextEditAction::Trim,
            None,
            Some("b"),
        ),
        (
            vec!["--omit", "--from", "a", "--to", "b"],
            ContextEditAction::Omit,
            Some("a"),
            Some("b"),
        ),
        (
            vec!["--restore", "--from", "marker"],
            ContextEditAction::Restore,
            Some("marker"),
            None,
        ),
        (vec!["--restore"], ContextEditAction::Restore, None, None),
    ] {
        let mut args = vec!["compact", "--thread", "t"];
        args.extend(arguments);
        assert_eq!(
            convert(&args).await,
            Command::ContextEdit(ContextEditArgs {
                max_chars: None,
                thread_id: Some("t".into()),
                action,
                from_node_id: from.map(str::to_owned),
                to_node_id: to.map(str::to_owned),
            })
        );
    }
    for arguments in [
        vec!["--spill", "--summary", "--from", "a", "--to", "b"],
        vec!["--spill", "--from", "a"],
        vec!["--summary", "--to", "b"],
        vec!["--omit", "--from", "a"],
        vec!["--omit", "--to", "b"],
        vec!["--omit", "--trim", "--from", "a", "--to", "b"],
        vec!["--trim"],
        vec!["--trim", "--from", "a", "--to", "b"],
        vec!["--restore", "--to", "b"],
        vec!["--from", "a", "--to", "b"],
    ] {
        let mut args = vec!["shellboy", "compact"];
        args.extend(arguments);
        if let Ok(cli) = Cli::try_parse_from(args.clone()) {
            assert!(
                cli.into_command_with_stdin(cwd(), &mut &b""[..])
                    .await
                    .is_err(),
                "{args:?}"
            );
        }
    }
}

#[tokio::test]
async fn defaults_globals_and_effort_clear_preserve_owner_and_wire_semantics() {
    let cli = Cli::try_parse_from([
        "shellboy",
        "--data-dir",
        "data",
        "thread",
        "list",
        "--config",
        "settings.toml",
        "--operation-id",
        "opaque",
    ])
    .unwrap();
    assert_eq!(cli.data_dir.as_deref(), Some(Path::new("data")));
    assert_eq!(cli.config.as_deref(), Some(Path::new("settings.toml")));
    assert_eq!(cli.operation_id.as_deref(), Some("opaque"));
    let clone = cli.clone();
    assert!(
        matches!(clone.into_command_with_stdin(cwd(),&mut &b""[..]).await.unwrap(),Some(Command::ThreadList(a)) if a.limit==50 && a.cursor.is_none())
    );
    let cli = Cli::try_parse_from(["shellboy"]).unwrap();
    assert!(
        cli.into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        matches!(convert(&["thread","set","--connection","proxy"]).await,Command::ThreadSet(a) if a.connection.as_deref()==Some("proxy") && a.model.is_none())
    );
    assert!(
        matches!(convert(&["thread","set","--effort",""]).await,Command::ThreadSet(a) if a.effort.as_deref()==Some(""))
    );
    assert!(
        matches!(convert(&["message","send","--text",""]).await,Command::MessageSend(a) if a.thread_id.is_none() && a.text.is_empty())
    );
    assert!(
        matches!(convert(&["history","get","--node","n"]).await,Command::HistoryGet(a) if a.limit_bytes==65536 && a.encoding=="UTF-8")
    );
    assert!(
        matches!(convert(&["history","search","--query","q"]).await,Command::HistorySearch(a) if a.limit==10)
    );
    assert!(
        matches!(convert(&["shell","read","--execution","x"]).await,Command::ShellRead(a) if a.encoding.is_none())
    );
    assert!(
        matches!(convert(&["shell","read","--execution","x","--encoding","CP932"]).await,Command::ShellRead(a) if a.encoding.as_deref()==Some("CP932"))
    );
}

#[tokio::test]
async fn attachment_alias_and_add_resolve_against_cli_cwd_without_accessing_file() {
    let canonical = convert(&[
        "attachment",
        "add",
        "--path",
        "missing/../日本語.png",
        "--kind",
        "image",
        "--thread",
        "t",
    ])
    .await;
    let alias = convert(&[
        "attach",
        "missing/../日本語.png",
        "--kind",
        "image",
        "--thread",
        "t",
    ])
    .await;
    assert_eq!(canonical, alias);
    match canonical {
        Command::AttachmentAdd(a) => assert_eq!(Path::new(&a.path), cwd().join("日本語.png")),
        _ => panic!(),
    }
    let command = convert(&["thread", "create", "--cwd", "relative/../project"]).await;
    assert!(
        matches!(command,Command::ThreadCreate(a) if a.cwd.as_deref()==Some("relative/../project"))
    );
}

#[tokio::test]
async fn stdin_text_preserves_exact_utf8_and_exclusive_text_source() {
    let cli = Cli::try_parse_from(["shellboy", "message", "send", "--text-stdin"]).unwrap();
    let mut input = &b"hello\n\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e\n\0"[..];
    let command = cli
        .into_command_with_stdin(cwd(), &mut input)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(command,Command::MessageSend(a) if a.text=="hello\n日本語\n\0"));
    let cli = Cli::try_parse_from([
        "shellboy",
        "shell",
        "write",
        "--execution",
        "x",
        "--text-stdin",
        "--encoding",
        "CP932",
    ])
    .unwrap();
    let command = cli
        .into_command_with_stdin(cwd(), &mut &b"text\n"[..])
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(command,Command::ShellWrite(a) if a.text=="text\n" && a.encoding=="CP932"));
    for args in [
        vec!["shellboy", "message", "send"],
        vec![
            "shellboy",
            "message",
            "send",
            "--text",
            "body",
            "--text-stdin",
        ],
        vec!["shellboy", "shell", "write", "--execution", "x"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
    // A --text operation never consumes stdin.
    let cli = Cli::try_parse_from(["shellboy", "message", "send", "--text", "body"]).unwrap();
    let mut unread = &b"unconsumed"[..];
    cli.into_command_with_stdin(cwd(), &mut unread)
        .await
        .unwrap();
    assert_eq!(unread, b"unconsumed");
}

#[tokio::test]
async fn stdin_invalid_utf8_and_overflow_reject_entire_input_without_echo() {
    let cli = Cli::try_parse_from(["shellboy", "message", "send", "--text-stdin"]).unwrap();
    let error = cli
        .into_command_with_stdin(cwd(), &mut &b"secret\xff"[..])
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(!error.message.contains("secret"));
    let bytes = vec![b'x'; MAX_STDIN_TEXT_BYTES + 2];
    let mut input = bytes.as_slice();
    let cli = Cli::try_parse_from(["shellboy", "message", "send", "--text-stdin"]).unwrap();
    let error = cli
        .into_command_with_stdin(cwd(), &mut input)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert_eq!(input.len(), 1);
}

#[tokio::test]
async fn invalid_flags_limits_wait_json_and_unselected_backfill_never_become_operations() {
    for args in [
        vec!["shellboy", "shell", "start", "--command", "x"],
        vec![
            "shellboy",
            "attachment",
            "add",
            "--path",
            "p",
            "--kind",
            "audio",
        ],
        vec!["shellboy", "index", "status", "--scope", "everything"],
        vec![
            "shellboy",
            "wait",
            "--conditions-json",
            "[]",
            "--timeout-ms",
            "-1",
        ],
        vec!["shellboy", "thread", "fork", "--thread", "t"],
        vec!["shellboy", "thread", "delete"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
    for args in [
        vec!["shellboy", "thread", "list", "--limit", "201"],
        vec![
            "shellboy",
            "history",
            "get",
            "--node",
            "n",
            "--limit-bytes",
            "65537",
        ],
        vec![
            "shellboy", "history", "search", "--query", "q", "--limit", "51",
        ],
        vec![
            "shellboy",
            "wait",
            "--conditions-json",
            "[{\"type\":\"invented\",\"secret\":\"hidden\"}]",
        ],
        vec!["shellboy", "wait", "--conditions-json", "[]"],
        vec!["shellboy", "thread", "set"],
        vec!["shellboy", "index", "backfill"],
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        let error = cli
            .into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(!error.message.contains("hidden"));
    }
}

#[test]
fn index_shrink_flags_map_to_one_operation_and_exclude_each_other() {
    let parsed = Cli::try_parse_from(["shellboy", "index", "shrink", "--dry-run", "--cancel"]);
    assert!(parsed.is_err(), "--dry-run conflicts with --cancel");
    assert!(Cli::try_parse_from(["shellboy", "index", "shrink"]).is_ok());
}

#[test]
fn help_and_version_are_display_success_and_unknown_operation_is_parse_error() {
    for args in [
        vec!["shellboy", "--help"],
        vec!["shellboy", "help", "thread"],
        vec!["shellboy", "shell", "--help"],
    ] {
        assert_eq!(
            Cli::try_parse_from(args).unwrap_err().kind(),
            ErrorKind::DisplayHelp
        );
    }
    let wait_help = Cli::try_parse_from(["shellboy", "wait", "--help"]).unwrap_err();
    assert_eq!(wait_help.kind(), ErrorKind::DisplayHelp);
    let wait_help = wait_help.to_string();
    assert!(wait_help.contains("reply_received"));
    assert!(wait_help.contains("\"message_id\":\"MESSAGE_ID\""));
    assert_eq!(
        Cli::try_parse_from(["shellboy", "--version"])
            .unwrap_err()
            .kind(),
        ErrorKind::DisplayVersion
    );
    assert_eq!(
        Cli::try_parse_from(["shellboy", "nonexistent"])
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidSubcommand
    );
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_paths_are_explicit_errors_instead_of_lossy_references() {
    use std::os::unix::ffi::OsStringExt;
    for prefix in [
        vec!["attach", "--kind", "file"],
        vec!["thread", "create", "--cwd"],
        vec!["thread", "set", "--cwd"],
        vec!["thread", "fork", "--thread", "t", "--node", "n", "--cwd"],
    ] {
        let args = std::iter::once(OsString::from("shellboy"))
            .chain(prefix.into_iter().map(OsString::from))
            .chain(std::iter::once(OsString::from_vec(b"bad-\xff".to_vec())))
            .collect::<Vec<_>>();
        let cli = Cli::try_parse_from(args).unwrap();
        let error = cli
            .into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.message, "path must be representable as UTF-8");
    }
}

#[tokio::test]
async fn operation_lookup_target_is_independent_of_global_request_id() {
    let cases: &[(&[&str], Option<&str>)] = &[
        (&["operation", "get", "--operation", "target:receipt"], None),
        (
            &[
                "--operation-id",
                "request:lookup",
                "operation",
                "get",
                "--operation",
                "target:receipt",
            ],
            Some("request:lookup"),
        ),
        (
            &[
                "operation",
                "--operation-id",
                "request:lookup",
                "get",
                "--operation",
                "target:receipt",
            ],
            Some("request:lookup"),
        ),
        (
            &[
                "operation",
                "get",
                "--operation-id",
                "request:lookup",
                "--operation",
                "target:receipt",
            ],
            Some("request:lookup"),
        ),
        (
            &[
                "operation",
                "get",
                "--operation",
                "target:receipt",
                "--operation-id",
                "request:lookup",
            ],
            Some("request:lookup"),
        ),
    ];
    for (args, expected_request_id) in cases {
        let cli = Cli::try_parse_from(std::iter::once("shellboy").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("{args:?}: {error}"));
        assert_eq!(
            cli.operation_id.as_deref(),
            *expected_request_id,
            "{args:?}"
        );
        let command = cli
            .into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            command,
            Command::OperationGet(shellboy::protocol::OperationTarget {
                operation_id: "target:receipt".into(),
            }),
            "{args:?}"
        );
    }
}

#[tokio::test]
async fn removed_status_operation_alias_is_rejected_and_help_uses_canonical_lookup() {
    for filter in [
        vec!["--thread", "t"],
        vec!["--run", "r"],
        vec!["--since", "2026-10-25T00:00:00Z"],
        vec!["--state", "failed"],
        vec!["--all"],
        vec!["--limit", "50"],
        vec!["--cursor", "cursor"],
    ] {
        let args: Vec<_> = ["shellboy", "status", "--operation", "target:receipt"]
            .into_iter()
            .chain(filter)
            .collect();
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
    }
    for target in ["", "target:receipt"] {
        assert!(Cli::try_parse_from(["shellboy", "status", "--operation", target]).is_err());
    }
    use clap::CommandFactory;
    let mut root = Cli::command();
    let status = root
        .find_subcommand_mut("status")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(!status.contains("Deprecated alias"));
    assert!(status.contains("operation get"));
    let operation = root
        .find_subcommand_mut("operation")
        .unwrap()
        .find_subcommand_mut("get")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(operation.contains("allocates no receipt"));
    assert!(!operation.contains("fresh observation ID"));
}

#[tokio::test]
async fn model_list_preserves_explicit_selection_and_rejects_empty_flags() {
    let Command::ModelList(args) = convert(&[
        "model",
        "list",
        "--connection",
        "proxy",
        "--thread",
        "target",
    ])
    .await
    else {
        panic!("model.list expected")
    };
    assert_eq!(args.connection.as_deref(), Some("proxy"));
    assert_eq!(args.thread_id.as_deref(), Some("target"));
    let Command::ModelList(args) = convert(&["model", "list"]).await else {
        panic!("model.list expected")
    };
    assert!(args.connection.is_none() && args.thread_id.is_none());
    for flag in ["--connection", "--thread"] {
        let cli = Cli::try_parse_from(["shellboy", "model", "list", flag, ""]).unwrap();
        assert!(
            cli.into_command_with_stdin(cwd(), &mut &b""[..])
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn thread_title_preserves_explicit_empty_reset() {
    assert!(
        matches!(convert(&["thread", "set", "--thread", "t", "--title", "作業タイトル"]).await,
        Command::ThreadSet(a) if a.title.as_deref() == Some("作業タイトル") && a.thread_id.as_deref() == Some("t"))
    );
    assert!(matches!(convert(&["thread", "set", "--title", ""]).await,
        Command::ThreadSet(a) if a.title.as_deref() == Some("") && a.thread_id.is_none()));
    assert!(matches!(convert(&["thread", "set", "--model", "m"]).await,
        Command::ThreadSet(a) if a.title.is_none()));
}

#[tokio::test]
async fn thread_set_max_tokens_passes_value_and_empty_restore() {
    assert!(
        matches!(convert(&["thread", "set", "--thread", "t", "--max-tokens", "64000"]).await,
        Command::ThreadSet(a) if a.max_tokens.as_deref() == Some("64000") && a.thread_id.as_deref() == Some("t"))
    );
    assert!(
        matches!(convert(&["thread", "set", "--max-tokens", ""]).await,
        Command::ThreadSet(a) if a.max_tokens.as_deref() == Some(""))
    );
    assert!(matches!(convert(&["thread", "set", "--model", "m"]).await,
        Command::ThreadSet(a) if a.max_tokens.is_none()));
    // Bad values parse as text and are rejected by the shared protocol validation.
    for bad in ["0", "-5", "1.5"] {
        let cli = Cli::try_parse_from(["shellboy", "thread", "set", "--max-tokens", bad]).unwrap();
        let error = cli
            .into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad}");
        assert!(error.message.contains("max_tokens"), "{}", error.message);
    }
}

#[tokio::test]
async fn search_filters_and_relation_direction_are_typed_without_implicit_target() {
    let command = convert(&[
        "history",
        "search",
        "--query",
        "q",
        "--thread",
        "t",
        "--kind",
        "user",
        "--kind",
        "assistant",
        "--exclude-kind",
        "tool",
        "--from",
        "2026-01-01T00:00:00+09:00",
        "--to",
        "2027-01-01T00:00:00Z",
    ])
    .await;
    assert!(
        matches!(command, Command::HistorySearch(a) if a.thread_id.as_deref()==Some("t") && a.kinds==["user", "assistant"] && a.exclude_kinds==["tool"] && a.from.is_some() && a.to.is_some())
    );
    assert!(
        matches!(convert(&["history", "search", "--query", "q"]).await, Command::HistorySearch(a) if a.thread_id.is_none() && a.kinds.is_empty())
    );
    assert!(
        matches!(convert(&["history", "relations", "--node", "n", "--relation", "replacements", "--limit", "2", "--cursor", "c"]).await, Command::HistoryRelations(a) if a.limit==2 && a.cursor.as_deref()==Some("c") && a.relation==shellboy::protocol::HistoryRelation::Replacements)
    );
}

#[tokio::test]
async fn shared_enum_vocabulary_keeps_cli_spellings_and_wire_values() {
    for name in ["image", "file"] {
        let value = serde_json::to_value(
            convert(&["attachment", "add", "--path", "missing", "--kind", name]).await,
        )
        .unwrap();
        assert_eq!(value["args"]["kind"], name);
    }
    for name in ["threads", "all"] {
        let value =
            serde_json::to_value(convert(&["index", "status", "--scope", name]).await).unwrap();
        assert_eq!(value["args"]["scope"], name);
    }
    for (cli, wire) in [
        ("raw", "raw"),
        ("managed-images", "managed_images"),
        ("history", "history"),
        ("index", "index"),
        ("all", "all"),
    ] {
        let value =
            serde_json::to_value(convert(&["storage", "status", "--category", cli]).await).unwrap();
        assert_eq!(value["args"]["category"], wire);
    }
    for name in ["previous", "next", "sources", "replacements"] {
        let value = serde_json::to_value(
            convert(&["history", "relations", "--node", "n", "--relation", name]).await,
        )
        .unwrap();
        assert_eq!(value["args"]["relation"], name);
    }
    for args in [
        vec![
            "shellboy",
            "storage",
            "status",
            "--category",
            "managed_images",
        ],
        vec![
            "shellboy",
            "history",
            "relations",
            "--node",
            "n",
            "--relation",
            "replacement",
        ],
        vec![
            "shellboy",
            "attachment",
            "add",
            "--path",
            "missing",
            "--kind",
            "other",
        ],
        vec!["shellboy", "index", "status", "--scope", "other"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[tokio::test]
async fn shared_defaults_keep_parser_and_wire_deserialization_identical() {
    for (argv, name, args) in [
        (vec!["thread", "list"], "thread.list", serde_json::json!({})),
        (
            vec!["history", "search", "--query", "q"],
            "history.search",
            serde_json::json!({"query":"q"}),
        ),
        (
            vec![
                "history",
                "relations",
                "--node",
                "n",
                "--relation",
                "previous",
            ],
            "history.relations",
            serde_json::json!({"node_id":"n","relation":"previous"}),
        ),
        (
            vec!["history", "get", "--node", "n"],
            "history.get",
            serde_json::json!({"node_id":"n"}),
        ),
        (
            vec!["shell", "write", "--execution", "x", "--text", "s"],
            "shell.write",
            serde_json::json!({"execution_id":"x","text":"s"}),
        ),
        (
            vec![
                "wait",
                "--conditions-json",
                "[{\"type\":\"run_ended\",\"run_id\":\"r\"}]",
            ],
            "wait",
            serde_json::json!({"conditions":[{"type":"run_ended","run_id":"r"}]}),
        ),
    ] {
        let parsed = convert(&argv).await;
        let wire: Command =
            serde_json::from_value(serde_json::json!({"command":name,"args":args})).unwrap();
        assert_eq!(parsed, wire, "{name}");
    }
}

#[tokio::test]
async fn malformed_history_since_is_rejected_before_trying_an_owner() {
    let cli = Cli::try_parse_from([
        "shellboy",
        "history",
        "list",
        "--thread",
        "t",
        "--since",
        "2026-10-25",
    ])
    .unwrap();
    let error = cli
        .into_command_with_stdin(cwd(), &mut &b""[..])
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.message, "since must be RFC3339 with timezone");
}

#[tokio::test]
async fn history_excerpt_cli_default_zero_and_upper_bound_map_to_the_wire() {
    for (extra, expected) in [
        (&[][..], 512),
        (&["--excerpt-chars", "0"][..], 0),
        (&["--excerpt-chars", "4096"][..], 4096),
    ] {
        let mut args = vec!["history", "list", "--thread", "t"];
        args.extend(extra);
        let Command::HistoryList(value) = convert(&args).await else {
            panic!("history list")
        };
        assert_eq!(value.excerpt_chars, expected);
    }
    for invalid in ["-1", "4097", "x"] {
        assert!(
            Cli::try_parse_from([
                "shellboy",
                "history",
                "list",
                "--thread",
                "t",
                "--excerpt-chars",
                invalid
            ])
            .is_err()
        );
    }
}

#[test]
fn rag_conversion_backend_flags_are_no_longer_part_of_the_cli() {
    for flag in ["--extractor", "--adapter"] {
        assert!(Cli::try_parse_from(["shellboy", "rag", "index", flag, "anything"]).is_err());
    }
}

#[tokio::test]
async fn message_delivery_timing_replaces_now_and_revisit_commands() {
    assert!(matches!(
        convert(&["message", "send", "--text", "x"]).await,
        Command::MessageSend(args) if !args.resume_recovery
    ));
    assert!(matches!(
        convert(&["message", "send", "--resume-recovery", "--text", "x"]).await,
        Command::MessageSend(args) if args.resume_recovery
    ));
    for (flag, deliver) in [
        (None, Deliver::Next),
        (Some("now"), Deliver::Now),
        (Some("complete"), Deliver::Complete),
    ] {
        let mut args = vec!["message", "send", "--text", "Check again"];
        if let Some(flag) = flag {
            args.extend(["--deliver", flag]);
        }
        assert_eq!(
            convert(&args).await,
            Command::MessageSend(MessageSendArgs {
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                thread_id: None,
                text: "Check again".into(),
                deliver,
            })
        );
    }
    for args in [
        vec!["shellboy", "message", "send", "--now", "--text", "x"],
        vec![
            "shellboy",
            "message",
            "send",
            "--deliver",
            "later",
            "--text",
            "x",
        ],
        vec!["shellboy", "revisit", "set", "--text", "x"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[tokio::test]
async fn explicit_index_dimension_commands_preserve_wire_selection() {
    let Command::IndexProbe(args) = convert(&["index", "probe", "--dimensions", "768"]).await
    else {
        panic!("probe")
    };
    assert_eq!(args.dimensions, Some(768));
    let Command::IndexRebuild(args) =
        convert(&["index", "rebuild", "--scope", "all", "--dimensions", "768"]).await
    else {
        panic!("rebuild")
    };
    assert_eq!(args.dimensions, Some(768));
    assert_eq!(args.scope, Some(shellboy::protocol::IndexScope::All));
    assert!(Cli::try_parse_from(["shellboy", "index", "retry", "--dimensions", "768"]).is_err());
}

#[tokio::test]
async fn agents_paths_remain_relative_until_target_cwd_is_known() {
    for (args, expected) in [
        (
            vec![
                "thread",
                "create",
                "--agents",
                "roles/coder.md",
                "--cwd",
                "project",
            ],
            "thread.create",
        ),
        (
            vec![
                "thread",
                "set",
                "--thread",
                "target",
                "--agents",
                "roles/coder.md",
                "--cwd",
                "project",
            ],
            "thread.set",
        ),
        (
            vec![
                "thread",
                "fork",
                "--thread",
                "target",
                "--node",
                "source",
                "--agents",
                "roles/coder.md",
                "--cwd",
                "project",
            ],
            "thread.fork",
        ),
    ] {
        let command = convert(&args).await;
        assert_eq!(command.name(), expected);
        let value = serde_json::to_value(command).unwrap();
        assert_eq!(value["args"]["agents"], "roles/coder.md");
        assert_eq!(value["args"]["cwd"], "project");
    }
    assert!(Cli::try_parse_from(["shellboy", "thread", "set", "--agents", ""]).is_err());
}

#[tokio::test]
async fn saved_usage_cli_scopes_are_read_only_and_mutually_exclusive() {
    for (args, kind) in [
        (vec!["usage", "get", "--turn", "u"], "turn_id"),
        (vec!["usage", "get", "--run", "r"], "run_id"),
        (
            vec!["usage", "get", "--thread", "t", "--include-children"],
            "thread_id",
        ),
    ] {
        let command = convert(&args).await;
        assert_eq!(command.name(), "usage.get");
        assert!(command.is_read_only());
        command.validate().unwrap();
        let value = serde_json::to_value(command).unwrap();
        assert!(value["args"][kind].is_string());
    }
    let grouped = convert(&[
        "usage",
        "get",
        "--group-by",
        "model",
        "--since",
        "2026-10-31T00:00:00Z",
        "--until",
        "2026-11-01T00:00:00Z",
        "--limit",
        "10",
        "--timezone",
        "UTC",
    ])
    .await;
    let value = serde_json::to_value(grouped).unwrap();
    assert_eq!(value["args"]["group_by"], "model");
    assert_eq!(value["args"]["since"], "2026-10-31T00:00:00Z");
    assert_eq!(value["args"]["limit"], 10);

    for args in [
        vec!["usage", "get", "--thread", "t", "--run", "r"],
        vec!["usage", "get", "--turn", "u", "--include-children"],
    ] {
        assert!(Cli::try_parse_from(std::iter::once("shellboy").chain(args)).is_err());
    }
}

#[tokio::test]
async fn sender_label_is_optional_bounded_display_metadata_not_origin() {
    let command = convert(&[
        "message",
        "send",
        "--thread",
        "target",
        "--sender-label",
        "別owner: リーダー",
        "--text",
        "hello",
    ])
    .await;
    assert!(
        matches!(command, Command::MessageSend(a) if a.sender_label.as_deref() == Some("別owner: リーダー"))
    );
    for label in ["界".repeat(65), "line\nbreak".into()] {
        let cli = Cli::try_parse_from([
            "shellboy",
            "message",
            "send",
            "--sender-label",
            &label,
            "--text",
            "hello",
        ])
        .unwrap();
        let error = cli
            .into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
}

#[tokio::test]
async fn compact_summary_limit_overrides_are_typed_and_summary_only() {
    let command = convert(&[
        "compact",
        "--summary",
        "--from",
        "a",
        "--to",
        "b",
        "--max-chars",
        "6000",
    ])
    .await;
    let Command::ContextEdit(args) = command else {
        panic!("expected context edit")
    };
    assert_eq!(args.max_chars, Some(6000));
    assert!(args.validate().is_ok());
    for arguments in [
        vec!["compact", "--max-chars", "6000"],
        vec![
            "compact",
            "--omit",
            "--from",
            "a",
            "--to",
            "b",
            "--max-percent",
            "2.5",
        ],
    ] {
        if let Ok(cli) = Cli::try_parse_from(std::iter::once("shellboy").chain(arguments)) {
            assert!(
                cli.into_command_with_stdin(cwd(), &mut &b""[..])
                    .await
                    .is_err()
            );
        }
    }
}

#[tokio::test]
async fn saved_readback_options_are_typed_bounded_and_observational() {
    crate::test_watchdog::watchdog("saved readback CLI schema", async {
        let c = convert(&[
            "shell",
            "list",
            "--thread",
            "t",
            "--since",
            "2026-10-31T00:00:00Z",
            "--until",
            "2026-11-01T00:00:00Z",
            "--command-contains",
            "rag search",
            "--state",
            "exited",
            "--exit-code",
            "-1",
            "--limit",
            "200",
        ])
        .await;
        assert!(c.is_read_only());
        let Command::ShellList(a) = c else { panic!() };
        assert_eq!(a.exit_code, Some(-1));
        assert_eq!(a.limit, 200);
        let c = convert(&["compact", "list", "--thread", "t", "--limit", "1"]).await;
        assert!(c.is_read_only());
        assert_eq!(c.name(), "compact.list");
        let c = convert(&[
            "usage",
            "get",
            "--thread",
            "t",
            "--since",
            "2026-10-31T09:00:00+09:00",
            "--until",
            "2026-11-01T00:00:00Z",
        ])
        .await;
        let Command::UsageGet(a) = c else { panic!() };
        assert!(a.since.is_some());
        assert!(a.until.is_some());
        for words in [
            vec!["shell", "list", "--limit", "201"],
            vec!["compact", "list", "--limit", "0"],
            vec!["usage", "get", "--thread", "t", "--since", "bad"],
            vec!["compact", "--summary", "list"],
        ] {
            match Cli::try_parse_from(std::iter::once("shellboy").chain(words)) {
                Err(_) => {}
                Ok(c) => assert!(
                    c.into_command_with_stdin(cwd(), &mut &b""[..])
                        .await
                        .is_err()
                ),
            }
        }
    })
    .await
    .expect("saved-readback CLI fixture watchdog");
}

#[tokio::test]
async fn status_memory_is_typed_read_only_and_excludes_activity_filters() {
    let command =
        crate::test_watchdog::watchdog("memory CLI conversion", convert(&["status", "--memory"]))
            .await
            .unwrap();
    assert!(matches!(command,Command::Status(ref a) if a.memory));
    command.validate().unwrap();
    for option in [
        vec!["--thread", "t"],
        vec!["--all"],
        vec!["--limit", "5"],
        vec!["--since", "2026-10-31T00:00:00Z"],
    ] {
        let mut args = vec!["shellboy", "status", "--memory"];
        args.extend(option);
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[tokio::test]
async fn thread_wait_after_cursor_maps_to_the_shared_contract_and_bounds_targets() {
    let command=convert(&["wait","--conditions-json",r#"[{"type":"thread_settled","thread_ids":["child-a","child-b"],"states":["completed","recovery_wait","failed","stopped"]}]"#,"--after","cursor","--timeout-ms","0"]).await;
    let Command::Wait(args) = command else {
        panic!("expected wait")
    };
    assert_eq!(args.after.as_deref(), Some("cursor"));
    assert_eq!(args.timeout_ms, 0);
    assert!(
        matches!(&args.conditions[0],shellboy::protocol::WaitCondition::ThreadSettled{thread_ids,..} if thread_ids.len()==2)
    );
    let conditions=serde_json::json!([{"type":"thread_settled","thread_ids":(0..65).map(|i|format!("t{i}")).collect::<Vec<_>>(),"states":["completed"]}]).to_string();
    let cli = Cli::try_parse_from(["shellboy", "wait", "--conditions-json", &conditions]).unwrap();
    assert_eq!(
        cli.into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let cli = Cli::try_parse_from([
        "shellboy",
        "wait",
        "--conditions-json",
        r#"[{"type":"run_ended","run_id":"r"}]"#,
        "--after",
        "cursor",
    ])
    .unwrap();
    assert_eq!(
        cli.into_command_with_stdin(cwd(), &mut &b""[..])
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn grouped_usage_cli_preserves_scope_timezone_limit_and_cursor() {
    for group in ["hour", "day", "thread", "shell-exit"] {
        let command = convert(&[
            "usage",
            "get",
            "--group-by",
            group,
            "--timezone=+09:00",
            "--limit",
            "12",
            "--cursor",
            "opaque",
            "--since",
            "2026-10-31T00:00:00Z",
            "--until",
            "2026-11-01T00:00:00Z",
        ])
        .await;
        let Command::UsageGet(a) = command else {
            panic!("usage command")
        };
        assert_eq!(a.group_by.as_deref(), Some(group));
        assert_eq!(a.timezone.as_deref(), Some("+09:00"));
        assert_eq!(a.limit, Some(12));
        assert_eq!(a.cursor.as_deref(), Some("opaque"));
        assert!(a.thread_id.is_none());
    }
    for flags in [
        vec!["--group-by", "month"],
        vec!["--timezone", "UTC"],
        vec!["--limit", "2"],
        vec!["--cursor", "opaque"],
    ] {
        assert!(
            Cli::try_parse_from(["shellboy", "usage", "get"].into_iter().chain(flags)).is_err()
        );
    }
}

#[tokio::test]
async fn usage_attempt_selector_is_typed_and_exclusive() {
    use clap::Parser;
    let cli = shellboy::cli::Cli::try_parse_from([
        "shellboy",
        "usage",
        "get",
        "--attempt",
        "attempt-fixture",
    ])
    .unwrap();
    let Some(shellboy::protocol::Command::UsageGet(args)) = cli
        .into_command_with_stdin(cwd(), &mut &b""[..])
        .await
        .unwrap()
    else {
        panic!("usage command")
    };
    assert_eq!(args.attempt_id.as_deref(), Some("attempt-fixture"));
    for extra in [
        vec!["--thread", "t"],
        vec!["--run", "r"],
        vec!["--turn", "u"],
        vec!["--include-children"],
        vec!["--since", "2026-11-01T00:00:00Z"],
        vec!["--group-by", "thread"],
    ] {
        let mut args = vec!["shellboy", "usage", "get", "--attempt", "a"];
        args.extend(extra);
        assert!(shellboy::cli::Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn message_ask_is_model_scoped_and_takes_text() {
    let cli = shellboy::cli::Cli::try_parse_from([
        "shellboy",
        "message",
        "ask",
        "--text",
        "choose A or B",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Some(shellboy::cli::CliCommand::Message {
            command: shellboy::cli::MessageCommand::Ask(_)
        })
    ));
}

#[test]
fn retry_help_distinguishes_acceptance_from_saved_outcome_without_new_flags() {
    let retry = Cli::try_parse_from(["shellboy", "run", "retry", "--help"]).unwrap_err();
    assert_eq!(retry.kind(), ErrorKind::DisplayHelp);
    let text = retry
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for required in [
        "not execution success",
        "ACK_TURN_ID",
        "snapshot.state",
        "context.recovery_reason",
        "BEFORE retry",
        "epoch is unknown",
        "128-input",
        "same operation ID",
    ] {
        assert!(
            text.contains(required),
            "missing retry guidance: {required}"
        );
    }
    let message = Cli::try_parse_from(["shellboy", "message", "send", "--help"])
        .unwrap_err()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(message.contains("acceptance is not execution success"));
    assert!(
        !text.contains("--wait"),
        "this change introduces no automatic observer flag"
    );
}

#[test]
fn message_get_help_uses_saved_context_and_body_read_without_resending() {
    let help = Cli::try_parse_from(["shellboy", "message", "get", "--help"]).unwrap_err();
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let text = help
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for fact in [
        "context.hold_reason",
        "history get --node MESSAGE_ID",
        "not proof of receipt termination or body loss",
        "fresh manual resend",
        "duplicate",
    ] {
        assert!(text.contains(fact), "missing held guidance: {fact}");
    }
}
