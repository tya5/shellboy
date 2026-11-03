use super::*;
use crate::cli::Cli;
use clap::{ArgAction, CommandFactory, Parser};
use std::collections::{BTreeMap, BTreeSet};

fn executable_commands(
    command: &clap::Command,
    path: &mut Vec<String>,
    leaves: &mut BTreeMap<String, clap::Command>,
) {
    if command.get_name() == "help" {
        // clap's generated help navigator is parser UI, not an executable action.
        return;
    }
    // clap's actual executable contract includes optional-subcommand parents
    // such as `compact`, in addition to the usual executable leaves.
    if !path.is_empty()
        && (command.get_subcommands().next().is_none() || !command.is_subcommand_required_set())
    {
        assert!(leaves.insert(path.join(" "), command.clone()).is_none());
    }
    for child in command.get_subcommands() {
        path.push(child.get_name().to_owned());
        executable_commands(child, path, leaves);
        path.pop();
    }
}

#[test]
fn fresh_clap_executable_paths_and_every_option_match_the_catalogue_bindings() {
    let mut parser = Cli::command();
    parser.build();
    let mut leaves = BTreeMap::new();
    executable_commands(&parser, &mut vec![], &mut leaves);
    assert_eq!(leaves.len(), 62, "track new CLI lifecycle paths explicitly");
    for name in ["serve", "shutdown"] {
        let lifecycle = leaves
            .remove(name)
            .expect("tracked headless lifecycle path");
        let actual = lifecycle
            .get_arguments()
            .filter(|arg| !matches!(arg.get_id().as_str(), "help" | "version"))
            .map(|arg| {
                format!(
                    "--{}",
                    arg.get_long().expect("lifecycle only has global options")
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual,
            GLOBAL_ARGUMENTS
                .iter()
                .map(|arg| arg.spelling.to_owned())
                .collect()
        );
        assert!(
            !CLI_BINDINGS.iter().any(|binding| binding.path == [name]),
            "headless lifecycle must not be offered as a TUI form"
        );
    }
    // A standalone read-only debug client (#137), deliberately not a TUI form:
    // it reads the database directly and never routes through an owner.
    let request = leaves
        .remove("thread request")
        .expect("tracked standalone debug path");
    let actual = request
        .get_arguments()
        .filter(|arg| !matches!(arg.get_id().as_str(), "help" | "version"))
        .map(|arg| format!("--{}", arg.get_long().expect("named options only")))
        .collect::<BTreeSet<_>>();
    let mut expected = ["--thread", "--attempt", "--compare-attempt", "--body"]
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
    expected.extend(GLOBAL_ARGUMENTS.iter().map(|arg| arg.spelling.to_owned()));
    assert_eq!(actual, expected);
    assert!(
        !CLI_BINDINGS
            .iter()
            .any(|binding| binding.path == ["thread", "request"]),
        "the debug client must not be offered as a TUI form"
    );
    assert_eq!(
        leaves.len(),
        59,
        "update executable-path inventory deliberately; UI launch/help/version are not operations"
    );
    let bindings = CLI_BINDINGS
        .iter()
        .map(|binding| (binding.path.join(" "), binding))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        bindings.len(),
        CLI_BINDINGS.len(),
        "one binding per executable spelling"
    );
    assert_eq!(
        leaves.keys().collect::<Vec<_>>(),
        bindings.keys().collect::<Vec<_>>(),
        "coverage is checked against actual clap, not OperationId::ALL"
    );
    for (path, command) in leaves {
        let binding = bindings[&path];
        let spec = binding.operation.spec();
        let actual = command
            .get_arguments()
            .filter(|arg| !matches!(arg.get_id().as_str(), "help" | "version"))
            .map(|arg| {
                let spelling = arg
                    .get_long()
                    .map(|long| format!("--{long}"))
                    .unwrap_or_else(|| {
                        assert!(
                            arg.get_index().is_some(),
                            "unexpected non-long/nonpositional argument: {path}: {arg:?}"
                        );
                        arg.get_id().to_string()
                    });
                (spelling, arg)
            })
            .collect::<BTreeMap<_, _>>();
        let mut expected = binding
            .arguments
            .iter()
            .map(|arg| arg.spelling.to_owned())
            .collect::<BTreeSet<_>>();
        expected.extend(GLOBAL_ARGUMENTS.iter().map(|arg| arg.spelling.to_owned()));
        if spec.route == Route::Rag {
            expected.insert("--db".into());
        }
        assert_eq!(
            actual.keys().cloned().collect::<BTreeSet<_>>(),
            expected,
            "{path}"
        );
        for declared in &binding.arguments {
            assert!(
                spec.fields.contains(&declared.field),
                "{path}: every CLI argument has a typed logical field"
            );
            let arg = actual[declared.spelling.as_str()];
            assert_eq!(
                arg.get_action().takes_values(),
                declared.takes_value,
                "{path} {} value/flag",
                declared.spelling
            );
            assert_eq!(
                matches!(arg.get_action(), ArgAction::Append),
                declared.repeat,
                "{path} {} repeat",
                declared.spelling
            );
            let defaults = arg
                .get_default_values()
                .iter()
                .map(|v| v.to_str().expect("CLI defaults are UTF8"))
                .collect::<Vec<_>>();
            assert_eq!(
                defaults,
                declared.default.as_deref().into_iter().collect::<Vec<_>>(),
                "{path} {} default",
                declared.spelling
            );
            if !declared.choices.is_empty() {
                // The CLI's enum declaration order is not the TUI's choice
                // order (attachment defaults prefer file before image).
                // Verify the complete accepted vocabulary without imposing it.
                let actual_choices = arg.get_possible_values();
                let actual_names = actual_choices
                    .iter()
                    .map(|value| value.get_name())
                    .collect::<BTreeSet<_>>();
                let declared_names = declared
                    .choices
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                assert_eq!(
                    actual_names.len(),
                    actual_choices.len(),
                    "{path} has duplicate CLI choices"
                );
                assert_eq!(
                    declared_names.len(),
                    declared.choices.len(),
                    "{path} has duplicate catalogue choices"
                );
                assert_eq!(
                    actual_names, declared_names,
                    "{path} {} choices",
                    declared.spelling
                );
            }
        }
    }
}

#[test]
fn identities_are_complete_and_attach_is_the_only_explicit_alias() {
    assert_eq!(OperationId::ALL.len(), 58);
    assert_eq!(SPECS.len(), 58);
    let ids = OperationId::ALL.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), OperationId::ALL.len());
    assert_eq!(
        SPECS.iter().map(|spec| spec.id).collect::<BTreeSet<_>>(),
        ids
    );
    for id in OperationId::ALL {
        assert_eq!(
            CLI_BINDINGS
                .iter()
                .filter(|binding| binding.operation == *id && binding.alias_of.is_none())
                .count(),
            1
        );
        let spec = id.spec();
        assert!(!spec.label.trim().is_empty() && !spec.meaning.trim().is_empty());
        assert_eq!(id.label_text().key, TextKey::Label(*id));
        assert_eq!(id.meaning_text().key, TextKey::Meaning(*id));
        assert!(spec.meaning.ends_with('。'), "complete meaning: {id:?}");
        for &field in spec.fields {
            let field = spec.field(field).unwrap();
            assert!(!field.field.label.is_empty() && !field.field.help.is_empty());
            assert_eq!(
                field.field.label_text(*id).key,
                TextKey::FieldLabel(*id, field.field.id)
            );
            assert_eq!(
                field.field.help_text(*id).key,
                TextKey::FieldHelp(*id, field.field.id)
            );
            assert!(!field.field.discovery.text().fallback.is_empty());
        }
    }
    let aliases = CLI_BINDINGS
        .iter()
        .filter(|binding| binding.alias_of.is_some())
        .collect::<Vec<_>>();
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].path, ["attach"]);
    assert_eq!(aliases[0].operation, OperationId::AttachmentAdd);
    assert_eq!(aliases[0].alias_of, Some(&["attachment", "add"][..]));
    assert_eq!(
        aliases[0]
            .arguments
            .iter()
            .find(|arg| arg.field == FieldId::Path)
            .unwrap()
            .spelling,
        "path"
    );
    assert_eq!(
        OperationId::AttachmentAdd
            .primary_cli()
            .arguments
            .iter()
            .find(|arg| arg.field == FieldId::Path)
            .unwrap()
            .spelling,
        "--path"
    );
}

#[test]
fn read_only_run_meaning_send_verb_and_mode_effects_are_factual() {
    let run = OperationId::RunGet.spec();
    assert_eq!(run.effect, Effect::Inspect);
    for word in ["保存状態", "Turn", "所有関係", "復旧理由"] {
        assert!(run.meaning.contains(word));
    }
    assert_eq!(
        OperationId::RunGet.enter_verb(run.effect),
        EnterVerb::Display
    );
    for id in OperationId::ALL {
        let verb = id.enter_verb(id.spec().effect);
        assert_eq!(verb == EnterVerb::Send, *id == OperationId::MessageSend);
        assert!(!verb.text().fallback.is_empty() && !id.spec().effect.text().fallback.is_empty());
    }
    for mode in [CompactMode::Deferred, CompactMode::Summary] {
        assert_eq!(compact_effect(mode), Effect::RuntimeControl);
    }
    for mode in [
        CompactMode::Maximal,
        CompactMode::Spill,
        CompactMode::Trim,
        CompactMode::Omit,
        CompactMode::Restore,
    ] {
        assert_eq!(compact_effect(mode), Effect::SavedMutation);
    }
    assert_eq!(rag_index_effect(true), Effect::Inspect);
    assert_eq!(rag_index_effect(false), Effect::RuntimeControl);
    for term in [Term::Run, Term::Turn, Term::Message, Term::Execution] {
        let text = term.text();
        assert_eq!(text.key, TextKey::Term(term));
        assert!(text.fallback.ends_with('。'));
    }
}

#[test]
fn runtime_cli_flags_and_mutual_exclusions_are_not_lost_in_form_metadata() {
    for (args, id, field) in [
        (
            vec!["history", "get", "--node", "node", "--final-answer-only"],
            OperationId::HistoryGet,
            FieldId::FinalAnswerOnly,
        ),
        (
            vec!["index", "rebuild", "--dimensions", "128"],
            OperationId::IndexRebuild,
            FieldId::Dimensions,
        ),
        (
            vec!["message", "cancel", "--message", "a", "--message", "b"],
            OperationId::MessageCancel,
            FieldId::Messages,
        ),
        (
            vec!["message", "send", "--deliver", "complete", "--text", "next"],
            OperationId::MessageSend,
            FieldId::Deliver,
        ),
        (
            vec!["exec", "--schema", "contract.json", "--thread", "t", "work"],
            OperationId::Exec,
            FieldId::Schema,
        ),
    ] {
        Cli::try_parse_from(std::iter::once("shellboy").chain(args)).unwrap();
        assert!(id.spec().field(field).is_some());
    }
    for args in [
        vec![
            "exec", "--schema", "s", "--thread", "t", "--model", "m", "work",
        ],
        vec!["message", "send", "--text", "x", "--text-stdin"],
        vec![
            "rag",
            "source",
            "add",
            "p",
            "--name",
            "n",
            "--exclude",
            "*.tmp",
            "--clear-excludes",
        ],
        vec!["usage", "get", "--run", "r", "--include-children"],
    ] {
        assert!(Cli::try_parse_from(std::iter::once("shellboy").chain(args)).is_err());
    }
    assert_eq!(
        OperationId::Exec.request_id_policy(),
        RequestIdPolicy::ComposedUnavailable
    );
    assert_eq!(
        OperationId::DocsGet.request_id_policy(),
        RequestIdPolicy::StandaloneUnavailable
    );
    assert_eq!(
        OperationId::ThreadGet.request_id_policy(),
        RequestIdPolicy::OwnerOperation
    );
}

#[test]
fn origins_describe_real_context_and_keep_cli_and_tui_defaults_distinct() {
    assert_eq!(
        OperationId::ThreadList
            .spec()
            .field(FieldId::Limit)
            .unwrap()
            .cli_default,
        Some("50")
    );
    assert_eq!(
        OperationId::ThreadList
            .spec()
            .field(FieldId::Limit)
            .unwrap()
            .tui_default,
        Some("20")
    );
    assert_eq!(
        OperationId::Status
            .spec()
            .field(FieldId::Limit)
            .unwrap()
            .tui_default,
        Some("50")
    );
    assert_eq!(
        OperationId::ThreadCreate
            .spec()
            .field(FieldId::Cwd)
            .unwrap()
            .default_origin,
        DefaultOrigin::ParentOrOwnerDefaults
    );
    assert_eq!(
        OperationId::Exec
            .spec()
            .field(FieldId::Cwd)
            .unwrap()
            .default_origin,
        DefaultOrigin::ProcessCwd
    );
    assert_eq!(
        OperationId::ShellRead
            .spec()
            .field(FieldId::Encoding)
            .unwrap()
            .default_origin,
        DefaultOrigin::SavedShellEncoding
    );
    assert_eq!(
        OperationId::HistorySearch
            .spec()
            .field(FieldId::Thread)
            .unwrap()
            .default_origin,
        DefaultOrigin::None
    );
    assert_eq!(
        OperationId::ThreadDelete.spec().confirmation,
        Confirmation::ThreadDeletePreview
    );
    assert_eq!(
        OperationId::IndexRebuild.spec().confirmation,
        Confirmation::IndexCostPreview
    );
    assert_eq!(
        OperationId::HistoryGet.spec().confirmation,
        Confirmation::None
    );
}

#[test]
fn globals_and_every_typed_field_have_complete_reachable_help_and_choices() {
    let mut spellings = BTreeSet::new();
    for argument in GLOBAL_ARGUMENTS
        .iter()
        .chain(std::iter::once(&RAG_DB_ARGUMENT))
    {
        assert!(argument.spelling.starts_with("--"));
        assert!(spellings.insert(argument.spelling));
        assert!(!argument.label.trim().is_empty());
        assert!(
            argument.meaning.ends_with('。'),
            "complete meaning for {}",
            argument.spelling
        );
    }
    assert_eq!(RAG_DB_ARGUMENT.spelling, "--db");
    assert!(RAG_DB_ARGUMENT.meaning.contains("DATA_DIR/rag.db"));
    let mut used = BTreeSet::new();
    for spec in SPECS {
        assert_eq!(
            spec.fields.iter().copied().collect::<BTreeSet<_>>().len(),
            spec.fields.len()
        );
        for &id in spec.fields {
            used.insert(id);
            let field = spec.field(id).unwrap();
            assert_eq!(field.field.id, id);
            assert!(field.field.help.ends_with('。'));
            if field.field.kind == FieldType::Choice {
                assert!(
                    !field.choices.is_empty(),
                    "{:?} {id:?} must expose its actual choices",
                    spec.id
                );
                assert_eq!(
                    field.choices.iter().collect::<BTreeSet<_>>().len(),
                    field.choices.len()
                );
            }
        }
    }
    assert_eq!(
        used,
        FIELD_SPECS
            .iter()
            .map(|field| field.id)
            .collect::<BTreeSet<_>>(),
        "no orphan field-help scaffolding"
    );
    assert_eq!(
        OperationId::Compact
            .spec()
            .field(FieldId::CompactAction)
            .unwrap()
            .choices,
        [
            "deferred", "maximal", "spill", "summary", "trim", "omit", "restore"
        ]
    );
    assert_eq!(
        OperationId::RagIndex
            .spec()
            .field(FieldId::SearchMode)
            .unwrap()
            .choices,
        ["keyword", "semantic"]
    );
    assert_eq!(
        OperationId::RagSearch
            .spec()
            .field(FieldId::SearchMode)
            .unwrap()
            .choices,
        ["keyword", "semantic", "hybrid"]
    );
}

#[test]
fn list_effect_markers_distinguish_changes_and_controls_before_opening_forms() {
    for (effect, expected) in [
        (Effect::Inspect, ""),
        (Effect::SavedMutation, "[変更] "),
        (Effect::RuntimeControl, "[制御] "),
    ] {
        let text = effect.list_marker();
        assert_eq!(text.key, TextKey::EffectMarker(effect));
        assert_eq!(text.fallback, expected);
    }
    assert_eq!(OperationId::CompactList.spec().effect, Effect::Inspect);
    assert_eq!(OperationId::ShellList.spec().effect, Effect::Inspect);
    assert_eq!(OperationId::Compact.spec().effect, Effect::RuntimeControl);
}

#[test]
fn executable_compact_parent_and_new_saved_read_flags_are_reachable() {
    for args in [
        vec!["compact"],
        vec![
            "compact",
            "list",
            "--thread",
            "t",
            "--since",
            "2026-10-30T00:00:00Z",
            "--until",
            "2026-10-31T00:00:00Z",
        ],
        vec![
            "shell",
            "list",
            "--thread",
            "t",
            "--command-contains",
            "printf",
            "--exit-code",
            "-1",
        ],
        vec!["status", "--memory"],
        vec![
            "usage",
            "get",
            "--since",
            "2026-10-30T00:00:00Z",
            "--until",
            "2026-10-31T00:00:00Z",
        ],
        vec![
            "wait",
            "--conditions-json",
            "[]",
            "--after",
            "opaque-cursor",
        ],
    ] {
        Cli::try_parse_from(std::iter::once("shellboy").chain(args)).unwrap();
    }
    assert!(Cli::try_parse_from(["shellboy", "status", "--memory", "--thread", "t"]).is_err());
    assert!(
        CLI_BINDINGS
            .iter()
            .any(|binding| binding.path == ["compact"])
    );
    assert!(
        CLI_BINDINGS
            .iter()
            .any(|binding| binding.path == ["compact", "list"])
    );
}

#[test]
fn headless_shutdown_has_no_tui_form_and_keeps_core_admission_boundary() {
    let command = crate::protocol::Command::OwnerShutdown(crate::protocol::OwnerShutdownArgs {
        instance_id: "current-owner".into(),
    });
    assert_eq!(OperationId::from_owner_command(&command), None);
}
