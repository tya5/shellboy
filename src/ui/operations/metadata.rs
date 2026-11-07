//! Operation-specific help; typed builders and actual field values stay local.
use super::*;
use crate::operations::{Discovery, FieldType, Term, Text};

#[cfg(test)]
pub(super) fn text(value: Text) -> &'static str {
    text_for(value, crate::config::UiLanguage::Ja)
}

impl Form {
    pub(super) fn effect(&self) -> Effect {
        if self.operation == OpKind::RagIndex {
            return crate::operations::rag_index_effect(self.boolean("dry_run").unwrap_or(false));
        }
        if self.operation != OpKind::Compact {
            return self.operation.catalogue().spec().effect;
        }
        let mode = match self.value("action").unwrap_or("deferred") {
            "maximal" => CompactMode::Maximal,
            "spill" => CompactMode::Spill,
            "summary" => CompactMode::Summary,
            "trim" => CompactMode::Trim,
            "omit" => CompactMode::Omit,
            "restore" => CompactMode::Restore,
            _ => CompactMode::Deferred,
        };
        crate::operations::compact_effect(mode)
    }
    #[cfg(test)]
    pub(super) fn verb(&self) -> &'static str {
        text(self.operation.catalogue().enter_verb(self.effect()).text())
    }
    pub(super) fn metadata_id(&self, name: &str) -> Option<FieldId> {
        Some(match name {
            "thread" => FieldId::Thread,
            "run" => FieldId::Run,
            "turn" => FieldId::Turn,
            "attempt" => FieldId::Attempt,
            "node" => FieldId::Node,
            "before" => FieldId::Before,
            "ordinal" => FieldId::Ordinal,
            "expect_node" => FieldId::ExpectNode,
            "items_before" => FieldId::ItemsBefore,
            "items_after" => FieldId::ItemsAfter,
            "parent" => FieldId::Parent,
            "message" => FieldId::Message,
            "messages" => FieldId::Messages,
            "operation" => FieldId::Operation,
            "execution" => FieldId::Execution,
            "query" => FieldId::Query,
            "group_by" => FieldId::GroupBy,
            "timezone" => FieldId::Timezone,
            "limit" if self.operation == OpKind::UsageGet => FieldId::GroupLimit,
            "cursor" => FieldId::Cursor,
            "limit"
                if matches!(
                    self.operation,
                    OpKind::HistoryGet | OpKind::ShellRead | OpKind::DocsGet | OpKind::RagGet
                ) =>
            {
                FieldId::LimitBytes
            }
            "limit" => FieldId::Limit,
            "since" => FieldId::Since,
            "until" => FieldId::Until,
            "after" => FieldId::After,
            "memory" => FieldId::Memory,
            "command_contains" => FieldId::CommandContains,
            "exit_code" => FieldId::ExitCode,
            "state" if self.operation == OpKind::Status => FieldId::States,
            "state" => FieldId::State,
            "all" => FieldId::All,
            "children" => FieldId::IncludeChildren,
            "agents" => FieldId::Agents,
            "cwd" => FieldId::Cwd,
            "connection" => FieldId::Connection,
            "model" => FieldId::Model,
            "effort" => FieldId::Effort,
            "auto_compact_at" => FieldId::AutoCompactAt,
            "segment_chars" => FieldId::SegmentChars,
            "reduce_rounds" => FieldId::ReduceRounds,
            "title" | "title_mode" => FieldId::Title,
            "descendants" => FieldId::Descendants,
            "archived" => FieldId::Archived,
            "tags" => FieldId::Tags,
            "add_tags" => FieldId::AddTags,
            "remove_tags" => FieldId::RemoveTags,
            "sender_label" => FieldId::SenderLabel,
            "deliver" => FieldId::Deliver,
            "expect_reply" => FieldId::ExpectReply,
            "text" => FieldId::Text,
            "condition" | "target" => FieldId::Conditions,
            "settled_states" => FieldId::SettlementStates,
            "timeout" => FieldId::Timeout,
            "excerpt_chars" => FieldId::ExcerptChars,
            "end_message_only" => FieldId::EndMessageOnly,
            "parts" => FieldId::Parts,
            "encoding" => FieldId::Encoding,
            "relation" => FieldId::Relation,
            "kinds" => FieldId::Kinds,
            "exclude_kinds" => FieldId::ExcludeKinds,
            "from" => FieldId::From,
            "to" => FieldId::To,
            "path" => FieldId::Path,
            "kind" => FieldId::AttachmentKind,
            "attachment" => FieldId::Attachment,
            "action" => FieldId::CompactAction,
            "from_node" => FieldId::FromNode,
            "to_node" => FieldId::ToNode,
            "max_chars" => FieldId::SummaryMaxChars,
            "max_percent" => FieldId::SummaryMaxPercent,
            "summary_segment_chars" => FieldId::SummarySegmentChars,
            "summary_reduce_rounds" => FieldId::SummaryReduceRounds,
            "scope" => FieldId::IndexScope,
            "space" => FieldId::Space,
            "threads" => FieldId::Threads,
            "sources" => FieldId::Sources,
            "index_kinds" => FieldId::IndexKinds,
            "dimensions" => FieldId::Dimensions,
            "category" => FieldId::Category,
            "schema" => FieldId::Schema,
            "max_repairs" => FieldId::MaxRepairs,
            "exec_result" => FieldId::ExecResult,
            "doc_parent" => FieldId::DocParent,
            "document" => FieldId::Document,
            "section" => FieldId::Section,
            "source" => FieldId::Source,
            "source_name" => FieldId::SourceName,
            "excludes" => FieldId::Excludes,
            "clear_excludes" => FieldId::ClearExcludes,
            "dry_run" => FieldId::DryRun,
            "cancel" => FieldId::ShrinkCancel,
            "revert" => FieldId::ShrinkRevert,
            "exclude_node" => FieldId::ExcludeNode,
            "search_mode" => FieldId::SearchMode,
            "ingest_kind" => FieldId::IngestKind,
            "source_file" => FieldId::SourceFile,
            "text_file" | "input_mode" => FieldId::TextFile,
            "metadata_file" => FieldId::MetadataFile,
            "rag_scopes" => FieldId::RagScopes,
            "reference" => FieldId::Reference,
            _ => return None,
        })
    }
    pub(super) fn selected_metadata(&self) -> Option<crate::operations::OperationField> {
        let field = self.fields.get(self.selected)?;
        self.operation
            .catalogue()
            .spec()
            .field(self.metadata_id(field.name)?)
    }
    fn origin(&self, origin: DefaultOrigin) -> &'static str {
        let language = self.language;
        if !self.follows_current
            && matches!(
                origin,
                DefaultOrigin::SelectedThread
                    | DefaultOrigin::SelectedRun
                    | DefaultOrigin::SelectedTurn
                    | DefaultOrigin::FocusedNode
                    | DefaultOrigin::FocusedExecution
            )
        {
            return local!(
                language,
                "フォームを開いた参照先（編集値は自動変更しません）",
                "The reference when this form was opened; edits are not changed automatically"
            );
        }
        match origin {
            DefaultOrigin::None => local!(language, "未指定", "Not specified"),
            DefaultOrigin::Required => {
                local!(language, "明示指定が必要", "Must be specified explicitly")
            }
            DefaultOrigin::Literal => {
                local!(language, "この操作の既定値", "This operation's default")
            }
            DefaultOrigin::SelectedThread => local!(
                language,
                "フォームを開いた会話",
                "The Thread when this form was opened"
            ),
            DefaultOrigin::SelectedRun => local!(
                language,
                "フォームを開いたRun",
                "The Run when this form was opened"
            ),
            DefaultOrigin::SelectedTurn => local!(
                language,
                "フォームを開いたTurn",
                "The Turn when this form was opened"
            ),
            DefaultOrigin::FocusedNode => local!(
                language,
                "フォーカスした履歴ノード",
                "The focused history node"
            ),
            DefaultOrigin::FocusedExecution => {
                local!(language, "フォーカスしたExecution", "The focused Execution")
            }
            DefaultOrigin::ReferencedMessage => {
                local!(language, "参照した入力ID", "The referenced input ID")
            }
            DefaultOrigin::ReferencedOperation => {
                local!(language, "参照した受付ID", "The referenced operation ID")
            }
            DefaultOrigin::TargetThreadSettings => local!(
                language,
                "対象Threadの保存設定",
                "The target Thread's saved settings"
            ),
            DefaultOrigin::ThreadThenOwnerConfig => local!(
                language,
                "対象Thread、所有者の既定設定の順",
                "The target Thread, then the owner's defaults"
            ),
            DefaultOrigin::ProcessCwd => {
                local!(language, "TUIの起動場所", "The TUI launch directory")
            }
            DefaultOrigin::ActiveIndexSpace => local!(
                language,
                "現在の有効索引領域",
                "The current active index space"
            ),
            DefaultOrigin::ConfiguredEmbedding => local!(
                language,
                "現在の埋め込み設定",
                "The current embedding configuration"
            ),
            DefaultOrigin::ParentOrOwnerDefaults => local!(
                language,
                "親Threadまたは所有者の既定設定",
                "The parent Thread or owner's defaults"
            ),
            DefaultOrigin::SavedShellEncoding => local!(
                language,
                "対象Executionに保存された文字コード",
                "The target Execution's saved encoding"
            ),
        }
    }
    pub(super) fn field_hint(&self) -> String {
        use crate::config::UiLanguage;
        let language = self.language;
        let sep = crate::ui::text_width::glyphs::separator();
        let Some(field) = self.fields.get(self.selected) else {
            return String::new();
        };
        if field.name == "db" {
            return crate::ui::text_width::glyphs::decorate(local!(
                language,
                "空欄は起動時data-dir/rag.db | 相対パスは起動場所が基準 | F1で説明",
                "Empty uses the launch data-dir/rag.db; relative paths use the launch directory. F1 opens details."
            ));
        }
        if field.name == "input_mode" {
            return crate::ui::text_width::glyphs::decorate(local!(
                language,
                "fileは本文ファイル、inlineは本文入力。CLI stdin相当でも端末stdinは読みません | F1で説明",
                "file uses a text file; inline uses this form. The TUI does not read terminal stdin. F1 opens details."
            ));
        }
        let Some(metadata) = self.selected_metadata() else {
            return local!(
                language,
                "F1で入力条件を確認できます",
                "Press F1 for input requirements."
            )
            .into();
        };
        if field.value != field.initial_value {
            return crate::ui::text_width::glyphs::decorate(local!(
                language,
                "編集した値 | F1で入力条件とIDの探し方",
                "Edited value. F1 shows input requirements and where to find IDs."
            ));
        }
        let discovery = metadata.field.discovery;
        if field.value.is_empty() && discovery != Discovery::None {
            let help = text_for(discovery.text(), language);
            return match language {
                UiLanguage::Ja => format!("未入力{sep}{help}"),
                UiLanguage::En => format!("No value entered. {help}"),
            };
        }
        let origin = if !field.initial_value.is_empty()
            && matches!(field.kind, Kind::Choice(_) | Kind::Boolean | Kind::Number)
        {
            local!(language, "このフォームの初期選択", "Initial form selection")
        } else {
            self.origin(metadata.default_origin)
        };
        match language {
            UiLanguage::Ja => format!("初期値: {origin}{sep}F1で入力条件とIDの探し方"),
            UiLanguage::En => format!(
                "Initial value: {origin}. F1 shows input requirements and where to find IDs."
            ),
        }
    }

    pub(super) fn help_lines(&self) -> Vec<Line<'static>> {
        use crate::config::UiLanguage;
        use crate::operations::{Confirmation, IdKind, RequestIdPolicy, Route};
        let language = self.language;
        let sep = crate::ui::text_width::glyphs::separator();
        let id = self.operation.catalogue();
        let spec = id.spec();
        let mut lines = vec![
            Line::from(text_for(id.meaning_text(), language)),
            Line::from(text_for(self.effect().text(), language)),
            Line::from(self.operation.cli()),
            Line::from(""),
        ];
        if self
            .fields
            .get(self.selected)
            .is_some_and(|field| field.name == "db")
        {
            let argument = crate::operations::RAG_DB_ARGUMENT;
            lines.push(Line::from(match language {
                UiLanguage::Ja => format!("{}{sep}{}", argument.label, argument.meaning),
                UiLanguage::En => crate::ui::text_width::glyphs::decorate("Independent RAG database | An explicit path, or resolved DATA_DIR/rag.db when omitted. This is separate from the owner's conversation database."),
            }));
            lines.push(Line::from(local!(language,
                "空欄は起動時data-dir/rag.db。相対パスはTUI起動場所が基準です。会話履歴DBとは別です。",
                "Empty uses launch data-dir/rag.db. Relative paths use the TUI launch directory. This is separate from the conversation history database.")));
        }
        if let Some(field) = self.selected_metadata() {
            let label = text_for(field.field.label_text(id), language);
            lines.push(Line::from(match language {
                UiLanguage::Ja => format!("選択項目: {label}"),
                UiLanguage::En => format!("Selected field: {label}"),
            }));
            lines.push(Line::from(text_for(field.field.help_text(id), language)));
            if let Some(input) = self.fields.get(self.selected)
                && let Kind::Choice(choices) = input.kind
            {
                let choices = choices.join(" / ");
                lines.push(Line::from(match language {
                    UiLanguage::Ja => format!("このフォームの選択値: {choices}"),
                    UiLanguage::En => format!("Form choices: {choices}"),
                }));
            }
            let origin = self.origin(field.default_origin);
            lines.push(Line::from(match language {
                UiLanguage::Ja => format!("初期値の出所: {origin}"),
                UiLanguage::En => format!("Initial value source: {origin}"),
            }));
            if let Some(input) = self.fields.get(self.selected)
                && !matches!(input.kind, Kind::Multiline)
            {
                let initial = input.initial_value.chars().take(80).collect::<String>();
                lines.push(Line::from(match (language, initial.is_empty()) {
                    (UiLanguage::Ja, false) => format!("このフォームを開いたときの値: {initial}"),
                    (UiLanguage::En, false) => format!("Value when this form opened: {initial}"),
                    (_, true) => local!(
                        language,
                        "このフォームを開いたときの値: 未指定",
                        "Value when this form opened: not specified"
                    )
                    .into(),
                }));
            }
            if let Some(value) = field.tui_default {
                lines.push(Line::from(match language {
                    UiLanguage::Ja => format!("TUI既定: {value}"),
                    UiLanguage::En => format!("TUI default: {value}"),
                }));
            }
            if let Some(value) = field.cli_default {
                lines.push(Line::from(match language {
                    UiLanguage::Ja => format!("CLI既定: {value}"),
                    UiLanguage::En => format!("CLI default: {value}"),
                }));
            }
            if !field.choices.is_empty() {
                let choices = field.choices.join(" / ");
                lines.push(Line::from(match language {
                    UiLanguage::Ja => format!("選択値: {choices}"),
                    UiLanguage::En => format!("Choices: {choices}"),
                }));
            }
            let kind = match field.field.kind {
                FieldType::Id(kind) | FieldType::Ids(kind) => match kind {
                    IdKind::Thread => {
                        local!(language, "Thread ID（会話）", "Thread ID (conversation)")
                    }
                    IdKind::Run => local!(language, "Run ID（作業）", "Run ID (work)"),
                    IdKind::Turn => local!(
                        language,
                        "Turn ID（応答処理）",
                        "Turn ID (response interval)"
                    ),
                    IdKind::Node => local!(language, "Node ID（履歴ノード）", "Node ID (history)"),
                    IdKind::Message => local!(
                        language,
                        "Message ID（保存入力）",
                        "Message ID (saved input)"
                    ),
                    IdKind::Operation => local!(
                        language,
                        "Operation ID（受付要求）",
                        "Operation ID (receipt)"
                    ),
                    IdKind::Execution => local!(
                        language,
                        "Execution ID（OSコマンド実行）",
                        "Execution ID (OS command)"
                    ),
                    IdKind::Attachment => local!(
                        language,
                        "Attachment ID（添付参照）",
                        "Attachment ID (reference)"
                    ),
                    IdKind::Space => local!(language, "Space ID（索引領域）", "Space ID (index)"),
                },
                FieldType::Text => local!(language, "文字列", "Text"),
                FieldType::TextList => local!(language, "文字列の一覧", "List of text values"),
                FieldType::Multiline => local!(language, "複数行の本文", "Multiline text"),
                FieldType::Path => {
                    local!(language, "ファイル／フォルダのパス", "File or folder path")
                }
                FieldType::Signed => local!(
                    language,
                    "整数（負数を含みます）",
                    "Integer, including negative values"
                ),
                FieldType::Unsigned => local!(
                    language,
                    "0以上の整数（操作の上限に従います）",
                    "Non-negative integer, subject to operation limits"
                ),
                FieldType::Decimal => local!(
                    language,
                    "有限の数値（操作ごとの範囲に従います）",
                    "Finite number, subject to operation limits"
                ),
                FieldType::Boolean => local!(language, "はい／いいえ", "Yes or no"),
                FieldType::Choice => local!(language, "候補から選択", "Choose from the options"),
                FieldType::Conditions => local!(
                    language,
                    "種類と対象IDを組み合わせた観測条件",
                    "Observation conditions combining kinds and target IDs"
                ),
                FieldType::Timestamp => local!(
                    language,
                    "タイムゾーン付きRFC3339日時",
                    "RFC3339 timestamp with timezone"
                ),
                FieldType::Cursor => local!(
                    language,
                    "同じ読み取り対象の続きcursor",
                    "Continuation cursor for the same read target"
                ),
            };
            lines.push(Line::from(kind));
            lines.push(Line::from(text_for(field.field.discovery.text(), language)));
        }
        lines.push(Line::from(""));
        if matches!(self.build, FormBuilder::Client(_)) {
            lines.extend([
                Line::from(local!(language, "このフォームの入力合計は1MiBまでです。ファイル入力の上限は各操作の既存規則に従います。", "Total form input is limited to 1 MiB. Each operation's existing file-input limit also applies.")),
                Line::from(local!(language, "相対パスはTUI起動場所が基準です。現在のThread cwdで暗黙に解決しません。", "Relative paths use the TUI launch directory, not the current Thread cwd.")),
                Line::from(local!(language, "config/data-dirは起動時の選択を使用します。設定の変更は現在の所有者へ即時反映しません。", "Config and data-dir are fixed at launch. Configuration changes do not immediately update the running owner.")),
            ]);
            if self.operation == OpKind::Exec {
                lines.push(Line::from(local!(language, "依頼本文は所有する入力欄から渡します。CLIの--text-stdin相当でも端末stdinは読みません。Schemaと依頼の合計は128KiBまでです。", "The request text comes from this form, not terminal stdin. The schema and request together are limited to 128 KiB.")));
            }
            if self.operation == OpKind::RagIngest {
                lines.push(Line::from(local!(language, "本文入力元fileはtext-file、inlineは所有する本文入力です。切替時はもう一方の本文／ファイル欄を空にします。端末stdinは読みません。", "file uses text-file; inline uses form text. Clear the other field when changing source. Terminal stdin is not read.")));
            }
        }
        lines.push(Line::from(local!(
            language,
            "CLIの入力項目",
            "CLI arguments"
        )));
        for argument in &id.primary_cli().arguments {
            let spelling = &argument.spelling;
            let line = match (
                language,
                argument.repeat,
                argument.takes_value,
                argument.default.as_deref(),
            ) {
                (UiLanguage::Ja, true, _, Some(default)) => {
                    format!("{spelling}（繰り返し指定）{sep}既定 {default}")
                }
                (UiLanguage::Ja, true, _, None) => format!("{spelling}（繰り返し指定）"),
                (UiLanguage::Ja, false, true, Some(default)) => {
                    format!("{spelling} 値{sep}既定 {default}")
                }
                (UiLanguage::Ja, false, true, None) => format!("{spelling} 値"),
                (UiLanguage::Ja, false, false, Some(default)) => {
                    format!("{spelling}（フラグ）{sep}既定 {default}")
                }
                (UiLanguage::Ja, false, false, None) => format!("{spelling}（フラグ）"),
                (UiLanguage::En, true, _, Some(default)) => {
                    format!("{spelling} (repeatable; default {default})")
                }
                (UiLanguage::En, true, _, None) => format!("{spelling} (repeatable)"),
                (UiLanguage::En, false, true, Some(default)) => {
                    format!("{spelling} VALUE (default {default})")
                }
                (UiLanguage::En, false, true, None) => format!("{spelling} VALUE"),
                (UiLanguage::En, false, false, Some(default)) => {
                    format!("{spelling} (flag; default {default})")
                }
                (UiLanguage::En, false, false, None) => format!("{spelling} (flag)"),
            };
            lines.push(Line::from(line));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(local!(
            language,
            "所有者・設定のパスは、このTUIを起動したときの選択に固定されています。",
            "Owner and configuration paths are fixed to the selection used to start this TUI."
        )));
        for argument in crate::operations::GLOBAL_ARGUMENTS {
            lines.push(Line::from(global_argument_text(argument, language)));
        }
        if spec.route == Route::Rag {
            lines.push(Line::from(global_argument_text(
                &crate::operations::RAG_DB_ARGUMENT,
                language,
            )));
        }
        lines.push(Line::from(match id.request_id_policy() {
            RequestIdPolicy::OwnerOperation => local!(
                language,
                "この要求の受付IDはTUIが追跡します。参照する対象IDは入力欄で指定します。",
                "The TUI tracks this request's receipt ID. Enter target IDs separately."
            ),
            RequestIdPolicy::StandaloneUnavailable => local!(
                language,
                "この独立操作では単一のoperation-id指定は使用しません。",
                "A single operation-id is not used for this standalone operation."
            ),
            RequestIdPolicy::ComposedUnavailable => local!(
                language,
                "複数の要求を個別に追跡するため、単一のoperation-id指定は使用しません。",
                "Requests are tracked individually; a single operation-id is not used."
            ),
        }));
        lines.push(Line::from(match spec.confirmation {
            Confirmation::None => local!(language, "Enterで指定した操作を要求します。", "Enter requests the selected operation."),
            Confirmation::ThreadDeletePreview => local!(language, "削除対象と実行状態を確認してから削除します。", "Review deletion targets and running work before deleting."),
            Confirmation::IndexRemove => local!(language, "削除する索引と解放される量をdry-runで確認してから確定します。作り直すには埋め込みの費用がかかります。", "Review the dry run, then confirm. Recreating the index means paid embedding."),
            Confirmation::IndexCostPreview => local!(language, "対象件数と費用previewを確認してから索引を開始します。", "Review target counts and estimated cost before starting indexing."),
            Confirmation::PaidRequest => local!(language, "モデルAPIの利用や追加処理の費用が発生する場合があります。", "Model API use and additional processing may incur costs."),
            Confirmation::StorageCollect => local!(language, "指定した保存データの種類について回収を要求します。保護された参照は既存の回収規則に従います。", "Request collection of the specified saved data. Existing rules protect referenced data."),
            Confirmation::ContextEditPreview => local!(language, "編集する保存範囲と保護された作業を確認します。", "Review the saved range to edit and protected work before continuing."),
        }));
        for binding in crate::operations::CLI_BINDINGS
            .iter()
            .filter(|binding| binding.operation == id && binding.alias_of.is_some())
        {
            let path = binding.path.join(" ");
            lines.push(Line::from(match language {
                UiLanguage::Ja => format!("同じ操作の短縮形: shellboy {path}"),
                UiLanguage::En => format!("Alias for the same operation: shellboy {path}"),
            }));
        }
        lines.push(Line::from(""));
        for term in [Term::Run, Term::Turn, Term::Message, Term::Execution] {
            lines.push(Line::from(text_for(term.text(), language)));
        }
        lines
    }
}
use super::super::i18n::local;

pub(super) fn display_label(id: OperationId, language: crate::config::UiLanguage) -> String {
    text_for(id.label_text(), language).replace(" · ", crate::ui::text_width::glyphs::separator())
}

fn operation_label_for(id: OperationId, language: crate::config::UiLanguage) -> &'static str {
    match id {
        OperationId::CompactList => local!(
            language,
            "圧縮 · 保存済み一覧",
            "Compaction · Saved history"
        ),
        OperationId::ShellList => local!(language, "shell · 一覧", "Shell · List"),
        OperationId::Status => local!(language, "作業 · 状態一覧", "Work · Status"),
        OperationId::UsageGet => local!(language, "使用量 · 合計", "Usage · Total"),
        OperationId::ModelList => local!(language, "モデル · 一覧", "Models · List"),
        OperationId::ThreadList => local!(language, "会話 · 一覧", "Conversations · List"),
        OperationId::ThreadGet => local!(language, "会話 · 詳細", "Conversation · Details"),
        OperationId::ThreadBranches => {
            local!(language, "会話 · 分岐の木", "Conversation · Branches")
        }
        OperationId::ThreadCreate => local!(language, "会話 · 作成", "Conversation · Create"),
        OperationId::ThreadFork => local!(language, "会話 · 分岐", "Conversation · Fork"),
        OperationId::ThreadDelete => local!(language, "会話 · 削除", "Conversation · Delete"),
        OperationId::ThreadDeletePreview => local!(
            language,
            "会話 · 削除影響",
            "Conversation · Deletion impact"
        ),
        OperationId::ThreadPrune => local!(
            language,
            "会話 · 原文の解放",
            "Conversation · Prune originals"
        ),
        OperationId::ThreadRewind => {
            local!(language, "会話 · 巻き戻し", "Conversation · Rewind")
        }
        OperationId::ThreadArchive => {
            local!(language, "会話 · アーカイブ", "Conversation · Archive")
        }
        OperationId::ThreadUnarchive => local!(
            language,
            "会話 · アーカイブ解除",
            "Conversation · Unarchive"
        ),
        OperationId::ThreadTag => local!(language, "会話 · タグ", "Conversation · Tags"),
        OperationId::ThreadTags => {
            local!(language, "会話 · タグ一覧", "Conversations · Tag names")
        }
        OperationId::ThreadCwds => {
            local!(
                language,
                "会話 · 作業場所一覧",
                "Conversations · Working folders"
            )
        }
        OperationId::ThreadSet => local!(language, "会話 · 設定", "Conversation · Settings"),
        OperationId::MessageReply => local!(language, "依頼 · 返信", "Message · Reply"),
        OperationId::MessageSend => local!(language, "依頼 · 送信", "Message · Send"),
        OperationId::MessageGet => local!(
            language,
            "依頼 · 割当と回答",
            "Message · Assignment and response"
        ),
        OperationId::MessageCancel => local!(
            language,
            "依頼 · 未採用入力の取消",
            "Message · Cancel unadopted input"
        ),
        OperationId::OperationGet => local!(language, "操作 · 受付結果", "Operation · Receipt"),
        OperationId::RunGet => local!(language, "作業 · 詳細", "Run · Details"),
        OperationId::RunRetry => local!(language, "作業 · 復旧を再試行", "Run · Retry recovery"),
        OperationId::RunStop => local!(language, "作業 · 停止", "Run · Stop"),
        OperationId::TurnGet => local!(language, "応答処理 · 詳細", "Turn · Details"),
        OperationId::Wait => local!(language, "待機 · 条件を観測", "Wait · Observe conditions"),
        OperationId::HistoryList => local!(language, "履歴 · 一覧", "History · List"),
        OperationId::HistoryWindow => local!(language, "履歴 · 位置で読む", "History · Window"),
        OperationId::HistoryGet => local!(language, "履歴 · 原文", "History · Original"),
        OperationId::HistorySearch => local!(language, "履歴 · 検索", "History · Search"),
        OperationId::HistoryRelations => local!(language, "履歴 · 関係", "History · Relationships"),
        OperationId::ShellGet => local!(language, "shell · 詳細", "Shell · Details"),
        OperationId::ShellRead => local!(language, "shell · 保存出力", "Shell · Saved output"),
        OperationId::ShellWrite => local!(language, "shell · 入力", "Shell · Input"),
        OperationId::ShellEof => local!(language, "shell · 入力を閉じる", "Shell · Close input"),
        OperationId::ShellStop => local!(language, "shell · 停止", "Shell · Stop"),
        OperationId::AttachmentAdd => local!(language, "添付 · 追加", "Attachment · Add"),
        OperationId::AttachmentList => local!(language, "添付 · 一覧", "Attachment · List"),
        OperationId::AttachmentRemove => local!(language, "添付 · 除外", "Attachment · Remove"),
        OperationId::Compact => local!(language, "履歴 · 圧縮と編集", "History · Compact and edit"),
        OperationId::IndexStatus => local!(language, "索引 · 状態", "Index · Status"),
        OperationId::IndexPreview => local!(language, "索引 · 費用確認", "Index · Cost preview"),
        OperationId::IndexBackfill => local!(
            language,
            "索引 · 過去履歴を追加",
            "Index · Backfill history"
        ),
        OperationId::IndexRetry => {
            local!(language, "索引 · 失敗分を再試行", "Index · Retry failures")
        }
        OperationId::IndexRebuild => local!(language, "索引 · 作り直す", "Index · Rebuild"),
        OperationId::IndexProbe => local!(
            language,
            "索引 · 接続と次元を試す",
            "Index · Probe connection and dimensions"
        ),
        OperationId::IndexShrink => {
            local!(
                language,
                "索引 · 小さくする",
                "Index · Shrink without API calls"
            )
        }
        OperationId::IndexRemove => local!(
            language,
            "索引 · 使わない索引を削除",
            "Index · Remove an unused index"
        ),
        OperationId::StorageStatus => local!(language, "保存 · 状態", "Storage · Status"),
        OperationId::StorageCollect => local!(language, "保存 · 回収", "Storage · Collect"),
        OperationId::Exec => local!(
            language,
            "作業 · JSON契約で実行",
            "Work · Execute with JSON contract"
        ),
        OperationId::ConfigInit => local!(
            language,
            "設定 · テンプレート作成",
            "Config · Create template"
        ),
        OperationId::ConfigCheck => {
            local!(language, "設定 · 接続確認", "Config · Check connection")
        }
        OperationId::DocsList => local!(language, "ガイド · 一覧", "Guides · List"),
        OperationId::DocsGet => local!(language, "ガイド · 本文", "Guides · Text"),
        OperationId::DocsSearch => local!(language, "ガイド · 検索", "Guides · Search"),
        OperationId::RagInit => local!(language, "資料検索 · DB作成", "RAG · Initialize database"),
        OperationId::RagSourceAdd => {
            local!(language, "資料検索 · ソース登録", "RAG · Register source")
        }
        OperationId::RagSourceList => {
            local!(language, "資料検索 · ソース一覧", "RAG · List sources")
        }
        OperationId::RagSourceRemove => {
            local!(language, "資料検索 · ソース解除", "RAG · Remove source")
        }
        OperationId::RagIndex => local!(language, "資料検索 · 索引", "RAG · Index sources"),
        OperationId::RagIngest => local!(
            language,
            "資料検索 · 抽出済み本文を登録",
            "RAG · Ingest extracted text"
        ),
        OperationId::RagStatus => local!(language, "資料検索 · 状態", "RAG · Status"),
        OperationId::RagInspect => {
            local!(language, "資料検索 · 文書詳細", "RAG · Document details")
        }
        OperationId::RagSearch => local!(language, "資料検索 · 検索", "RAG · Search"),
        OperationId::RagGet => local!(language, "資料検索 · 原文", "RAG · Original"),
    }
}

fn operation_meaning_for(id: OperationId, language: crate::config::UiLanguage) -> &'static str {
    match id {
        OperationId::IndexRemove => local!(
            language,
            "指定した非アクティブの索引を完全に削除します。作り直すには埋め込みの費用がかかります。",
            "Delete one named inactive index for good. Recreating it means paid embedding."
        ),
        OperationId::IndexShrink => local!(
            language,
            "選択中の索引をAPIを使わず8ビットへ変換し、検索で読むベクトル量を約4分の1にします。完成まで古い索引で検索を続けます。古い索引は削除せず残します。",
            "Convert the active index to 8-bit values without any API call, so a search reads about a quarter of the vector bytes. Search keeps using the old index until the new one is complete. The old index is kept, not deleted."
        ),
        OperationId::CompactList => local!(
            language,
            "保存された圧縮の履歴を期間で絞って表示します。新しい圧縮は要求しません。",
            "Show saved compaction history filtered by time. This does not request a new compaction."
        ),
        OperationId::ShellList => local!(
            language,
            "保存されたshell実行を会話・期間・状態で絞って表示します。出力本文は読みません。",
            "List saved shell executions filtered by conversation, time, and state. This does not read output bodies."
        ),
        OperationId::Status => local!(
            language,
            "保存されたRun・Turn・shell・入力の状態と関連IDを指定範囲で表示します。",
            "Show saved Run, Turn, shell, and input states and related IDs within the specified scope."
        ),
        OperationId::UsageGet => local!(
            language,
            "指定したThread・Run・Turnの保存済みトークン使用量と処理時間を表示します。",
            "Show saved token usage and processing time for the specified Thread, Run, or Turn."
        ),
        OperationId::ModelList => local!(
            language,
            "指定接続、対象Threadの接続、所有者の既定接続の順でモデル一覧をAPIから取得します。",
            "Fetch models from the API using the specified connection, target Thread connection, or owner default, in that order."
        ),
        OperationId::ThreadList => local!(
            language,
            "保存された会話のタイトル、親子関係、現在の状態を絞り込んで表示します。",
            "Filter saved conversation titles, parent-child relationships, and current states."
        ),
        OperationId::ThreadGet => local!(
            language,
            "指定したThreadの設定、現在のRun・Turn、所有作業と復旧理由を表示します。",
            "Show the specified Thread's settings, current Run and Turn, owned work, and recovery reasons."
        ),
        OperationId::ThreadBranches => local!(
            language,
            "Threadの分岐（fork・編集・巻き戻しで分かれた子Thread）を木として、分かれた位置・先端の最後の依頼・時刻・大きさ・状態とともに表示します。",
            "Show a Thread's branches (child Threads made by fork, edit or rewind) as a tree with where they diverge, each tip's last request, time, size and state."
        ),
        OperationId::ThreadCreate => local!(
            language,
            "保存済みThreadを作成します。モデル処理は別の入力送信で開始します。",
            "Create a saved Thread. Model processing starts with a separate message."
        ),
        OperationId::ThreadFork => local!(
            language,
            "指定した履歴ノードを境界に、出典を保持した新しいThreadを作成します。",
            "Create a new Thread at the specified history node, retaining provenance."
        ),
        OperationId::ThreadDelete => local!(
            language,
            "指定Threadと選択した子孫を、実行中の作業や参照を再確認して削除します。",
            "Delete the specified Thread and selected descendants after rechecking running work and references."
        ),
        OperationId::ThreadDeletePreview => local!(
            language,
            "指定Threadと選択した子孫の削除を妨げる作業・参照・回収見込みを表示します。",
            "Show work and references blocking deletion and expected reclamation for the specified Thread and selected descendants."
        ),
        OperationId::ThreadPrune => local!(
            language,
            "指定ノードより前で採用済みの要約・spillの原文を、このThreadの保持から外します。回収は既存の到達性GCが行い、元に戻せません。",
            "Release this Thread's hold on originals of summaries and spills adopted before the node. The existing reachability GC reclaims them; this cannot be undone."
        ),
        OperationId::ThreadRewind => local!(
            language,
            "指定ノードまでの履歴に選択経路を戻します。切り取った部分は子Threadとして残り、シェルの副作用は戻りません。",
            "Cut the selected path back to the specified node. The removed part is kept as a child Thread; shell side effects are not undone."
        ),
        OperationId::ThreadArchive => local!(
            language,
            "指定Threadと選択した子孫を既定の一覧から外します。削除ではなく、履歴・検索・使用量・受信は変わらず、新しいRunが始まると一覧へ戻ります。",
            "Hide the specified Thread and selected descendants from the default list. This is not deletion: history, search, usage and receiving messages are unchanged, and a new Run returns the Thread to the list."
        ),
        OperationId::ThreadUnarchive => local!(
            language,
            "指定Threadと選択した子孫のアーカイブを解除し、既定の一覧へ戻します。",
            "Unarchive the specified Thread and selected descendants and return them to the default list."
        ),
        OperationId::ThreadTag => local!(
            language,
            "指定Threadへタグを付け外しします。置き場所や切り口を表す自由なラベルで、履歴・検索・使用量・受信・一覧の順序は変わりません。",
            "Add or remove tags on the specified Thread: free labels for placement and cross-cutting views. History, search, usage, receiving messages and list order are unchanged."
        ),
        OperationId::ThreadTags => local!(
            language,
            "使われているタグ名を重複なく、バイト順に表示します。件数は数えません。",
            "Show the distinct tag names in use in byte order. Member counts are not computed."
        ),
        OperationId::ThreadCwds => local!(
            language,
            "削除されていない会話（アーカイブ済みを含む）の保存cwdを重複なく、バイト順に表示します。件数は数えません。",
            "Show the distinct saved working folders of non-deleted conversations (archived included) in byte order. Counts are not computed."
        ),
        OperationId::ThreadSet => local!(
            language,
            "Threadの表示タイトル、作業場所、接続、モデル、推論設定、指示ファイル、親Threadを変更します。",
            "Change a Thread's display title, working folder, connection, model, reasoning effort, instruction file, and parent Thread."
        ),
        OperationId::MessageReply => local!(
            language,
            "受信した依頼へ一度だけ返信します。",
            "Reply once to a received request."
        ),
        OperationId::MessageSend => local!(
            language,
            "指定Threadへ入力を保存します。受付、Run・Turnへの採用、回答完了は個別に追跡します。",
            "Save input to the specified Thread. Receipt, Run and Turn assignment, and response completion are tracked separately."
        ),
        OperationId::MessageGet => local!(
            language,
            "保存された入力の受付状態と不変のRun・Turn割当、回答への参照を表示します。",
            "Show the saved message receipt state, immutable Run and Turn assignment, and response references."
        ),
        OperationId::MessageCancel => local!(
            language,
            "明示した未採用の入力を取り消します。既に履歴へ採用された入力は対象外です。",
            "Cancel explicitly specified unadopted input. Input already adopted into history is not eligible."
        ),
        OperationId::OperationGet => local!(
            language,
            "指定operation_idの保存済み受付結果と後続処理を確認し、不確実な再送を判断できます。",
            "Inspect the saved receipt and subsequent work for an operation_id to assess an uncertain resend."
        ),
        OperationId::RunGet => local!(
            language,
            "Runの保存状態、現在のTurn、所有関係、子作業・shellと保存された復旧理由を表示します。",
            "Show the Run's saved state, current Turn, ownership, child work and shells, and saved recovery reason."
        ),
        OperationId::RunRetry => local!(
            language,
            "原因を確認した復旧待ちTurnを、明示したRunで再開します。終了済みRunは再開できません。",
            "Resume a recovery-waiting Turn on the specified Run after checking its cause. A finished Run cannot resume."
        ),
        OperationId::RunStop => local!(
            language,
            "指定Runとその所有作業の停止を要求します。保存状態で実際の終了を確認します。",
            "Request that the specified Run and its owned work stop. Check saved state for actual completion."
        ),
        OperationId::TurnGet => local!(
            language,
            "一つのTurnの保存状態と回答ノードIDを表示します。回答本文は履歴原文から参照できます。",
            "Show the saved state and response node ID of one Turn. The response body is available from the original history."
        ),
        OperationId::Wait => local!(
            language,
            "指定条件のいずれかが成立するまで観測します。観測の期限切れ後も対象の作業は続行します。",
            "Observe until any specified condition is met. Target work continues after the observation deadline expires."
        ),
        OperationId::HistoryList => local!(
            language,
            "指定Threadの採用経路を、出典のRun・日時・関連shell状態で絞り込んで表示します。",
            "Show a Thread's adopted path filtered by source Run, time, and related shell state."
        ),
        OperationId::HistoryWindow => local!(
            language,
            "採用経路を位置（末尾・位置・ノード）を基準に前後の件数だけ読み、全体の件数と経路の版を返します。",
            "Read the adopted path around an end, position or node by item counts, returning the total length and path revision."
        ),
        OperationId::HistoryGet => local!(
            language,
            "指定ノードの保存原文と出典を取得します。終了メッセージのみを選ぶと推論や途中説明を除きます。",
            "Fetch a node's saved original and provenance. End-message-only excludes reasoning and interim explanations."
        ),
        OperationId::HistorySearch => local!(
            language,
            "指定範囲の会話履歴を埋め込み検索し、原文と出典への参照を返します。API費用が発生する場合があります。",
            "Search conversation history in the specified scope with embeddings and return original and provenance references. API costs may apply."
        ),
        OperationId::HistoryRelations => local!(
            language,
            "履歴ノードの前後、出典、置換関係を、不変DAGと現在の採用経路を区別して表示します。",
            "Show previous, next, source, and replacement relationships, distinguishing the immutable DAG from the adopted path."
        ),
        OperationId::ShellGet => local!(
            language,
            "対象コマンドの元のcommand、cwd、Run・Turn、保存状態と出力完了を表示します。",
            "Show the target command's original command and cwd, Run and Turn, saved state, and output completion."
        ),
        OperationId::ShellRead => local!(
            language,
            "指定Executionの保存済みstdout・stderrを、文字コードとcursorを指定して読み取ります。",
            "Read saved stdout and stderr of the specified Execution using an encoding and cursor."
        ),
        OperationId::ShellWrite => local!(
            language,
            "指定Executionのstdinへ入力を送ります。送信結果と相手の処理完了は別に確認します。",
            "Send input to the specified Execution's stdin. Sending and the receiver's completion are checked separately."
        ),
        OperationId::ShellEof => local!(
            language,
            "指定Executionのstdinを閉じ、これ以降の入力を終了します。",
            "Close the specified Execution's stdin, ending further input."
        ),
        OperationId::ShellStop => local!(
            language,
            "指定ExecutionのOSプロセス群へ停止を要求します。Run全体の停止とは対象が異なります。",
            "Request termination of the specified Execution's OS process group. Its scope differs from stopping the whole Run."
        ),
        OperationId::AttachmentAdd => local!(
            language,
            "対象Threadへファイルまたは画像のパス参照を追加します。読み取り・更新・出典は既存の添付規則に従います。",
            "Add a file or image path reference to the target Thread. Existing attachment rules govern reading, updates, and provenance."
        ),
        OperationId::AttachmentList => local!(
            language,
            "対象Threadで選択されている添付参照とその状態を表示します。",
            "Show attachment references selected for the target Thread and their states."
        ),
        OperationId::AttachmentRemove => local!(
            language,
            "指定attachment_idをThreadの添付選択から外します。外部ファイルは削除しません。",
            "Remove an attachment_id from the Thread's selection. This does not delete the external file."
        ),
        OperationId::Compact => local!(
            language,
            "安全な境界で圧縮を要求するか、指定範囲の退避・要約・削除・復元を行います。要約はモデルAPIを使用します。",
            "Request compaction at a safe boundary or spill, summarize, remove, or restore a range. Summarization uses the model API."
        ),
        OperationId::IndexStatus => local!(
            language,
            "会話索引の設定、領域、処理件数と停止・失敗理由を表示します。",
            "Show conversation index settings, spaces, processed counts, and stop or failure reasons."
        ),
        OperationId::IndexPreview => local!(
            language,
            "指定資料の索引対象件数と読み取り可能な入力量を、埋め込みAPIを呼ばずに確認します。",
            "Inspect target counts and readable input size without calling the embedding API."
        ),
        OperationId::IndexBackfill => local!(
            language,
            "明示した過去履歴の埋め込み索引を開始します。事前確認とAPI費用の確認が必要です。",
            "Start embedding indexing for explicitly selected past history. Review the preview and API costs first."
        ),
        OperationId::IndexRetry => local!(
            language,
            "対象の失敗済み索引処理を、現在の設定で再試行します。API費用が発生する場合があります。",
            "Retry failed indexing work under current settings. API costs may apply."
        ),
        OperationId::IndexRebuild => local!(
            language,
            "指定した資料と任意の次元数で候補索引を構築します。検証成功まで既存の有効領域を保持します。",
            "Build a candidate index for selected sources and optional dimensions. The existing active space remains until validation succeeds."
        ),
        OperationId::IndexProbe => local!(
            language,
            "一つの固定入力を埋め込みAPIへ送り、接続・次元の結果を保存します。API費用が発生する場合があります。",
            "Send one fixed input to the embedding API and save connection and dimension results. API costs may apply."
        ),
        OperationId::StorageStatus => local!(
            language,
            "指定カテゴリの論理使用量、上限、保持と回収に関する保存情報を表示します。",
            "Show saved logical usage, limits, retention, and reclamation information for selected categories."
        ),
        OperationId::StorageCollect => local!(
            language,
            "指定カテゴリで回収可能な保存データを明示的に回収します。保護された原文と参照は保持します。",
            "Explicitly collect reclaimable saved data in selected categories. Protected originals and references are retained."
        ),
        OperationId::Exec => local!(
            language,
            "保存Threadへ依頼し、最終JSON回答をローカルschemaで検証します。修復回数の範囲で追加のモデル・tool処理を行います。",
            "Request work on a saved Thread and validate final JSON against a local schema. Additional model and tool processing may run within the repair limit."
        ),
        OperationId::ConfigInit => local!(
            language,
            "選択したconfigパスへテンプレートを作成します。既存ファイルは上書きしません。",
            "Create a template at the selected config path. Existing files are not overwritten."
        ),
        OperationId::ConfigCheck => local!(
            language,
            "設定ファイルと環境変数を読み、既定接続のモデル一覧を取得して設定を確認します。実行中の所有者への反映は再起動時です。",
            "Read config and environment and fetch models from the default connection. Changes reach the running owner on restart."
        ),
        OperationId::DocsList => local!(
            language,
            "この実行ファイルに同梱されたガイドを階層ごとに表示します。",
            "List the bundled guides by hierarchy."
        ),
        OperationId::DocsGet => local!(
            language,
            "同梱ガイドの文書IDと任意の見出しから、版付き本文を読み取ります。",
            "Read versioned bundled guide text by document ID and optional heading."
        ),
        OperationId::DocsSearch => local!(
            language,
            "同梱ガイドの文字列を検索します。所有者や埋め込みAPIは不要です。",
            "Search bundled guide text without an owner or embedding API."
        ),
        OperationId::RagInit => local!(
            language,
            "指定した独立RAGデータベースを初期化します。会話の所有者は起動しません。",
            "Initialize the specified independent RAG database without starting the conversation owner."
        ),
        OperationId::RagSourceAdd => local!(
            language,
            "資料フォルダまたはファイルを名前付きソースとして登録し、除外パターンを保存します。",
            "Register a document folder or file as a named source and save exclusion patterns."
        ),
        OperationId::RagSourceList => local!(
            language,
            "独立RAGに登録した資料ソースと除外設定を表示します。",
            "Show registered independent RAG sources and exclusion settings."
        ),
        OperationId::RagSourceRemove => local!(
            language,
            "指定した資料ソースの登録と対応する独立索引を削除します。元の外部資料は保持します。",
            "Remove a source registration and its independent index. Original external documents are retained."
        ),
        OperationId::RagIndex => local!(
            language,
            "登録資料をkeywordまたはsemanticで索引化します。dry-runは実行対象を表示し、semantic処理はAPI費用が発生する場合があります。",
            "Index registered sources with keyword or semantic mode. dry-run shows targets; semantic processing may incur API costs."
        ),
        OperationId::RagIngest => local!(
            language,
            "外部抽出した本文を、元ファイル・種類・任意のメタデータと対応付けて登録します。",
            "Register externally extracted text with its source file, kind, and optional metadata."
        ),
        OperationId::RagStatus => local!(
            language,
            "独立RAGの登録ソース、索引状態と未索引・失敗情報を表示します。",
            "Show independent RAG sources, index state, and unindexed or failed items."
        ),
        OperationId::RagInspect => local!(
            language,
            "指定文書IDの保存された版、出典、抽出情報と索引情報を表示します。",
            "Show saved versions, provenance, extraction details, and index information for a document ID."
        ),
        OperationId::RagSearch => local!(
            language,
            "ガイド、会話、登録資料の明示した範囲を検索します。semantic・hybridではAPI費用が発生する場合があります。",
            "Search explicit scopes of guides, conversations, and registered sources. Semantic and hybrid modes may incur API costs."
        ),
        OperationId::RagGet => local!(
            language,
            "検索結果の版付きrefを使って、対応する原文を有限ページで取得します。",
            "Fetch corresponding original text in bounded pages using a versioned ref from search results."
        ),
    }
}

fn field_label_for(id: FieldId, language: crate::config::UiLanguage) -> &'static str {
    match id {
        FieldId::ShrinkRevert => local!(language, "元の索引へ戻す", "Switch back to the old index"),
        FieldId::ShrinkCancel => local!(
            language,
            "実行中の変換をやめる",
            "Stop the running conversion"
        ),
        FieldId::SettlementStates => local!(
            language,
            "会話の観測状態",
            "Conversation observation states"
        ),
        FieldId::Until => local!(language, "終了日時（未満）", "End time (exclusive)"),
        FieldId::After => local!(language, "前回の待機cursor", "Previous wait cursor"),
        FieldId::Memory => local!(language, "メモリ状態", "Memory status"),
        FieldId::CommandContains => local!(language, "コマンドを含む", "Command contains"),
        FieldId::ExitCode => local!(language, "終了コード", "Exit code"),
        FieldId::Thread => local!(language, "会話ID", "Thread ID"),
        FieldId::Run => local!(language, "作業ID（Run）", "Run ID"),
        FieldId::Attempt => local!(
            language,
            "モデル試行ID（単独指定）",
            "Model attempt ID (alone)"
        ),
        FieldId::Turn => local!(language, "応答処理ID（Turn）", "Turn ID"),
        FieldId::Node => local!(language, "履歴ノードID", "History node ID"),
        FieldId::Before => local!(language, "この履歴ノードより前", "Before this history node"),
        FieldId::Ordinal => local!(language, "採用経路の位置", "Selected-path position"),
        FieldId::ExpectNode => local!(
            language,
            "位置にあるはずのノード",
            "Node expected at the position"
        ),
        FieldId::ItemsBefore => local!(language, "基準より前の件数", "Items before the anchor"),
        FieldId::ItemsAfter => local!(language, "基準より後の件数", "Items after the anchor"),
        FieldId::Parent => local!(language, "親の会話ID", "Parent Thread ID"),
        FieldId::Message => local!(language, "入力ID", "Message ID"),
        FieldId::Messages => local!(language, "取消対象の入力ID", "Message IDs to cancel"),
        FieldId::Operation => local!(language, "受付照会のoperation ID", "Receipt operation ID"),
        FieldId::Execution => local!(language, "コマンド実行ID", "Command execution ID"),
        FieldId::Query => local!(language, "検索・絞込み文字列", "Search or filter text"),
        FieldId::GroupBy => local!(language, "使用量の内訳", "Usage breakdown"),
        FieldId::Timezone => local!(language, "内訳の時間帯", "Breakdown timezone"),
        FieldId::GroupLimit => local!(language, "内訳の区切り上限", "Breakdown group limit"),
        FieldId::Limit => local!(language, "取得件数", "Result count"),
        FieldId::Cursor => local!(language, "続きcursor", "Continuation cursor"),
        FieldId::Since => local!(language, "記録日時以降", "Recorded at or after"),
        FieldId::States => local!(language, "保存された状態", "Saved states"),
        FieldId::State => local!(
            language,
            "関連shellの保存状態",
            "Saved state of related shell"
        ),
        FieldId::All => local!(language, "終端状態も含める", "Include terminal states"),
        FieldId::IncludeChildren => local!(language, "子の使用量も含める", "Include child usage"),
        FieldId::Agents => local!(language, "指示ファイル", "Instruction file"),
        FieldId::Cwd => local!(language, "作業フォルダ", "Working folder"),
        FieldId::Connection => local!(language, "接続名", "Connection name"),
        FieldId::Model => local!(language, "モデル名", "Model name"),
        FieldId::Effort => local!(language, "推論設定", "Reasoning effort"),
        FieldId::AutoCompactAt => local!(language, "自動要約の閾値", "Auto-compaction threshold"),
        FieldId::SegmentChars => local!(language, "要約の部分の大きさ", "Summary part size"),
        FieldId::ReduceRounds => local!(
            language,
            "要約の統合の最大段数",
            "Summary maximum reduce rounds"
        ),
        FieldId::Title => local!(language, "表示タイトル", "Display title"),
        FieldId::Descendants => local!(language, "子孫も対象", "Include descendants"),
        FieldId::Archived => local!(language, "アーカイブの扱い", "Archived conversations"),
        FieldId::Tags => local!(language, "タグで絞り込み", "Filter by tags"),
        FieldId::AddTags => local!(language, "付けるタグ", "Tags to add"),
        FieldId::RemoveTags => local!(language, "外すタグ", "Tags to remove"),
        FieldId::SenderLabel => local!(language, "送信者の表示名", "Sender display name"),
        FieldId::ExpectReply => local!(language, "返信を依頼", "Expect a reply"),
        FieldId::Deliver => local!(language, "届けるとき", "Delivery timing"),
        FieldId::Text => local!(language, "入力本文", "Message text"),
        FieldId::Conditions => local!(language, "観測条件", "Observation conditions"),
        FieldId::Timeout => local!(
            language,
            "観測・実行の上限ms",
            "Observation or execution limit (ms)"
        ),
        FieldId::ExcerptChars => local!(language, "抜粋文字数", "Excerpt characters"),
        FieldId::LimitBytes => local!(language, "取得byte数", "Read size in bytes"),
        FieldId::EndMessageOnly => local!(language, "終了メッセージのみ", "End message only"),
        FieldId::Parts => local!(language, "要約partの一覧", "Summary parts"),
        FieldId::Encoding => local!(language, "文字コード", "Encoding"),
        FieldId::Relation => local!(language, "履歴の関係", "History relationship"),
        FieldId::Kinds => local!(language, "含めるnode種類", "Included node kinds"),
        FieldId::ExcludeKinds => local!(language, "除外node種類", "Excluded node kinds"),
        FieldId::From => local!(language, "開始日時", "Start time"),
        FieldId::To => local!(language, "終了日時", "End time"),
        FieldId::Path => local!(language, "参照するパス", "Path"),
        FieldId::AttachmentKind => local!(language, "添付の種類", "Attachment kind"),
        FieldId::Attachment => local!(language, "添付参照ID", "Attachment reference ID"),
        FieldId::CompactAction => {
            local!(language, "圧縮・編集方法", "Compaction or editing method")
        }
        FieldId::FromNode => local!(language, "範囲先頭node ID", "First node ID in range"),
        FieldId::ToNode => local!(language, "範囲末尾node ID", "Last node ID in range"),
        FieldId::SummaryMaxChars => local!(
            language,
            "要約文字数上限（空=設定値）",
            "Summary character limit (empty = configured)"
        ),
        FieldId::SummaryMaxPercent => local!(
            language,
            "要約割合上限（空=設定値）",
            "Summary percentage limit (empty = configured)"
        ),
        FieldId::SummarySegmentChars => local!(
            language,
            "部分の大きさ（空=設定値）",
            "Part size (empty = configured)"
        ),
        FieldId::SummaryReduceRounds => local!(
            language,
            "統合の最大段数（空=設定値）",
            "Maximum reduce rounds (empty = configured)"
        ),
        FieldId::IndexScope => local!(language, "索引対象範囲", "Index scope"),
        FieldId::Space => local!(language, "索引領域ID", "Index space ID"),
        FieldId::Threads => local!(language, "索引対象Thread ID", "Thread IDs to index"),
        FieldId::Sources => local!(
            language,
            "索引対象source node ID",
            "Source node IDs to index"
        ),
        FieldId::IndexKinds => local!(
            language,
            "追加する種類（空=設定の自動索引対象）",
            "Kinds to add (empty = configured automatic kinds)"
        ),
        FieldId::Dimensions => local!(
            language,
            "要求する埋め込み次元",
            "Requested embedding dimensions"
        ),
        FieldId::Category => local!(language, "保存カテゴリ", "Saved category"),
        FieldId::Schema => local!(
            language,
            "最終JSONのschemaファイル",
            "Final JSON schema file"
        ),
        FieldId::ExecResult => local!(language, "結果モード", "Result mode"),
        FieldId::MaxRepairs => local!(language, "追加修復の上限", "Maximum additional repairs"),
        FieldId::DocParent => local!(language, "ガイドの親ID", "Guide parent ID"),
        FieldId::Document => local!(language, "文書ID", "Document ID"),
        FieldId::Section => local!(language, "ガイド見出し", "Guide section"),
        FieldId::Source => local!(language, "登録資料ソース", "Registered source"),
        FieldId::SourceName => local!(language, "登録ソース名", "Registered source name"),
        FieldId::Excludes => local!(language, "除外するglob", "Excluded globs"),
        FieldId::ClearExcludes => local!(
            language,
            "既存の除外設定を消去",
            "Clear existing exclusions"
        ),
        FieldId::DryRun => local!(language, "実行対象だけを確認", "Preview targets only"),
        FieldId::ExcludeNode => local!(
            language,
            "このノードを含めず直前で止める",
            "Stop just before this node"
        ),
        FieldId::SearchMode => local!(language, "検索・索引方式", "Search or index mode"),
        FieldId::IngestKind => local!(language, "登録本文の種類", "Ingested content kind"),
        FieldId::SourceFile => local!(language, "元資料ファイル", "Source material file"),
        FieldId::TextFile => local!(language, "登録本文のファイル", "Text file to ingest"),
        FieldId::MetadataFile => local!(
            language,
            "補助metadataファイル",
            "Supplementary metadata file"
        ),
        FieldId::RagScopes => local!(language, "検索する資料領域", "Document spaces to search"),
        FieldId::Reference => local!(language, "原文の版付きref", "Versioned original ref"),
    }
}

fn field_help_for(id: FieldId, language: crate::config::UiLanguage) -> &'static str {
    match id {
        FieldId::ShrinkRevert => local!(
            language,
            "残してある変換前の索引へ切り替えます。変換後に増えた分は通常の索引作成へ戻します。",
            "Switch to the retained index from before the conversion. Segments indexed since go back through normal indexing."
        ),
        FieldId::ShrinkCancel => local!(
            language,
            "途中の結果を捨て、古い索引をそのまま使い続けます。",
            "Discard the partial result and keep using the old index."
        ),
        FieldId::SettlementStates => local!(
            language,
            "thread_settledのstatesを空白で区切ります。フォーム初期値はcompleted recovery_wait failed stoppedです。CLIではconditions-jsonのstates配列で明示します。",
            "Space-separated thread_settled states. The form defaults to completed recovery_wait failed stopped; CLI uses the states array in conditions-json."
        ),
        FieldId::Until => local!(
            language,
            "RFC3339の終了境界です。開始日時以上・終了日時未満で保存済みの値を選びます。",
            "RFC3339 end boundary. Saved values are selected from the start inclusive to the end exclusive."
        ),
        FieldId::After => local!(
            language,
            "前回waitのcursorを指定し、同じ条件で保存された後続の状態変更を観測します。",
            "Use the previous wait cursor to observe later saved changes under the same conditions."
        ),
        FieldId::Memory => local!(
            language,
            "保存済み作業の条件とは併用せず、ownerのcacheとOSの現在値を観測します。",
            "Observe current owner cache and OS values; do not combine with saved work conditions."
        ),
        FieldId::CommandContains => local!(
            language,
            "保存されたコマンドの部分文字列で絞ります。コマンド自体は実行しません。",
            "Filter by substring of the saved command. This does not execute it."
        ),
        FieldId::ExitCode => local!(
            language,
            "保存された終了コードで絞ります。負数も指定できます。",
            "Filter by saved exit code; negative values are allowed."
        ),
        FieldId::Thread => local!(
            language,
            "対象の保存Threadを指定します。CLI省略時は認証済みモデルの自分のThreadだけが既定です。",
            "Specify the saved target Thread. On CLI, an authenticated model defaults only to its own Thread."
        ),
        FieldId::Run => local!(
            language,
            "対象Runを指定します。人のCLIは明示IDが必要です。",
            "Specify the target Run. A human CLI must supply an ID."
        ),
        FieldId::Attempt => local!(
            language,
            "保存attemptの使用量とキャッシュ診断を取得します。他の集計指定は空にします。",
            "Retrieve saved attempt usage and cache diagnostics. Leave other grouping options empty."
        ),
        FieldId::Turn => local!(
            language,
            "一つの回答区間の保存Turn IDを指定します。Run IDとは別です。",
            "Specify the saved Turn ID for one response interval. This is distinct from a Run ID."
        ),
        FieldId::Node => local!(
            language,
            "原文または関係を調べる保存ノードIDを指定します。",
            "Specify the saved node ID whose original or relationships to inspect."
        ),
        FieldId::Ordinal => local!(
            language,
            "0から数える採用経路上の位置です。省略するとノード指定も含め末尾を基準にします。",
            "A zero-based position on the selected path. Omit it, and the node, to anchor at the end."
        ),
        FieldId::ExpectNode => local!(
            language,
            "位置指定と併用します。そこに別のノードがあれば、経路が変わったものとして失効を返します。",
            "Use with a position. If another node sits there, the read fails as stale because the path changed."
        ),
        FieldId::ItemsBefore => local!(
            language,
            "基準の前に読む件数です。前後と基準の合計は200件までで、CLI既定は20です。",
            "Items to read before the anchor. Before, after and the anchor total at most 200; CLI default is 20."
        ),
        FieldId::ItemsAfter => local!(
            language,
            "基準の後に読む件数です。前後と基準の合計は200件までで、CLI既定は20です。",
            "Items to read after the anchor. Before, after and the anchor total at most 200; CLI default is 20."
        ),
        FieldId::Before => local!(
            language,
            "選択パス上のノードIDです。これより前で採用済みの要約・spillの原文をこのThreadの保持から外します。",
            "A node ID on the selected path. Releases this Thread's hold on originals of summaries and spills adopted before it."
        ),
        FieldId::Parent => local!(
            language,
            "会話作成では新しい会話の親Threadです（省略時は親なし）。設定変更では付け替え先の親Threadで、空文字（フォームではnone）で親なしにします。実行中のRunの所有者は変わりません。",
            "On create: the parent Thread of the new conversation (omit for none). On set: the Thread to move this one under; an empty string (none in the form) detaches it. Live Runs keep their owner."
        ),
        FieldId::Message => local!(
            language,
            "message sendの受付結果に返されたmessage_idです。",
            "The message_id returned in the message send receipt."
        ),
        FieldId::Messages => local!(
            language,
            "取消す未採用message_idを明示します。CLIは--messageを繰り返し、最大128件です。",
            "Specify unadopted message_id values to cancel. CLI repeats --message; maximum 128."
        ),
        FieldId::Operation => local!(
            language,
            "受付結果またはエラーに返されたoperation_idです。照会自身のrequest IDとは別です。",
            "The operation_id returned by the receipt or error, distinct from the request ID of this lookup."
        ),
        FieldId::Execution => local!(
            language,
            "対象作業のshellツールが返したexecution_idを使います。観測CLI自身のexecution_idと区別します。",
            "Use the execution_id returned by the target work's shell tool, not the observing CLI's own ID."
        ),
        FieldId::Query => local!(
            language,
            "この操作の検索語または一覧の絞込み文字列です。",
            "Search terms or a list filter for this operation."
        ),
        FieldId::GroupBy => local!(
            language,
            "空は従来の合計。hour/day/thread は有界な保存済み内訳です。",
            "Empty gives the traditional total; hour/day/thread give bounded saved breakdowns."
        ),
        FieldId::Timezone => local!(
            language,
            "内訳指定時のみ。空は owner local、UTC または固定 +HH:MM/-HH:MM。",
            "Only with a breakdown. Empty uses owner local; otherwise UTC or a fixed +HH:MM/-HH:MM offset."
        ),
        FieldId::GroupLimit => local!(
            language,
            "内訳指定時のみ1..168。空は有効上限168で、CLI引数自体の既定値はありません。",
            "Only with a breakdown; 1..168. Empty uses effective limit 168; the CLI argument itself has no default."
        ),
        FieldId::Limit => local!(
            language,
            "CLIの件数既定を表示します。一覧のTUI表示既定20件は別に明示します。",
            "Shows the CLI count default. The separate TUI list display default is 20."
        ),
        FieldId::Cursor => local!(
            language,
            "直前の同じ対象・条件の読み取り結果cursorです。空は先頭です。",
            "The cursor from the immediately preceding read of the same target and conditions. Empty starts at the beginning."
        ),
        FieldId::Since => local!(
            language,
            "タイムゾーン付きRFC3339です。操作の保存時刻の意味に従います。",
            "RFC3339 with timezone. The meaning of the saved timestamp depends on the operation."
        ),
        FieldId::States => local!(
            language,
            "保存native stateを列挙します。CLIは--stateを繰り返します。",
            "List saved native states. CLI repeats --state."
        ),
        FieldId::State => local!(
            language,
            "履歴に関連したshellのstarting/running/stopping/exited/failed/stoppedを絞り込みます。",
            "Filter history-related shells by starting/running/stopping/exited/failed/stopped."
        ),
        FieldId::All => local!(
            language,
            "通常のpending範囲に加えて終端状態も表示します。",
            "Show terminal states as well as the usual pending range."
        ),
        FieldId::IncludeChildren => local!(
            language,
            "Thread範囲で子の使用量も合計します。Run/Turn指定とは同時に使えません。",
            "Include child usage in a Thread total. Cannot be combined with a Run or Turn."
        ),
        FieldId::Agents => local!(
            language,
            "対象の実効cwdから解決する指示ファイルです。継承・未指定の意味は作成／設定操作に従います。",
            "An instruction file resolved from the effective target cwd. Inheritance and omission follow the create or set operation."
        ),
        FieldId::Cwd => local!(
            language,
            "作成時の相対パスは起動場所、設定変更時は対象Threadの保存cwdから解決します。未指定はその操作の既定・継承です。一覧では字句的に整えた（`.`・`..`・末尾の`/`）保存cwdと一致するThreadだけに絞ります（CLIとフォームの相対パスは起動場所から解決し、wireは絶対パスのみ）。",
            "On creation, relative paths use the launch directory; on update, the target Thread's saved cwd. Omission follows the operation's defaults or inheritance. In a list it keeps only Threads whose saved cwd equals the path after lexical cleanup (`.`, `..`, trailing `/`); the CLI and form resolve relative paths against the launch directory, a wire client must send an absolute path."
        ),
        FieldId::Connection => local!(
            language,
            "設定ファイルに定義された接続名です。model listではThread接続、所有者既定の順で選びます。",
            "A connection defined in config. model list chooses the Thread connection and then the owner's default."
        ),
        FieldId::Model => local!(
            language,
            "接続から取得したモデル名を指定します。内蔵の価格・能力カタログはありません。",
            "Specify a model returned by the connection. No price or capability catalogue is built in."
        ),
        FieldId::Effort => local!(
            language,
            "未指定は既定・継承です。CLIの空文字は明示解除として扱います。",
            "Omission uses defaults or inheritance. An empty CLI value explicitly clears it."
        ),
        FieldId::AutoCompactAt => local!(
            language,
            "このThreadだけの自動要約の閾値（トークン）です。0で停止、CLIの空文字（フォームではnone）で接続または[compaction]の値へ戻します。",
            "Proactive compaction threshold in tokens for this Thread only. 0 turns it off; an empty CLI value returns to the connection value."
        ),
        FieldId::SegmentChars => local!(
            language,
            "このThreadだけの要約の部分の大きさ（元の会話の文字数、2000から1000000）です。CLIの空文字（フォームではnone）で接続または[compaction]の値へ戻します。",
            "Summary part size in original-conversation characters (2000 to 1000000) for this Thread only. An empty CLI value returns to the connection value."
        ),
        FieldId::ReduceRounds => local!(
            language,
            "このThreadだけの要約の統合の最大段数（1から16）です。CLIの空文字（フォームではnone）で接続または[compaction]の値へ戻します。",
            "Maximum summary reduce rounds (1 to 16) for this Thread only. An empty CLI value returns to the connection value."
        ),
        FieldId::Title => local!(
            language,
            "保存タイトルを変更します。空文字は自動タイトルへ戻します。",
            "Change the saved title. An empty value restores automatic naming."
        ),
        FieldId::Descendants => local!(
            language,
            "親子関係上の子孫Threadも削除／削除影響確認／アーカイブの対象へ含めます。",
            "Include descendant Threads in deletion, its impact preview, or archiving."
        ),
        FieldId::Archived => local!(
            language,
            "空は既定（アーカイブ済みを隠す）。includeは両方、onlyはアーカイブ済みだけを一覧します。",
            "Empty is the default (archived are hidden). include lists both; only lists archived conversations."
        ),
        FieldId::Tags => local!(
            language,
            "指定したタグをすべて持つThreadだけを一覧します（AND）。CLIは--tagを繰り返し、フォームはコンマ区切りです（フォームではコンマを含むタグは指定できません）。最初のタグの所属Threadから読むため、最も絞れるタグを先に指定します。",
            "List only Threads that carry every given tag (AND). The CLI repeats --tag; the form takes a comma-separated list (a tag containing a comma cannot be entered in the form). The page is read from the first tag's members, so name the narrowest tag first."
        ),
        FieldId::AddTags => local!(
            language,
            "対象Threadへ付けるタグです（最大64個、1つは128文字以内の1行）。CLIは--addを繰り返し、フォームはコンマ区切りです。",
            "Tags to add to the target Thread (at most 64; each one line of at most 128 characters). The CLI repeats --add; the form takes a comma-separated list."
        ),
        FieldId::RemoveTags => local!(
            language,
            "対象Threadから外すタグです（最大64個）。付けるタグと同じ値は指定できません。CLIは--removeを繰り返し、フォームはコンマ区切りです。",
            "Tags to remove from the target Thread (at most 64). A value cannot be in both lists. The CLI repeats --remove; the form takes a comma-separated list."
        ),
        FieldId::SenderLabel => local!(
            language,
            "自己申告の表示名であり権限を与えません。message sendは最大64Unicode文字です。",
            "A self-declared label; it grants no authority. message send allows at most 64 Unicode characters."
        ),
        FieldId::ExpectReply => local!(
            language,
            "指定メッセージへの返信を依頼します。",
            "Request an explicit reply to this message."
        ),
        FieldId::Deliver => local!(
            language,
            "空は次の安全な区切り。nowは前景shell待ちも解除します（応答生成は割込みません）。completeは動いているRunが完了するときに届け、動いていなければすぐ届けます。",
            "Empty: the next safe boundary. now: also release a foreground shell wait (never interrupts response generation). complete: deliver when the live Run would complete, or immediately if no Run is live."
        ),
        FieldId::Text => local!(
            language,
            "TUIでは保持した本文を使います。CLIの--text-stdinをTUIの端末stdinへ接続しません。",
            "The TUI uses the retained text. CLI --text-stdin is not connected to terminal stdin in the TUI."
        ),
        FieldId::Conditions => local!(
            language,
            "turn_completedはTurn ID、run_endedはRun ID、shell_endedとshell_outputは実行ID、thread_settledはThread IDの一覧で観測します。種類を変えたら対応するIDを明示します。出力条件はshellのcursor、再開は前回waitのafterを別々に指定します。",
            "Use Turn IDs for turn_completed, Run IDs for run_ended, execution IDs for shell_ended and shell_output, and Thread IDs for thread_settled. Specify matching IDs when changing kind. Set the shell cursor for output and previous wait after for resumption separately."
        ),
        FieldId::Timeout => local!(
            language,
            "期限は観測クライアントの上限です。期限切れだけで対象Runやshellは停止しません。",
            "The observing client's deadline. Expiry alone does not stop the target Run or shell."
        ),
        FieldId::ExcerptChars => local!(
            language,
            "0..4096Unicode scalarです。0は抜粋本文を省略し、CLI既定は512です。",
            "0..4096 Unicode scalars. Zero omits excerpt text; CLI default is 512."
        ),
        FieldId::LimitBytes => local!(
            language,
            "原文の一回の取得上限です。CLI既定と操作ごとの上限に従います。",
            "Maximum original text fetched in one request, subject to CLI defaults and per-operation limits."
        ),
        FieldId::EndMessageOnly => local!(
            language,
            "保存された最終assistant messageだけを取得し、推論と途中説明を除きます。",
            "Fetch only the saved final assistant message, excluding reasoning and interim explanations."
        ),
        FieldId::Parts => local!(
            language,
            "要約ノードの保存済みpart(map、reduce)を、範囲・段・費用・本文つきで取得します。",
            "Fetch a summary node's saved parts (map, reduce) with range, stage, usage and bodies."
        ),
        FieldId::Encoding => local!(
            language,
            "shell readの省略は保存値です。継続読み取りは同じ文字コードと対象を保持します。",
            "An omitted shell read encoding uses the saved value. Continue reading with the same encoding and target."
        ),
        FieldId::Relation => local!(
            language,
            "previous/next/sources/replacementsを指定します。前後と出典は別の関係です。",
            "Choose previous/next/sources/replacements. Neighbours and sources are distinct relationships."
        ),
        FieldId::Kinds => local!(
            language,
            "検索へ含める保存node kindを列挙します。CLIは--kindを繰り返します。",
            "List saved node kinds to include in search. CLI repeats --kind."
        ),
        FieldId::ExcludeKinds => local!(
            language,
            "検索から除く保存node kindを列挙します。CLIは--exclude-kindを繰り返します。",
            "List saved node kinds to exclude from search. CLI repeats --exclude-kind."
        ),
        FieldId::From => local!(
            language,
            "タイムゾーン付きRFC3339の開始境界を含めます。",
            "Inclusive start boundary in RFC3339 with timezone."
        ),
        FieldId::To => local!(
            language,
            "タイムゾーン付きRFC3339の終了境界を含めません。",
            "Exclusive end boundary in RFC3339 with timezone."
        ),
        FieldId::Path => local!(
            language,
            "指定操作で解決するファイル／フォルダです。既存外部資料は履歴のために複製しません。",
            "File or folder resolved by the operation. Existing external sources are not copied into history."
        ),
        FieldId::AttachmentKind => local!(
            language,
            "fileまたはimageを指定します。",
            "Specify file or image."
        ),
        FieldId::Attachment => local!(
            language,
            "attachment listが返したattachment_idを指定します。",
            "Specify the attachment_id returned by attachment list."
        ),
        FieldId::CompactAction => local!(
            language,
            "未指定は安全境界での圧縮要求です。maximal/spill/summary/trim/omit/restoreは排他的です。",
            "Omission requests compaction at a safe boundary. maximal/spill/summary/trim/omit/restore are mutually exclusive."
        ),
        FieldId::FromNode => local!(
            language,
            "編集範囲の先頭を含みます。restoreは選択済みmarkerまたは最初の省略元も指定できます。",
            "Includes the first node in the edit range. restore can also specify the selected marker or first omitted source."
        ),
        FieldId::ToNode => local!(
            language,
            "編集範囲の末尾を含みます。trimは削除するprefix末尾です。",
            "Includes the last node in the edit range. For trim, this is the last node in the removed prefix."
        ),
        FieldId::SummaryMaxChars => local!(
            language,
            "summaryだけで指定できます。完成した要約の文字数上限です。",
            "Only for summary. Maximum length of the completed summary."
        ),
        FieldId::SummaryMaxPercent => local!(
            language,
            "summaryだけで指定できます。元の文字数に対する割合で、0より大きく100以下です。",
            "Only for summary. Percentage of original characters, greater than zero and at most 100."
        ),
        FieldId::SummarySegmentChars => local!(
            language,
            "summaryだけで指定できます。この要約だけの、部分（モデル要求1回分）の元の文字数です。2000から1000000です。",
            "Only for summary. Original characters per part (one model request) for this summary only, 2000 to 1000000."
        ),
        FieldId::SummaryReduceRounds => local!(
            language,
            "summaryだけで指定できます。この要約だけの、統合の最大段数です。1から16です。",
            "Only for summary. Maximum reduce rounds for this summary only, 1 to 16."
        ),
        FieldId::IndexScope => local!(
            language,
            "threads/allを指定します。実行時は現在のpreviewと明示対象の規則を保持します。",
            "Specify threads or all. Execution retains current preview and explicit target rules."
        ),
        FieldId::Space => local!(
            language,
            "現在の有効領域または明示した保存space IDです。",
            "The current active space or an explicitly specified saved space ID."
        ),
        FieldId::Threads => local!(
            language,
            "過去履歴を索引化するThread IDを明示します。CLIは--threadを繰り返します。",
            "Specify Thread IDs for past history indexing. CLI repeats --thread."
        ),
        FieldId::Sources => local!(
            language,
            "索引元の保存node IDを指定します。CLIは--sourceを繰り返します。",
            "Specify saved source node IDs. CLI repeats --source."
        ),
        FieldId::IndexKinds => local!(
            language,
            "設定の自動索引対象に加えて索引化する種類です。CLIは--kindを繰り返します。",
            "Kinds to index in addition to the configured automatic kinds. CLI repeats --kind."
        ),
        FieldId::Dimensions => local!(
            language,
            "省略時は現在の設定／領域の契約に従います。モデル側が受け付ける次元を内蔵推測しません。",
            "Omission follows current configuration or space contract. Accepted model dimensions are not guessed."
        ),
        FieldId::Category => local!(
            language,
            "回収対象を選びます。保存状態の照会は選択に関係なく全体を表示します。",
            "Select a collection target. Saved-state inspection shows the whole state regardless of selection."
        ),
        FieldId::Schema => local!(
            language,
            "最大64KiBの対応JSON Schemaファイルです。未対応keywordは入力エラーです。",
            "A supported JSON Schema file up to 64 KiB. Unsupported keywords are input errors."
        ),
        FieldId::ExecResult => local!(
            language,
            "reply、turn-end、run-endから選択します。",
            "Choose reply, turn-end, or run-end."
        ),
        FieldId::MaxRepairs => local!(
            language,
            "0..5回、CLI既定2回です。各追加依頼はモデル・tool費用を生じ得ます。",
            "0..5; CLI default is 2. Each additional request can incur model and tool costs."
        ),
        FieldId::DocParent => local!(
            language,
            "階層を絞る同梱ガイドカテゴリIDです。",
            "Bundled guide category ID used to narrow the hierarchy."
        ),
        FieldId::Document => local!(
            language,
            "同梱ガイドまたは独立RAGの保存文書IDです。操作の文書領域を確認します。",
            "Saved document ID for bundled guides or independent RAG. Check the operation's document space."
        ),
        FieldId::Section => local!(
            language,
            "docs getが示す見出し・sectionを指定します。",
            "Specify a heading or section returned by docs get."
        ),
        FieldId::Source => local!(
            language,
            "登録ソースのIDまたは名前です。rag indexのdocsは同梱ガイドです。",
            "Registered source ID or name. docs in rag index denotes bundled guides."
        ),
        FieldId::SourceName => local!(
            language,
            "資料を識別する登録名です。",
            "The registered name identifying a source."
        ),
        FieldId::Excludes => local!(
            language,
            "ソース相対globを繰り返します。clear-excludesとは同時に使えません。",
            "Repeat source-relative globs. Cannot be combined with clear-excludes."
        ),
        FieldId::ClearExcludes => local!(
            language,
            "ソース再登録時の既存除外を明示解除します。excludeとの併用はできません。",
            "Explicitly clear exclusions when re-registering a source. Cannot be combined with exclude."
        ),
        FieldId::DryRun => local!(
            language,
            "操作を適用せず、対象や変更の見込みを確認します。",
            "Inspect intended targets and changes without applying the operation."
        ),
        FieldId::ExcludeNode => local!(
            language,
            "指定ノードを残さず、その直前のノードを新しい末尾にします。依頼を選んで、その依頼の前へ戻すときに使います。",
            "Do not keep the specified node; the node just before it becomes the new end. Use it to rewind to before a chosen request."
        ),
        FieldId::SearchMode => local!(
            language,
            "操作が対応するkeyword/semantic/hybridから選びます。semantic方式はAPIを使用し得ます。",
            "Choose keyword/semantic/hybrid supported by the operation. Semantic mode can use an API."
        ),
        FieldId::IngestKind => local!(
            language,
            "extracted/llm-explanation/summaryを指定し、抽出結果の由来を保持します。",
            "Specify extracted/llm-explanation/summary to retain provenance of extracted content."
        ),
        FieldId::SourceFile => local!(
            language,
            "抽出済み本文の元となった資料ファイルです。",
            "Original source file for the extracted text."
        ),
        FieldId::TextFile => local!(
            language,
            "--text-stdinと排他的なUTF-8本文ファイルです。既存の16MiB上限を保持します。",
            "UTF-8 text file mutually exclusive with --text-stdin. The existing 16 MiB limit applies."
        ),
        FieldId::MetadataFile => local!(
            language,
            "任意の登録metadataファイルです。読み取りと検証はworkerで行います。",
            "Optional registration metadata file. A worker reads and validates it."
        ),
        FieldId::RagScopes => local!(
            language,
            "docs/history/history:THREAD/source:IDまたは名前を繰り返します。",
            "Repeat docs/history/history:THREAD/source:ID or names."
        ),
        FieldId::Reference => local!(
            language,
            "rag searchが返したrefを指定します。cursorは同じrefへ束縛されます。",
            "Specify the ref returned by rag search. The cursor is bound to the same ref."
        ),
    }
}

fn discovery_for(id: Discovery, language: crate::config::UiLanguage) -> &'static str {
    match id {
        Discovery::None => local!(
            language,
            "操作の入力条件を確認してください。",
            "Review the operation's input requirements."
        ),

        Discovery::ThreadList => local!(
            language,
            "thread listまたは会話一覧でThread IDを確認できます。",
            "Find a Thread ID in thread list or the conversation list."
        ),

        Discovery::RunFromThread => local!(
            language,
            "thread getまたはstatusで対象のRun IDを確認できます。",
            "Find the target Run ID with thread get or status."
        ),

        Discovery::TurnFromRun => local!(
            language,
            "run getで対象のTurn IDを確認できます。",
            "Find the target Turn ID with run get."
        ),

        Discovery::HistoryList => local!(
            language,
            "history listで保存ノードIDと採用経路を確認できます。",
            "Find saved node IDs and the adopted path with history list."
        ),

        Discovery::MessageReceipt => local!(
            language,
            "message sendの返却message_idまたはキューの入力IDを使います。",
            "Use the message_id returned by message send or the input ID in the queue."
        ),

        Discovery::OperationReceipt => local!(
            language,
            "受付結果またはエラーのoperation_idを使います。",
            "Use the operation_id returned by the receipt or error."
        ),

        Discovery::ShellFromToolOrStatus => local!(
            language,
            "shellツールの返却execution_idまたはstatusの作業一覧から確認できます。",
            "Find the execution_id returned by the shell tool or in the status work list."
        ),

        Discovery::SameReadCursor => local!(
            language,
            "直前の同じ対象の読み取り結果にあるcursorを使います。空は先頭です。",
            "Use the cursor from the previous read of the same target. Empty starts at the beginning."
        ),

        Discovery::Status => local!(
            language,
            "statusで保存された作業と関連IDを確認できます。",
            "Inspect saved work and related IDs with status."
        ),

        Discovery::Filesystem => local!(
            language,
            "対象ファイル／フォルダのパスを確認してください。読み取りはworkerで行います。",
            "Check the target file or folder path. A worker performs the read."
        ),

        Discovery::ConnectionConfig => local!(
            language,
            "設定ファイルのconnections名を確認できます。apiはresponsesまたはchat_completionsです。",
            "Check connection names in config. api is responses or chat_completions."
        ),

        Discovery::ModelList => local!(
            language,
            "model listで接続が返す利用可能モデル名を確認できます。",
            "Find available model names returned by the connection with model list."
        ),

        Discovery::AttachmentList => local!(
            language,
            "attachment listでattachment_idを確認できます。",
            "Find attachment_id values with attachment list."
        ),

        Discovery::IndexStatus => local!(
            language,
            "index statusで領域ID、現在の設定と次元を確認できます。",
            "Inspect space ID, current settings, and dimensions with index status."
        ),

        Discovery::DocsList => local!(
            language,
            "docs listで同梱ガイドのIDと階層を確認できます。",
            "Find bundled guide IDs and hierarchy with docs list."
        ),

        Discovery::DocumentList => local!(
            language,
            "同梱ガイドはdocs list、独立資料はrag searchで文書IDを確認できます。",
            "Find bundled guide IDs with docs list and independent documents with rag search."
        ),

        Discovery::DocsSections => local!(
            language,
            "docs getの見出し一覧からsectionを確認できます。",
            "Find a section in the headings listed by docs get."
        ),

        Discovery::RagSourceList => local!(
            language,
            "rag source listで登録ソースのID・名前を確認できます。",
            "Find registered source IDs and names with rag source list."
        ),

        Discovery::RagSearch => local!(
            language,
            "rag searchが返す版付きrefを使います。",
            "Use the versioned ref returned by rag search."
        ),
    }
}

fn term_for(id: Term, language: crate::config::UiLanguage) -> &'static str {
    match id {
        Term::Run => local!(
            language,
            "作業（Run）は依頼を進める実行単位です。Turnの回答後も、所有するshellや子Runの終了まで続くことがあります。",
            "A Run is a unit of work progressing a request. It may continue after a Turn's response until owned shells and child Runs finish."
        ),

        Term::Turn => local!(
            language,
            "応答処理（Turn）は一つの回答へ進む区間です。複数のモデル要求やtool実行を含み、Runと別に完了・停止・復旧待ちになります。",
            "A Turn is one response interval. It may include multiple model requests and tools, and completes, stops, or awaits recovery separately from its Run."
        ),

        Term::Message => local!(
            language,
            "入力（Message）は保存された依頼です。受付、Run・Turnへの採用、回答完了は別の段階です。",
            "A Message is saved input. Receipt, Run and Turn assignment, and response completion are separate stages."
        ),

        Term::Execution => local!(
            language,
            "コマンド実行（Execution）はshellツールが開始した一つのOSプロセス群です。観測CLI自身のIDと対象作業IDを区別します。",
            "An Execution is one OS process group started by a shell tool. Distinguish the observing CLI's ID from the target work's ID."
        ),
    }
}

pub(super) fn text_for(value: Text, language: crate::config::UiLanguage) -> &'static str {
    use crate::operations::{EnterVerb, TextKey};
    let rendered = match value.key {
        TextKey::Label(id) => operation_label_for(id, language),
        TextKey::Meaning(id) => operation_meaning_for(id, language),
        TextKey::FieldLabel(_, field) => field_label_for(field, language),
        TextKey::FieldHelp(_, field) => field_help_for(field, language),
        TextKey::Effect(Effect::Inspect) => local!(
            language,
            "保存された状態や原文を表示します。",
            "Show saved state or original text."
        ),
        TextKey::Effect(Effect::SavedMutation) => local!(
            language,
            "保存データや設定を変更します。",
            "Change saved data or settings."
        ),
        TextKey::Effect(Effect::RuntimeControl) => local!(
            language,
            "モデル・shell・索引などの処理を開始、再開、制御します。",
            "Start, resume, or control model, shell, indexing, or other processing."
        ),
        TextKey::EffectMarker(Effect::Inspect) => local!(language, "", ""),
        TextKey::EffectMarker(Effect::SavedMutation) => local!(language, "[変更] ", "[Change] "),
        TextKey::EffectMarker(Effect::RuntimeControl) => local!(language, "[制御] ", "[Control] "),
        TextKey::Enter(EnterVerb::Display) => local!(language, "表示", "Show"),
        TextKey::Enter(EnterVerb::Execute) => local!(language, "実行", "Run"),
        TextKey::Enter(EnterVerb::Send) => local!(language, "送信", "Send"),
        TextKey::Term(term) => term_for(term, language),
        TextKey::Discovery(discovery) => discovery_for(discovery, language),
    };
    debug_assert!(language != crate::config::UiLanguage::Ja || rendered == value.fallback);
    rendered
}

fn global_argument_text(
    argument: &crate::operations::GlobalArgument,
    language: crate::config::UiLanguage,
) -> String {
    let rendered = match argument.spelling {
        "--data-dir" => Some(local!(
            language,
            "--data-dir | 所有者のデータパス: CLI指定、環境変数、TOML、OS既定の優先順です。TUIの所有者操作は現在の所有者へ固定されます。",
            "--data-dir | Owner data path: CLI, environment, TOML, then OS default. TUI owner operations use the running owner."
        )),
        "--config" => Some(local!(
            language,
            "--config | 設定ファイル: CLI指定がOS既定より優先されます。config checkはそのファイルの既定接続を確認し、実行中の所有者設定は再起動時に反映します。",
            "--config | Configuration file: CLI takes precedence over the OS default. config check inspects that file's default connection; owner settings change on restart."
        )),
        "--operation-id" => Some(local!(
            language,
            "--operation-id | この要求のoperation ID: 所有者操作の再送または照会要求の識別子です。execと独立config/docs/RAGでは使用できません。operation getの照会対象は--operationです。",
            "--operation-id | This request's operation ID: Identifies an owner-operation resend or lookup. Not available for exec or standalone config/docs/RAG; operation get uses --operation for its target."
        )),
        "--db" => Some(local!(
            language,
            "--db | 独立RAGデータベース: 明示パス、未指定は解決済みDATA_DIR/rag.dbです。所有者の会話DBとは別です。",
            "--db | Independent RAG database: An explicit path, or resolved DATA_DIR/rag.db when omitted. This is separate from the owner's conversation database."
        )),
        _ => None,
    };
    match rendered {
        Some(message) => crate::ui::text_width::glyphs::decorate(message),
        None => {
            let sep = crate::ui::text_width::glyphs::separator();
            match language {
                crate::config::UiLanguage::Ja => format!(
                    "{}{sep}{}: {}",
                    argument.spelling, argument.label, argument.meaning
                ),
                crate::config::UiLanguage::En => {
                    format!("{}{sep}See CLI help for this option", argument.spelling)
                }
            }
        }
    }
}

#[cfg(test)]
mod paired_message_tests {
    use super::*;
    use crate::config::UiLanguage;

    #[test]
    fn catalogue_display_preserves_separator_when_terminal_measures_one_cell() {
        for (observed, separator) in [(Some(1), " · "), (Some(2), " | "), (None, " | ")] {
            crate::ui::text_width::set_terminal_width(observed);
            for language in [UiLanguage::Ja, UiLanguage::En] {
                let label = display_label(OperationId::ConfigInit, language);
                assert!(
                    label.contains(separator),
                    "{observed:?} {language:?}: {label}"
                );
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn global_argument_messages_preserve_cli_spelling_and_japanese_metadata() {
        for argument in crate::operations::GLOBAL_ARGUMENTS
            .iter()
            .chain(std::iter::once(&crate::operations::RAG_DB_ARGUMENT))
        {
            assert_eq!(
                global_argument_text(argument, UiLanguage::Ja),
                format!(
                    "{} | {}: {}",
                    argument.spelling, argument.label, argument.meaning
                )
            );
            let en = global_argument_text(argument, UiLanguage::En);
            assert!(en.starts_with(argument.spelling), "{en}");
            assert!(en.is_ascii(), "{en}");
        }
    }

    #[test]
    fn japanese_catalogue_fallbacks_remain_exact_and_english_is_defined() {
        fn verify(text: Text) {
            assert_eq!(
                text_for(text, UiLanguage::Ja),
                text.fallback,
                "{:?}",
                text.key
            );
            let en = text_for(text, UiLanguage::En);
            assert!(
                en.chars().all(|ch| ch.is_ascii()
                    || (matches!(text.key, crate::operations::TextKey::Label(_)) && ch == '·')),
                "{:?}: {en}",
                text.key
            );
        }
        for &id in OperationId::ALL {
            verify(id.label_text());
            verify(id.meaning_text());
            let spec = id.spec();
            verify(spec.effect.text());
            verify(spec.effect.list_marker());
            verify(id.enter_verb(spec.effect).text());
            for &field in spec.fields {
                let field = spec.field(field).expect("catalogue field");
                verify(field.field.label_text(id));
                verify(field.field.help_text(id));
                verify(field.field.discovery.text());
            }
        }
        for term in [Term::Run, Term::Turn, Term::Message, Term::Execution] {
            verify(term.text());
        }
    }
}
