//! Explicit domain links; all CLI syntax facts come from the actual clap tree.
use super::*;
use clap::{ArgAction, CommandFactory};
type Meaning = (
    OperationId,
    &'static [&'static str],
    &'static [(&'static str, FieldId)],
    Option<&'static [&'static str]>,
);
const MEANINGS: &[Meaning] = &[
    (
        OperationId::CompactList,
        &["compact", "list"],
        &[
            ("thread_id", FieldId::Thread),
            ("since", FieldId::Since),
            ("until", FieldId::Until),
            ("limit", FieldId::Limit),
        ],
        None,
    ),
    (
        OperationId::ShellList,
        &["shell", "list"],
        &[
            ("thread_id", FieldId::Thread),
            ("since", FieldId::Since),
            ("until", FieldId::Until),
            ("command_contains", FieldId::CommandContains),
            ("state", FieldId::State),
            ("exit_code", FieldId::ExitCode),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::Status,
        &["status"],
        &[
            ("memory", FieldId::Memory),
            ("thread_id", FieldId::Thread),
            ("run_id", FieldId::Run),
            ("since", FieldId::Since),
            ("state", FieldId::States),
            ("all", FieldId::All),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::UsageGet,
        &["usage", "get"],
        &[
            ("group_by", FieldId::GroupBy),
            ("timezone", FieldId::Timezone),
            ("limit", FieldId::GroupLimit),
            ("cursor", FieldId::Cursor),
            ("since", FieldId::Since),
            ("until", FieldId::Until),
            ("thread_id", FieldId::Thread),
            ("run_id", FieldId::Run),
            ("turn_id", FieldId::Turn),
            ("include_children", FieldId::IncludeChildren),
            ("attempt_id", FieldId::Attempt),
        ],
        None,
    ),
    (
        OperationId::ModelList,
        &["model", "list"],
        &[
            ("connection", FieldId::Connection),
            ("thread_id", FieldId::Thread),
        ],
        None,
    ),
    (
        OperationId::ThreadList,
        &["thread", "list"],
        &[
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
            ("query", FieldId::Query),
            ("archived", FieldId::Archived),
        ],
        None,
    ),
    (
        OperationId::ThreadGet,
        &["thread", "get"],
        &[("thread_id", FieldId::Thread)],
        None,
    ),
    (
        OperationId::ThreadCreate,
        &["thread", "create"],
        &[
            ("agents", FieldId::Agents),
            ("cwd", FieldId::Cwd),
            ("connection", FieldId::Connection),
            ("model", FieldId::Model),
            ("effort", FieldId::Effort),
            ("parent_id", FieldId::Parent),
        ],
        None,
    ),
    (
        OperationId::ThreadFork,
        &["thread", "fork"],
        &[
            ("thread_id", FieldId::Thread),
            ("node_id", FieldId::Node),
            ("cwd", FieldId::Cwd),
            ("agents", FieldId::Agents),
        ],
        None,
    ),
    (
        OperationId::ThreadDelete,
        &["thread", "delete"],
        &[
            ("thread_id", FieldId::Thread),
            ("descendants", FieldId::Descendants),
        ],
        None,
    ),
    (
        OperationId::ThreadDeletePreview,
        &["thread", "delete-preview"],
        &[
            ("thread_id", FieldId::Thread),
            ("descendants", FieldId::Descendants),
        ],
        None,
    ),
    (
        OperationId::ThreadPrune,
        &["thread", "prune"],
        &[
            ("thread_id", FieldId::Thread),
            ("before_node_id", FieldId::Before),
            ("dry_run", FieldId::DryRun),
        ],
        None,
    ),
    (
        OperationId::ThreadBranches,
        &["thread", "branches"],
        &[
            ("thread_id", FieldId::Thread),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::ThreadRewind,
        &["thread", "rewind"],
        &[
            ("thread_id", FieldId::Thread),
            ("node_id", FieldId::Node),
            ("exclude_node", FieldId::ExcludeNode),
            ("dry_run", FieldId::DryRun),
        ],
        None,
    ),
    (
        OperationId::ThreadArchive,
        &["thread", "archive"],
        &[
            ("thread_ids", FieldId::Thread),
            ("descendants", FieldId::Descendants),
        ],
        None,
    ),
    (
        OperationId::ThreadUnarchive,
        &["thread", "unarchive"],
        &[
            ("thread_ids", FieldId::Thread),
            ("descendants", FieldId::Descendants),
        ],
        None,
    ),
    (
        OperationId::ThreadSet,
        &["thread", "set"],
        &[
            ("thread_id", FieldId::Thread),
            ("title", FieldId::Title),
            ("cwd", FieldId::Cwd),
            ("connection", FieldId::Connection),
            ("model", FieldId::Model),
            ("effort", FieldId::Effort),
            ("auto_compact_at", FieldId::AutoCompactAt),
            ("segment_chars", FieldId::SegmentChars),
            ("reduce_rounds", FieldId::ReduceRounds),
            ("parent_id", FieldId::Parent),
            ("agents", FieldId::Agents),
        ],
        None,
    ),
    (
        OperationId::MessageSend,
        &["message", "send"],
        &[
            ("thread_id", FieldId::Thread),
            ("sender_label", FieldId::SenderLabel),
            ("deliver", FieldId::Deliver),
            ("expect_reply", FieldId::ExpectReply),
            ("text", FieldId::Text),
            ("text_stdin", FieldId::Text),
        ],
        None,
    ),
    (
        OperationId::MessageReply,
        &["message", "reply"],
        &[
            ("message_id", FieldId::Message),
            ("sender_label", FieldId::SenderLabel),
            ("text", FieldId::Text),
            ("text_stdin", FieldId::Text),
        ],
        None,
    ),
    (
        OperationId::MessageGet,
        &["message", "get"],
        &[("message_id", FieldId::Message)],
        None,
    ),
    (
        OperationId::MessageCancel,
        &["message", "cancel"],
        &[("message_ids", FieldId::Messages)],
        None,
    ),
    (
        OperationId::OperationGet,
        &["operation", "get"],
        &[("lookup_operation_id", FieldId::Operation)],
        None,
    ),
    (
        OperationId::RunGet,
        &["run", "get"],
        &[("run_id", FieldId::Run)],
        None,
    ),
    (
        OperationId::RunRetry,
        &["run", "retry"],
        &[("run_id", FieldId::Run)],
        None,
    ),
    (
        OperationId::RunStop,
        &["run", "stop"],
        &[("run_id", FieldId::Run)],
        None,
    ),
    (
        OperationId::TurnGet,
        &["turn", "get"],
        &[("turn_id", FieldId::Turn)],
        None,
    ),
    (
        OperationId::Wait,
        &["wait"],
        &[
            ("after", FieldId::After),
            ("conditions_json", FieldId::Conditions),
            ("timeout_ms", FieldId::Timeout),
        ],
        None,
    ),
    (
        OperationId::HistoryList,
        &["history", "list"],
        &[
            ("thread_id", FieldId::Thread),
            ("run_id", FieldId::Run),
            ("since", FieldId::Since),
            ("state", FieldId::State),
            ("excerpt_chars", FieldId::ExcerptChars),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::HistoryGet,
        &["history", "get"],
        &[
            ("node_id", FieldId::Node),
            ("limit_bytes", FieldId::LimitBytes),
            ("cursor", FieldId::Cursor),
            ("encoding", FieldId::Encoding),
            ("end_message_only", FieldId::EndMessageOnly),
            ("parts", FieldId::Parts),
        ],
        None,
    ),
    (
        OperationId::HistorySearch,
        &["history", "search"],
        &[
            ("query", FieldId::Query),
            ("thread_id", FieldId::Thread),
            ("kinds", FieldId::Kinds),
            ("exclude_kinds", FieldId::ExcludeKinds),
            ("from", FieldId::From),
            ("to", FieldId::To),
            ("limit", FieldId::Limit),
        ],
        None,
    ),
    (
        OperationId::HistoryRelations,
        &["history", "relations"],
        &[
            ("node_id", FieldId::Node),
            ("relation", FieldId::Relation),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::ShellGet,
        &["shell", "get"],
        &[("execution_id", FieldId::Execution)],
        None,
    ),
    (
        OperationId::ShellRead,
        &["shell", "read"],
        &[
            ("execution_id", FieldId::Execution),
            ("cursor", FieldId::Cursor),
            ("encoding", FieldId::Encoding),
            ("limit_bytes", FieldId::LimitBytes),
        ],
        None,
    ),
    (
        OperationId::ShellWrite,
        &["shell", "write"],
        &[
            ("execution_id", FieldId::Execution),
            ("text", FieldId::Text),
            ("text_stdin", FieldId::Text),
            ("encoding", FieldId::Encoding),
        ],
        None,
    ),
    (
        OperationId::ShellEof,
        &["shell", "eof"],
        &[("execution_id", FieldId::Execution)],
        None,
    ),
    (
        OperationId::ShellStop,
        &["shell", "stop"],
        &[("execution_id", FieldId::Execution)],
        None,
    ),
    (
        OperationId::AttachmentAdd,
        &["attachment", "add"],
        &[
            ("thread_id", FieldId::Thread),
            ("path", FieldId::Path),
            ("kind", FieldId::AttachmentKind),
        ],
        None,
    ),
    (
        OperationId::AttachmentList,
        &["attachment", "list"],
        &[("thread_id", FieldId::Thread)],
        None,
    ),
    (
        OperationId::AttachmentRemove,
        &["attachment", "remove"],
        &[
            ("thread_id", FieldId::Thread),
            ("attachment_id", FieldId::Attachment),
        ],
        None,
    ),
    (
        OperationId::Compact,
        &["compact"],
        &[
            ("dry_run", FieldId::DryRun),
            ("thread_id", FieldId::Thread),
            ("maximal", FieldId::CompactAction),
            ("spill", FieldId::CompactAction),
            ("summary", FieldId::CompactAction),
            ("trim", FieldId::CompactAction),
            ("omit", FieldId::CompactAction),
            ("restore", FieldId::CompactAction),
            ("from_node_id", FieldId::FromNode),
            ("to_node_id", FieldId::ToNode),
            ("max_chars", FieldId::SummaryMaxChars),
            ("max_percent", FieldId::SummaryMaxPercent),
            ("segment_chars", FieldId::SummarySegmentChars),
            ("reduce_rounds", FieldId::SummaryReduceRounds),
        ],
        None,
    ),
    (
        OperationId::IndexStatus,
        &["index", "status"],
        &[
            ("scope", FieldId::IndexScope),
            ("space_id", FieldId::Space),
            ("thread_ids", FieldId::Threads),
            ("source_ids", FieldId::Sources),
        ],
        None,
    ),
    (
        OperationId::IndexPreview,
        &["index", "preview"],
        &[
            ("scope", FieldId::IndexScope),
            ("space_id", FieldId::Space),
            ("thread_ids", FieldId::Threads),
            ("source_ids", FieldId::Sources),
            ("kinds", FieldId::IndexKinds),
        ],
        None,
    ),
    (
        OperationId::IndexBackfill,
        &["index", "backfill"],
        &[
            ("scope", FieldId::IndexScope),
            ("space_id", FieldId::Space),
            ("thread_ids", FieldId::Threads),
            ("source_ids", FieldId::Sources),
            ("kinds", FieldId::IndexKinds),
        ],
        None,
    ),
    (
        OperationId::IndexRetry,
        &["index", "retry"],
        &[
            ("scope", FieldId::IndexScope),
            ("space_id", FieldId::Space),
            ("thread_ids", FieldId::Threads),
            ("source_ids", FieldId::Sources),
        ],
        None,
    ),
    (
        OperationId::IndexRebuild,
        &["index", "rebuild"],
        &[
            ("scope", FieldId::IndexScope),
            ("space_id", FieldId::Space),
            ("thread_ids", FieldId::Threads),
            ("source_ids", FieldId::Sources),
            ("kinds", FieldId::IndexKinds),
            ("dimensions", FieldId::Dimensions),
        ],
        None,
    ),
    (
        OperationId::IndexProbe,
        &["index", "probe"],
        &[("dimensions", FieldId::Dimensions)],
        None,
    ),
    (
        OperationId::StorageStatus,
        &["storage", "status"],
        &[("category", FieldId::Category)],
        None,
    ),
    (
        OperationId::StorageCollect,
        &["storage", "collect"],
        &[("category", FieldId::Category)],
        None,
    ),
    (
        OperationId::Exec,
        &["exec"],
        &[
            ("schema", FieldId::Schema),
            ("thread_id", FieldId::Thread),
            ("agents", FieldId::Agents),
            ("cwd", FieldId::Cwd),
            ("connection", FieldId::Connection),
            ("model", FieldId::Model),
            ("effort", FieldId::Effort),
            ("parent_id", FieldId::Parent),
            ("text", FieldId::Text),
            ("text_stdin", FieldId::Text),
            ("max_repairs", FieldId::MaxRepairs),
            ("result", FieldId::ExecResult),
            ("timeout_ms", FieldId::Timeout),
            ("sender_label", FieldId::SenderLabel),
        ],
        None,
    ),
    (OperationId::ConfigInit, &["config", "init"], &[], None),
    (OperationId::ConfigCheck, &["config", "check"], &[], None),
    (
        OperationId::DocsList,
        &["docs", "list"],
        &[
            ("parent", FieldId::DocParent),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::DocsGet,
        &["docs", "get"],
        &[
            ("id", FieldId::Document),
            ("section", FieldId::Section),
            ("limit_bytes", FieldId::LimitBytes),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::DocsSearch,
        &["docs", "search"],
        &[
            ("query", FieldId::Query),
            ("parent", FieldId::DocParent),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (OperationId::RagInit, &["rag", "init"], &[], None),
    (
        OperationId::RagSourceAdd,
        &["rag", "source", "add"],
        &[
            ("path", FieldId::Path),
            ("name", FieldId::SourceName),
            ("exclude", FieldId::Excludes),
            ("clear_excludes", FieldId::ClearExcludes),
        ],
        None,
    ),
    (
        OperationId::RagSourceList,
        &["rag", "source", "list"],
        &[],
        None,
    ),
    (
        OperationId::RagSourceRemove,
        &["rag", "source", "remove"],
        &[("source", FieldId::Source)],
        None,
    ),
    (
        OperationId::RagIndex,
        &["rag", "index"],
        &[
            ("source", FieldId::Source),
            ("dry_run", FieldId::DryRun),
            ("mode", FieldId::SearchMode),
        ],
        None,
    ),
    (
        OperationId::RagIngest,
        &["rag", "ingest"],
        &[
            ("id", FieldId::Document),
            ("source_file", FieldId::SourceFile),
            ("kind", FieldId::IngestKind),
            ("text_file", FieldId::TextFile),
            ("text_stdin", FieldId::Text),
            ("metadata_file", FieldId::MetadataFile),
        ],
        None,
    ),
    (OperationId::RagStatus, &["rag", "status"], &[], None),
    (
        OperationId::RagInspect,
        &["rag", "inspect"],
        &[("id", FieldId::Document)],
        None,
    ),
    (
        OperationId::RagSearch,
        &["rag", "search"],
        &[
            ("query", FieldId::Query),
            ("scope", FieldId::RagScopes),
            ("mode", FieldId::SearchMode),
            ("limit", FieldId::Limit),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::RagGet,
        &["rag", "get"],
        &[
            ("reference", FieldId::Reference),
            ("limit_bytes", FieldId::LimitBytes),
            ("cursor", FieldId::Cursor),
        ],
        None,
    ),
    (
        OperationId::AttachmentAdd,
        &["attach"],
        &[
            ("thread_id", FieldId::Thread),
            ("path", FieldId::Path),
            ("kind", FieldId::AttachmentKind),
        ],
        Some(&["attachment", "add"]),
    ),
];
pub(super) fn build() -> Vec<CliBinding> {
    let mut parser = crate::cli::Cli::command();
    parser.build();
    MEANINGS
        .iter()
        .map(|(operation, path, fields, alias_of)| {
            let mut command = &parser;
            for name in *path {
                command = command
                    .find_subcommand(name)
                    .expect("typed CLI path exists");
            }
            let arguments = fields
                .iter()
                .map(|(arg_id, field)| {
                    let arg = command
                        .get_arguments()
                        .find(|arg| arg.get_id().as_str() == *arg_id)
                        .expect("typed CLI field exists");
                    CliArgument {
                        spelling: arg
                            .get_long()
                            .map(|name| format!("--{name}"))
                            .unwrap_or_else(|| arg.get_id().to_string()),
                        field: *field,
                        takes_value: arg.get_action().takes_values(),
                        repeat: matches!(arg.get_action(), ArgAction::Append),
                        default: arg
                            .get_default_values()
                            .first()
                            .map(|s| s.to_str().expect("CLI defaults are UTF8").to_owned()),
                        choices: arg
                            .get_possible_values()
                            .iter()
                            .map(|v| v.get_name().to_owned())
                            .collect(),
                    }
                })
                .collect();
            CliBinding {
                operation: *operation,
                path,
                arguments,
                alias_of: *alias_of,
            }
        })
        .collect()
}
