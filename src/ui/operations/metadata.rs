//! Operation-specific help; typed builders and actual field values stay local.
use super::*;
use crate::operations::{Discovery, FieldType, Term, Text};

pub(super) fn text(value: Text) -> &'static str {
    // This is the single fallback-text boundary for these typed message keys.
    // The key remains the cache/localization identity when #35 adds a resolver.
    match value.key {
        crate::operations::TextKey::Label(_)
        | crate::operations::TextKey::Meaning(_)
        | crate::operations::TextKey::FieldLabel(_, _)
        | crate::operations::TextKey::FieldHelp(_, _)
        | crate::operations::TextKey::Effect(_)
        | crate::operations::TextKey::EffectMarker(_)
        | crate::operations::TextKey::Enter(_)
        | crate::operations::TextKey::Term(_)
        | crate::operations::TextKey::Discovery(_) => value.fallback,
    }
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
    pub(super) fn verb(&self) -> &'static str {
        text(self.operation.catalogue().enter_verb(self.effect()).text())
    }
    fn metadata_id(&self, name: &str) -> Option<FieldId> {
        Some(match name {
            "thread" => FieldId::Thread,
            "run" => FieldId::Run,
            "turn" => FieldId::Turn,
            "attempt" => FieldId::Attempt,
            "node" => FieldId::Node,
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
            "title" | "title_mode" => FieldId::Title,
            "descendants" => FieldId::Descendants,
            "sender_label" => FieldId::SenderLabel,
            "now" => FieldId::Now,
            "text" => FieldId::Text,
            "condition" | "target" => FieldId::Conditions,
            "settled_states" => FieldId::SettlementStates,
            "timeout" => FieldId::Timeout,
            "excerpt_chars" => FieldId::ExcerptChars,
            "final_answer_only" => FieldId::FinalAnswerOnly,
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
            "scope" => FieldId::IndexScope,
            "space" => FieldId::Space,
            "threads" => FieldId::Threads,
            "sources" => FieldId::Sources,
            "dimensions" => FieldId::Dimensions,
            "category" => FieldId::Category,
            "schema" => FieldId::Schema,
            "max_repairs" => FieldId::MaxRepairs,
            "doc_parent" => FieldId::DocParent,
            "document" => FieldId::Document,
            "section" => FieldId::Section,
            "source" => FieldId::Source,
            "source_name" => FieldId::SourceName,
            "excludes" => FieldId::Excludes,
            "clear_excludes" => FieldId::ClearExcludes,
            "dry_run" => FieldId::DryRun,
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
            return "フォームを開いた参照先（編集値は自動変更しません）";
        }
        match origin {
            DefaultOrigin::None => "未指定",
            DefaultOrigin::Required => "明示指定が必要",
            DefaultOrigin::Literal => "この操作の既定値",
            DefaultOrigin::SelectedThread => "フォームを開いた会話",
            DefaultOrigin::SelectedRun => "フォームを開いたRun",
            DefaultOrigin::SelectedTurn => "フォームを開いたTurn",
            DefaultOrigin::FocusedNode => "フォーカスした履歴ノード",
            DefaultOrigin::FocusedExecution => "フォーカスしたExecution",
            DefaultOrigin::ReferencedMessage => "参照した入力ID",
            DefaultOrigin::ReferencedOperation => "参照した受付ID",
            DefaultOrigin::TargetThreadSettings => "対象Threadの保存設定",
            DefaultOrigin::ThreadThenOwnerConfig => "対象Thread、所有者の既定設定の順",
            DefaultOrigin::ProcessCwd => "TUIの起動場所",
            DefaultOrigin::ActiveIndexSpace => "現在の有効索引領域",
            DefaultOrigin::ConfiguredEmbedding => "現在の埋め込み設定",
            DefaultOrigin::ParentOrOwnerDefaults => "親Threadまたは所有者の既定設定",
            DefaultOrigin::SavedShellEncoding => "対象Executionに保存された文字コード",
        }
    }
    pub(super) fn field_hint(&self) -> String {
        let Some(field) = self.fields.get(self.selected) else {
            return String::new();
        };
        if field.name == "db" {
            return "空欄は起動時data-dir/rag.db | 相対パスは起動場所が基準 | F1で説明".into();
        }
        if field.name == "input_mode" {
            return "fileは本文ファイル、inlineは本文入力。CLI stdin相当でも端末stdinは読みません | F1で説明".into();
        }
        let Some(metadata) = self.selected_metadata() else {
            return "F1で入力条件を確認できます".into();
        };
        if field.value != field.initial_value {
            return "編集した値 | F1で入力条件とIDの探し方".into();
        }
        let discovery = metadata.field.discovery;
        if field.value.is_empty() && discovery != Discovery::None {
            return format!("未入力 | {}", text(discovery.text()));
        }
        let origin = if !field.initial_value.is_empty()
            && matches!(field.kind, Kind::Choice(_) | Kind::Boolean | Kind::Number)
        {
            "このフォームの初期選択"
        } else {
            self.origin(metadata.default_origin)
        };
        format!("初期値: {origin} | F1で入力条件とIDの探し方")
    }
    pub(super) fn help_lines(&self) -> Vec<Line<'static>> {
        let id = self.operation.catalogue();
        let spec = id.spec();
        let mut lines = vec![
            Line::from(text(id.meaning_text())),
            Line::from(text(self.effect().text())),
            Line::from(self.operation.cli()),
            Line::from(""),
        ];
        if self
            .fields
            .get(self.selected)
            .is_some_and(|field| field.name == "db")
        {
            let argument = crate::operations::RAG_DB_ARGUMENT;
            lines.push(Line::from(format!(
                "{} | {}",
                argument.label, argument.meaning
            )));
            lines.push(Line::from("空欄は起動時data-dir/rag.db。相対パスはTUI起動場所が基準です。会話履歴DBとは別です。"));
        }
        if let Some(field) = self.selected_metadata() {
            lines.push(Line::from(format!(
                "選択項目: {}",
                text(field.field.label_text(id))
            )));
            lines.push(Line::from(text(field.field.help_text(id))));
            if let Some(input) = self.fields.get(self.selected)
                && let Kind::Choice(choices) = input.kind
            {
                lines.push(Line::from(format!(
                    "このフォームの選択値: {}",
                    choices.join(" / ")
                )));
            }
            lines.push(Line::from(format!(
                "初期値の出所: {}",
                self.origin(field.default_origin)
            )));
            if let Some(input) = self.fields.get(self.selected)
                && !matches!(input.kind, Kind::Multiline)
            {
                let initial = input.initial_value.chars().take(80).collect::<String>();
                lines.push(Line::from(format!(
                    "このフォームを開いたときの値: {}",
                    if initial.is_empty() {
                        "未指定"
                    } else {
                        &initial
                    }
                )));
            }
            if let Some(value) = field.tui_default {
                lines.push(Line::from(format!("TUI既定: {value}")));
            }
            if let Some(value) = field.cli_default {
                lines.push(Line::from(format!("CLI既定: {value}")));
            }
            if !field.choices.is_empty() {
                lines.push(Line::from(format!("選択値: {}", field.choices.join(" / "))));
            }
            let kind = match field.field.kind {
                FieldType::Id(kind) | FieldType::Ids(kind) => match kind {
                    crate::operations::IdKind::Thread => "Thread ID（会話）",
                    crate::operations::IdKind::Run => "Run ID（作業）",
                    crate::operations::IdKind::Turn => "Turn ID（応答処理）",
                    crate::operations::IdKind::Node => "Node ID（履歴ノード）",
                    crate::operations::IdKind::Message => "Message ID（保存入力）",
                    crate::operations::IdKind::Operation => "Operation ID（受付要求）",
                    crate::operations::IdKind::Execution => "Execution ID（OSコマンド実行）",
                    crate::operations::IdKind::Attachment => "Attachment ID（添付参照）",
                    crate::operations::IdKind::Space => "Space ID（索引領域）",
                },
                FieldType::Text => "文字列",
                FieldType::TextList => "文字列の一覧",
                FieldType::Multiline => "複数行の本文",
                FieldType::Path => "ファイル／フォルダのパス",
                FieldType::Signed => "整数（負数を含みます）",
                FieldType::Unsigned => "0以上の整数（操作の上限に従います）",
                FieldType::Decimal => "有限の数値（操作ごとの範囲に従います）",
                FieldType::Boolean => "はい／いいえ",
                FieldType::Choice => "候補から選択",
                FieldType::Conditions => "種類と対象IDを組み合わせた観測条件",
                FieldType::Timestamp => "タイムゾーン付きRFC3339日時",
                FieldType::Cursor => "同じ読み取り対象の続きcursor",
            };
            lines.push(Line::from(kind));
            lines.push(Line::from(text(field.field.discovery.text())));
        }
        lines.push(Line::from(""));
        if matches!(self.build, FormBuilder::Client(_)) {
            lines.push(Line::from("このフォームの入力合計は1MiBまでです。ファイル入力の上限は各操作の既存規則に従います。"));
            lines.push(Line::from(
                "相対パスはTUI起動場所が基準です。現在のThread cwdで暗黙に解決しません。",
            ));
            lines.push(Line::from("config/data-dirは起動時の選択を使用します。設定の変更は現在の所有者へ即時反映しません。"));
            if self.operation == OpKind::Exec {
                lines.push(Line::from("依頼本文は所有する入力欄から渡します。CLIの--text-stdin相当でも端末stdinは読みません。Schemaと依頼の合計は128KiBまでです。"));
            }
            if self.operation == OpKind::RagIngest {
                lines.push(Line::from("本文入力元fileはtext-file、inlineは所有する本文入力です。切替時はもう一方の本文／ファイル欄を空にします。端末stdinは読みません。"));
            }
        }
        lines.push(Line::from("CLIの入力項目"));
        for argument in &id.primary_cli().arguments {
            let suffix = if argument.repeat {
                "（繰り返し指定）"
            } else if argument.takes_value {
                " 値"
            } else {
                "（フラグ）"
            };
            let default = argument
                .default
                .as_deref()
                .map(|value| format!(" | 既定 {value}"))
                .unwrap_or_default();
            lines.push(Line::from(format!(
                "{}{suffix}{default}",
                argument.spelling
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(
            "所有者・設定のパスは、このTUIを起動したときの選択に固定されています。",
        ));
        for argument in crate::operations::GLOBAL_ARGUMENTS {
            lines.push(Line::from(format!(
                "{} | {}: {}",
                argument.spelling, argument.label, argument.meaning
            )));
        }
        // Standalone forms use this same help projection once G3 supplies their
        // typed invocation. No unsupported action is advertised in the G2 list.
        if spec.route == crate::operations::Route::Rag {
            let argument = crate::operations::RAG_DB_ARGUMENT;
            lines.push(Line::from(format!(
                "{} | {}: {}",
                argument.spelling, argument.label, argument.meaning
            )));
        }
        let request_help = match id.request_id_policy() {
            crate::operations::RequestIdPolicy::OwnerOperation => {
                "この要求の受付IDはTUIが追跡します。参照する対象IDは入力欄で指定します。"
            }
            crate::operations::RequestIdPolicy::StandaloneUnavailable => {
                "この独立操作では単一のoperation-id指定は使用しません。"
            }
            crate::operations::RequestIdPolicy::ComposedUnavailable => {
                "複数の要求を個別に追跡するため、単一のoperation-id指定は使用しません。"
            }
        };
        lines.push(Line::from(request_help));
        let confirmation = match spec.confirmation {
            crate::operations::Confirmation::None => "Enterで指定した操作を要求します。",
            crate::operations::Confirmation::ThreadDeletePreview => {
                "削除対象と実行状態を確認してから削除します。"
            }
            crate::operations::Confirmation::IndexCostPreview => {
                "対象件数と費用previewを確認してから索引を開始します。"
            }
            crate::operations::Confirmation::PaidRequest => {
                "モデルAPIの利用や追加処理の費用が発生する場合があります。"
            }
            crate::operations::Confirmation::StorageCollect => {
                "指定した保存データの種類について回収を要求します。保護された参照は既存の回収規則に従います。"
            }
            crate::operations::Confirmation::ContextEditPreview => {
                "編集する保存範囲と保護された作業を確認します。"
            }
        };
        lines.push(Line::from(confirmation));
        for binding in crate::operations::CLI_BINDINGS
            .iter()
            .filter(|binding| binding.operation == id && binding.alias_of.is_some())
        {
            lines.push(Line::from(format!(
                "同じ操作の短縮形: shellboy {}",
                binding.path.join(" ")
            )));
        }
        lines.push(Line::from(""));
        for term in [Term::Run, Term::Turn, Term::Message, Term::Execution] {
            lines.push(Line::from(text(term.text())));
        }
        lines
    }
}
fn operation_label_en(operation: OperationId) -> &'static str {
    match operation {
        OperationId::Status => "Work - Status",
        OperationId::UsageGet => "Usage - Total",
        OperationId::ModelList => "Models - List",
        OperationId::ThreadList => "Conversations - List",
        OperationId::ThreadGet => "Conversation - Details",
        OperationId::ThreadCreate => "Conversation - Create",
        OperationId::ThreadFork => "Conversation - Fork",
        OperationId::ThreadDelete => "Conversation - Delete",
        OperationId::ThreadDeletePreview => "Conversation - Deletion impact",
        OperationId::ThreadSet => "Conversation - Settings",
        OperationId::MessageSend => "Message - Send",
        OperationId::MessageGet => "Message - Assignment and response",
        OperationId::MessageCancel => "Message - Cancel unadopted input",
        OperationId::OperationGet => "Operation - Receipt",
        OperationId::RunGet => "Run - Details",
        OperationId::RunRetry => "Run - Retry recovery",
        OperationId::RunStop => "Run - Stop",
        OperationId::RevisitSet => "Next instruction - Set",
        OperationId::RevisitGet => "Next instruction - Inspect",
        OperationId::RevisitCancel => "Next instruction - Cancel",
        OperationId::TurnGet => "Turn - Details",
        OperationId::Wait => "Wait - Observe conditions",
        OperationId::HistoryList => "History - List",
        OperationId::HistoryGet => "History - Original",
        OperationId::HistorySearch => "History - Search",
        OperationId::HistoryRelations => "History - Relationships",
        OperationId::ShellList => "Shell - List",
        OperationId::ShellGet => "Shell - Details",
        OperationId::ShellRead => "Shell - Saved output",
        OperationId::ShellWrite => "Shell - Input",
        OperationId::ShellEof => "Shell - Close input",
        OperationId::ShellStop => "Shell - Stop",
        OperationId::AttachmentAdd => "Attachment - Add",
        OperationId::AttachmentList => "Attachment - List",
        OperationId::AttachmentRemove => "Attachment - Remove",
        OperationId::CompactList => "Compaction - Saved history",
        OperationId::Compact => "History - Compact and edit",
        OperationId::IndexStatus => "Index - Status",
        OperationId::IndexPreview => "Index - Cost preview",
        OperationId::IndexBackfill => "Index - Backfill history",
        OperationId::IndexRetry => "Index - Retry failures",
        OperationId::IndexRebuild => "Index - Rebuild",
        OperationId::IndexProbe => "Index - Probe connection and dimensions",
        OperationId::StorageStatus => "Storage - Status",
        OperationId::StorageCollect => "Storage - Collect",
        OperationId::Exec => "Work - Execute with JSON contract",
        OperationId::ConfigInit => "Config - Create template",
        OperationId::ConfigCheck => "Config - Check connection",
        OperationId::DocsList => "Guides - List",
        OperationId::DocsGet => "Guides - Text",
        OperationId::DocsSearch => "Guides - Search",
        OperationId::RagInit => "RAG - Initialize database",
        OperationId::RagSourceAdd => "RAG - Register source",
        OperationId::RagSourceList => "RAG - List sources",
        OperationId::RagSourceRemove => "RAG - Remove source",
        OperationId::RagIndex => "RAG - Index sources",
        OperationId::RagIngest => "RAG - Ingest extracted text",
        OperationId::RagStatus => "RAG - Status",
        OperationId::RagInspect => "RAG - Document details",
        OperationId::RagSearch => "RAG - Search",
        OperationId::RagGet => "RAG - Original",
    }
}
fn operation_meaning_en(operation: OperationId) -> &'static str {
    match operation {
        OperationId::Status => {
            "Show saved Run, Turn, shell, and input states and related IDs within the specified scope."
        }
        OperationId::UsageGet => {
            "Show saved token usage and processing time for the specified Thread, Run, or Turn."
        }
        OperationId::ModelList => {
            "Fetch models from the API using the specified connection, target Thread connection, or owner default, in that order."
        }
        OperationId::ThreadList => {
            "Filter saved conversation titles, parent-child relationships, and current states."
        }
        OperationId::ThreadGet => {
            "Show the specified Thread's settings, current Run and Turn, owned work, and recovery reasons."
        }
        OperationId::ThreadCreate => {
            "Create a saved Thread. Model processing starts with a separate message."
        }
        OperationId::ThreadFork => {
            "Create a new Thread at the specified history node, retaining provenance."
        }
        OperationId::ThreadDelete => {
            "Delete the specified Thread and selected descendants after rechecking running work and references."
        }
        OperationId::ThreadDeletePreview => {
            "Show work and references blocking deletion and expected reclamation for the specified Thread and selected descendants."
        }
        OperationId::ThreadSet => {
            "Change a Thread's display title, working folder, connection, model, reasoning effort, and instruction file."
        }
        OperationId::MessageSend => {
            "Save input to the specified Thread. Receipt, Run and Turn assignment, and response completion are tracked separately."
        }
        OperationId::MessageGet => {
            "Show the saved message receipt state, immutable Run and Turn assignment, and response references."
        }
        OperationId::MessageCancel => {
            "Cancel explicitly specified unadopted input. Input already adopted into history is not eligible."
        }
        OperationId::OperationGet => {
            "Inspect the saved receipt and subsequent work for an operation_id to assess an uncertain resend."
        }
        OperationId::RunGet => {
            "Show the Run's saved state, current Turn, ownership, child work and shells, and saved recovery reason."
        }
        OperationId::RunRetry => {
            "Resume a recovery-waiting Turn on the specified Run after checking its cause. A finished Run cannot resume."
        }
        OperationId::RunStop => {
            "Request that the specified Run and its owned work stop. Check saved state for actual completion."
        }
        OperationId::RevisitSet => {
            "Replace the instruction to run when the Run completes normally. Stop and shutdown take precedence."
        }
        OperationId::RevisitGet => {
            "Show the next instruction saved for a Run, who set it, execution count, and last execution time."
        }
        OperationId::RevisitCancel => {
            "Cancel the pending instruction to run on normal Run completion. The Run itself continues."
        }
        OperationId::TurnGet => {
            "Show the saved state and response node ID of one Turn. The response body is available from the original history."
        }
        OperationId::Wait => {
            "Observe until any specified condition is met. Target work continues after the observation deadline expires."
        }
        OperationId::HistoryList => {
            "Show a Thread's adopted path filtered by source Run, time, and related shell state."
        }
        OperationId::HistoryGet => {
            "Fetch a node's saved original and provenance. Final-answer-only excludes reasoning and interim explanations."
        }
        OperationId::HistorySearch => {
            "Search conversation history in the specified scope with embeddings and return original and provenance references. API costs may apply."
        }
        OperationId::HistoryRelations => {
            "Show previous, next, source, and replacement relationships, distinguishing the immutable DAG from the adopted path."
        }
        OperationId::ShellList => {
            "List saved shell executions filtered by conversation, time, and state. This does not read output bodies."
        }
        OperationId::ShellGet => {
            "Show the target command's original command and cwd, Run and Turn, saved state, and output completion."
        }
        OperationId::ShellRead => {
            "Read saved stdout and stderr of the specified Execution using an encoding and cursor."
        }
        OperationId::ShellWrite => {
            "Send input to the specified Execution's stdin. Sending and the receiver's completion are checked separately."
        }
        OperationId::ShellEof => "Close the specified Execution's stdin, ending further input.",
        OperationId::ShellStop => {
            "Request termination of the specified Execution's OS process group. Its scope differs from stopping the whole Run."
        }
        OperationId::AttachmentAdd => {
            "Add a file or image path reference to the target Thread. Existing attachment rules govern reading, updates, and provenance."
        }
        OperationId::AttachmentList => {
            "Show attachment references selected for the target Thread and their states."
        }
        OperationId::AttachmentRemove => {
            "Remove an attachment_id from the Thread's selection. This does not delete the external file."
        }
        OperationId::CompactList => {
            "Show saved compaction history filtered by time. This does not request a new compaction."
        }
        OperationId::Compact => {
            "Request compaction at a safe boundary or spill, summarize, remove, or restore a range. Summarization uses the model API."
        }
        OperationId::IndexStatus => {
            "Show conversation index settings, spaces, processed counts, and stop or failure reasons."
        }
        OperationId::IndexPreview => {
            "Inspect target counts and readable input size without calling the embedding API."
        }
        OperationId::IndexBackfill => {
            "Start embedding indexing for explicitly selected past history. Review the preview and API costs first."
        }
        OperationId::IndexRetry => {
            "Retry failed indexing work under current settings. API costs may apply."
        }
        OperationId::IndexRebuild => {
            "Build a candidate index for selected sources and optional dimensions. The existing active space remains until validation succeeds."
        }
        OperationId::IndexProbe => {
            "Send one fixed input to the embedding API and save connection and dimension results. API costs may apply."
        }
        OperationId::StorageStatus => {
            "Show saved logical usage, limits, retention, and reclamation information for selected categories."
        }
        OperationId::StorageCollect => {
            "Explicitly collect reclaimable saved data in selected categories. Protected originals and references are retained."
        }
        OperationId::Exec => {
            "Request work on a saved Thread and validate final JSON against a local schema. Additional model and tool processing may run within the repair limit."
        }
        OperationId::ConfigInit => {
            "Create a template at the selected config path. Existing files are not overwritten."
        }
        OperationId::ConfigCheck => {
            "Read config and environment and fetch models from the default connection. Changes reach the running owner on restart."
        }
        OperationId::DocsList => "List the bundled guides by hierarchy.",
        OperationId::DocsGet => {
            "Read versioned bundled guide text by document ID and optional heading."
        }
        OperationId::DocsSearch => "Search bundled guide text without an owner or embedding API.",
        OperationId::RagInit => {
            "Initialize the specified independent RAG database without starting the conversation owner."
        }
        OperationId::RagSourceAdd => {
            "Register a document folder or file as a named source and save exclusion patterns."
        }
        OperationId::RagSourceList => {
            "Show registered independent RAG sources and exclusion settings."
        }
        OperationId::RagSourceRemove => {
            "Remove a source registration and its independent index. Original external documents are retained."
        }
        OperationId::RagIndex => {
            "Index registered sources with keyword or semantic mode. dry-run shows targets; semantic processing may incur API costs."
        }
        OperationId::RagIngest => {
            "Register externally extracted text with its source file, kind, and optional metadata."
        }
        OperationId::RagStatus => {
            "Show independent RAG sources, index state, and unindexed or failed items."
        }
        OperationId::RagInspect => {
            "Show saved versions, provenance, extraction details, and index information for a document ID."
        }
        OperationId::RagSearch => {
            "Search explicit scopes of guides, conversations, and registered sources. Semantic and hybrid modes may incur API costs."
        }
        OperationId::RagGet => {
            "Fetch corresponding original text in bounded pages using a versioned ref from search results."
        }
    }
}
fn field_label_en(field: FieldId) -> &'static str {
    match field {
        FieldId::Attempt => "Model attempt ID (alone)",
        FieldId::Thread => "Thread ID",
        FieldId::Run => "Run ID",
        FieldId::Turn => "Turn ID",
        FieldId::Node => "History node ID",
        FieldId::Parent => "Parent Thread ID",
        FieldId::Message => "Message ID",
        FieldId::Messages => "Message IDs to cancel",
        FieldId::Operation => "Receipt operation ID",
        FieldId::Execution => "Command execution ID",
        FieldId::Query => "Search or filter text",
        FieldId::GroupBy => "Usage breakdown",
        FieldId::Timezone => "Breakdown timezone",
        FieldId::GroupLimit => "Breakdown group limit",
        FieldId::Limit => "Result count",
        FieldId::Cursor => "Continuation cursor",
        FieldId::Since => "Recorded at or after",
        FieldId::Until => "End time (exclusive)",
        FieldId::After => "Previous wait cursor",
        FieldId::Memory => "Memory status",
        FieldId::CommandContains => "Command contains",
        FieldId::ExitCode => "Exit code",
        FieldId::States => "Saved states",
        FieldId::State => "Saved state of related shell",
        FieldId::All => "Include terminal states",
        FieldId::IncludeChildren => "Include child usage",
        FieldId::Agents => "Instruction file",
        FieldId::Cwd => "Working folder",
        FieldId::Connection => "Connection name",
        FieldId::Model => "Model name",
        FieldId::Effort => "Reasoning effort",
        FieldId::Title => "Display title",
        FieldId::Descendants => "Include descendants",
        FieldId::SenderLabel => "Sender display name",
        FieldId::Now => "Release foreground shell wait",
        FieldId::Text => "Message text",
        FieldId::Conditions => "Observation conditions",
        FieldId::SettlementStates => "Conversation observation states",
        FieldId::Timeout => "Observation or execution limit (ms)",
        FieldId::ExcerptChars => "Excerpt characters",
        FieldId::LimitBytes => "Read size in bytes",
        FieldId::FinalAnswerOnly => "Final answer only",
        FieldId::Encoding => "Encoding",
        FieldId::Relation => "History relationship",
        FieldId::Kinds => "Included node kinds",
        FieldId::ExcludeKinds => "Excluded node kinds",
        FieldId::From => "Start time",
        FieldId::To => "End time",
        FieldId::Path => "Path",
        FieldId::AttachmentKind => "Attachment kind",
        FieldId::Attachment => "Attachment reference ID",
        FieldId::CompactAction => "Compaction or editing method",
        FieldId::FromNode => "First node ID in range",
        FieldId::ToNode => "Last node ID in range",
        FieldId::SummaryMaxChars => "Summary character limit (empty = configured)",
        FieldId::SummaryMaxPercent => "Summary percentage limit (empty = configured)",
        FieldId::IndexScope => "Index scope",
        FieldId::Space => "Index space ID",
        FieldId::Threads => "Thread IDs to index",
        FieldId::Sources => "Source node IDs to index",
        FieldId::Dimensions => "Requested embedding dimensions",
        FieldId::Category => "Saved category",
        FieldId::Schema => "Final JSON schema file",
        FieldId::MaxRepairs => "Maximum additional repairs",
        FieldId::DocParent => "Guide parent ID",
        FieldId::Document => "Document ID",
        FieldId::Section => "Guide section",
        FieldId::Source => "Registered source",
        FieldId::SourceName => "Registered source name",
        FieldId::Excludes => "Excluded globs",
        FieldId::ClearExcludes => "Clear existing exclusions",
        FieldId::DryRun => "Preview targets only",
        FieldId::SearchMode => "Search or index mode",
        FieldId::IngestKind => "Ingested content kind",
        FieldId::SourceFile => "Source material file",
        FieldId::TextFile => "Text file to ingest",
        FieldId::MetadataFile => "Supplementary metadata file",
        FieldId::RagScopes => "Document spaces to search",
        FieldId::Reference => "Versioned original ref",
    }
}
fn field_help_en(field: FieldId) -> &'static str {
    match field {
        FieldId::Attempt => {
            "Retrieve saved attempt usage and cache diagnostics. Leave other grouping options empty."
        }
        FieldId::Thread => {
            "Specify the saved target Thread. On CLI, an authenticated model defaults only to its own Thread."
        }
        FieldId::Run => {
            "Specify the target Run. A human CLI must supply an ID; only Revisit can default to the model's own Run."
        }
        FieldId::Turn => {
            "Specify the saved Turn ID for one response interval. This is distinct from a Run ID."
        }
        FieldId::Node => "Specify the saved node ID whose original or relationships to inspect.",
        FieldId::Parent => {
            "Parent Thread of the new conversation. Omit for a conversation without a parent."
        }
        FieldId::Message => "The message_id returned in the message send receipt.",
        FieldId::Messages => {
            "Specify unadopted message_id values to cancel. CLI repeats --message; maximum 128."
        }
        FieldId::Operation => {
            "The operation_id returned by the receipt or error, distinct from the request ID of this lookup."
        }
        FieldId::Execution => {
            "Use the execution_id returned by the target work's shell tool, not the observing CLI's own ID."
        }
        FieldId::Query => "Search terms or a list filter for this operation.",
        FieldId::GroupBy => {
            "Empty gives the traditional total; hour/day/thread give bounded saved breakdowns."
        }
        FieldId::Timezone => {
            "Only with a breakdown. Empty uses owner local; otherwise UTC or a fixed +HH:MM/-HH:MM offset."
        }
        FieldId::GroupLimit => {
            "Only with a breakdown; 1..168. Empty uses effective limit 168; the CLI argument itself has no default."
        }
        FieldId::Limit => {
            "Shows the CLI count default. The separate TUI list display default is 20."
        }
        FieldId::Cursor => {
            "The cursor from the immediately preceding read of the same target and conditions. Empty starts at the beginning."
        }
        FieldId::Since => {
            "RFC3339 with timezone. The meaning of the saved timestamp depends on the operation."
        }
        FieldId::Until => {
            "RFC3339 end boundary. Saved values are selected from the start inclusive to the end exclusive."
        }
        FieldId::After => {
            "Use the previous wait cursor to observe later saved changes under the same conditions."
        }
        FieldId::Memory => {
            "Observe current owner cache and OS values; do not combine with saved work conditions."
        }
        FieldId::CommandContains => {
            "Filter by substring of the saved command. This does not execute it."
        }
        FieldId::ExitCode => "Filter by saved exit code; negative values are allowed.",
        FieldId::States => "List saved native states. CLI repeats --state.",
        FieldId::State => {
            "Filter history-related shells by starting/running/stopping/exited/failed/stopped."
        }
        FieldId::All => "Show terminal states as well as the usual pending range.",
        FieldId::IncludeChildren => {
            "Include child usage in a Thread total. Cannot be combined with a Run or Turn."
        }
        FieldId::Agents => {
            "An instruction file resolved from the effective target cwd. Inheritance and omission follow the create or set operation."
        }
        FieldId::Cwd => {
            "On creation, relative paths use the launch directory; on update, the target Thread's saved cwd. Omission follows the operation's defaults or inheritance."
        }
        FieldId::Connection => {
            "A connection defined in config. model list chooses the Thread connection and then the owner's default."
        }
        FieldId::Model => {
            "Specify a model returned by the connection. No price or capability catalogue is built in."
        }
        FieldId::Effort => {
            "Omission uses defaults or inheritance. An empty CLI value explicitly clears it."
        }
        FieldId::Title => "Change the saved title. An empty value restores automatic naming.",
        FieldId::Descendants => "Include descendant Threads in deletion or its impact preview.",
        FieldId::SenderLabel => {
            "A self-declared label; it grants no authority. message send allows at most 64 Unicode characters."
        }
        FieldId::Now => {
            "Save the input and release the foreground shell wait. This does not interrupt response generation."
        }
        FieldId::Text => {
            "The TUI uses the retained text. CLI --text-stdin is not connected to terminal stdin in the TUI."
        }
        FieldId::Conditions => {
            "Use Turn IDs for turn_completed, Run IDs for run_ended, execution IDs for shell_ended and shell_output, and Thread IDs for thread_settled. Specify matching IDs when changing kind. Set the shell cursor for output and previous wait after for resumption separately."
        }
        FieldId::SettlementStates => {
            "Space-separated thread_settled states. The form defaults to completed recovery_wait failed stopped; CLI uses the states array in conditions-json."
        }
        FieldId::Timeout => {
            "The observing client's deadline. Expiry alone does not stop the target Run or shell."
        }
        FieldId::ExcerptChars => {
            "0..4096 Unicode scalars. Zero omits excerpt text; CLI default is 512."
        }
        FieldId::LimitBytes => {
            "Maximum original text fetched in one request, subject to CLI defaults and per-operation limits."
        }
        FieldId::FinalAnswerOnly => {
            "Fetch only the saved final assistant message, excluding reasoning and interim explanations."
        }
        FieldId::Encoding => {
            "An omitted shell read encoding uses the saved value. Continue reading with the same encoding and target."
        }
        FieldId::Relation => {
            "Choose previous/next/sources/replacements. Neighbours and sources are distinct relationships."
        }
        FieldId::Kinds => "List saved node kinds to include in search. CLI repeats --kind.",
        FieldId::ExcludeKinds => {
            "List saved node kinds to exclude from search. CLI repeats --exclude-kind."
        }
        FieldId::From => "Inclusive start boundary in RFC3339 with timezone.",
        FieldId::To => "Exclusive end boundary in RFC3339 with timezone.",
        FieldId::Path => {
            "File or folder resolved by the operation. Existing external sources are not copied into history."
        }
        FieldId::AttachmentKind => "Specify file or image.",
        FieldId::Attachment => "Specify the attachment_id returned by attachment list.",
        FieldId::CompactAction => {
            "Omission requests compaction at a safe boundary. maximal/spill/summary/trim/omit/restore are mutually exclusive."
        }
        FieldId::FromNode => {
            "Includes the first node in the edit range. restore can also specify the selected marker or first omitted source."
        }
        FieldId::ToNode => {
            "Includes the last node in the edit range. For trim, this is the last node in the removed prefix."
        }
        FieldId::SummaryMaxChars => "Only for summary. Maximum length of the completed summary.",
        FieldId::SummaryMaxPercent => {
            "Only for summary. Percentage of original characters, greater than zero and at most 100."
        }
        FieldId::IndexScope => {
            "Specify threads or all. Execution retains current preview and explicit target rules."
        }
        FieldId::Space => "The current active space or an explicitly specified saved space ID.",
        FieldId::Threads => "Specify Thread IDs for past history indexing. CLI repeats --thread.",
        FieldId::Sources => "Specify saved source node IDs. CLI repeats --source.",
        FieldId::Dimensions => {
            "Omission follows current configuration or space contract. Accepted model dimensions are not guessed."
        }
        FieldId::Category => {
            "Select a collection target. Saved-state inspection shows the whole state regardless of selection."
        }
        FieldId::Schema => {
            "A supported JSON Schema file up to 64 KiB. Unsupported keywords are input errors."
        }
        FieldId::MaxRepairs => {
            "0..5; CLI default is 2. Each additional request can incur model and tool costs."
        }
        FieldId::DocParent => "Bundled guide category ID used to narrow the hierarchy.",
        FieldId::Document => {
            "Saved document ID for bundled guides or independent RAG. Check the operation's document space."
        }
        FieldId::Section => "Specify a heading or section returned by docs get.",
        FieldId::Source => {
            "Registered source ID or name. docs in rag index denotes bundled guides."
        }
        FieldId::SourceName => "The registered name identifying a source.",
        FieldId::Excludes => {
            "Repeat source-relative globs. Cannot be combined with clear-excludes."
        }
        FieldId::ClearExcludes => {
            "Explicitly clear exclusions when re-registering a source. Cannot be combined with exclude."
        }
        FieldId::DryRun => "Inspect intended targets and changes without applying the operation.",
        FieldId::SearchMode => {
            "Choose keyword/semantic/hybrid supported by the operation. Semantic mode can use an API."
        }
        FieldId::IngestKind => {
            "Specify extracted/llm-explanation/summary to retain provenance of extracted content."
        }
        FieldId::SourceFile => "Original source file for the extracted text.",
        FieldId::TextFile => {
            "UTF-8 text file mutually exclusive with --text-stdin. The existing 16 MiB limit applies."
        }
        FieldId::MetadataFile => {
            "Optional registration metadata file. A worker reads and validates it."
        }
        FieldId::RagScopes => "Repeat docs/history/history:THREAD/source:ID or names.",
        FieldId::Reference => {
            "Specify the ref returned by rag search. The cursor is bound to the same ref."
        }
    }
}
fn discovery_en(discovery: Discovery) -> &'static str {
    match discovery {
        Discovery::None => "Review the operation's input requirements.",
        Discovery::ThreadList => "Find a Thread ID in thread list or the conversation list.",
        Discovery::RunFromThread => "Find the target Run ID with thread get or status.",
        Discovery::TurnFromRun => "Find the target Turn ID with run get.",
        Discovery::HistoryList => "Find saved node IDs and the adopted path with history list.",
        Discovery::MessageReceipt => {
            "Use the message_id returned by message send or the input ID in the queue."
        }
        Discovery::OperationReceipt => "Use the operation_id returned by the receipt or error.",
        Discovery::ShellFromToolOrStatus => {
            "Find the execution_id returned by the shell tool or in the status work list."
        }
        Discovery::SameReadCursor => {
            "Use the cursor from the previous read of the same target. Empty starts at the beginning."
        }
        Discovery::Status => "Inspect saved work and related IDs with status.",
        Discovery::Filesystem => {
            "Check the target file or folder path. A worker performs the read."
        }
        Discovery::ConnectionConfig => {
            "Check connection names in config. api is responses or chat_completions."
        }
        Discovery::ModelList => {
            "Find available model names returned by the connection with model list."
        }
        Discovery::AttachmentList => "Find attachment_id values with attachment list.",
        Discovery::IndexStatus => {
            "Inspect space ID, current settings, and dimensions with index status."
        }
        Discovery::DocsList => "Find bundled guide IDs and hierarchy with docs list.",
        Discovery::DocumentList => {
            "Find bundled guide IDs with docs list and independent documents with rag search."
        }
        Discovery::DocsSections => "Find a section in the headings listed by docs get.",
        Discovery::RagSourceList => "Find registered source IDs and names with rag source list.",
        Discovery::RagSearch => "Use the versioned ref returned by rag search.",
    }
}
fn term_en(term: Term) -> &'static str {
    match term {
        Term::Run => {
            "A Run is a unit of work progressing a request. It may continue after a Turn's response until owned shells and child Runs finish."
        }
        Term::Turn => {
            "A Turn is one response interval. It may include multiple model requests and tools, and completes, stops, or awaits recovery separately from its Run."
        }
        Term::Message => {
            "A Message is saved input. Receipt, Run and Turn assignment, and response completion are separate stages."
        }
        Term::Execution => {
            "An Execution is one OS process group started by a shell tool. Distinguish the observing CLI's ID from the target work's ID."
        }
    }
}
#[allow(dead_code)]
pub(super) fn text_for(value: Text, language: crate::config::UiLanguage) -> &'static str {
    use crate::config::UiLanguage;
    use crate::operations::{EnterVerb, TextKey};
    if language == UiLanguage::Ja {
        return value.fallback;
    }
    match value.key {
        TextKey::Label(id) => operation_label_en(id),
        TextKey::Meaning(id) => operation_meaning_en(id),
        TextKey::FieldLabel(_, field) => field_label_en(field),
        TextKey::FieldHelp(_, field) => field_help_en(field),
        TextKey::Effect(Effect::Inspect) => "Show saved state or original text.",
        TextKey::Effect(Effect::SavedMutation) => "Change saved data or settings.",
        TextKey::Effect(Effect::RuntimeControl) => {
            "Start, resume, or control model, shell, indexing, or other processing."
        }
        TextKey::EffectMarker(Effect::Inspect) => "",
        TextKey::EffectMarker(Effect::SavedMutation) => "[Change] ",
        TextKey::EffectMarker(Effect::RuntimeControl) => "[Control] ",
        TextKey::Enter(EnterVerb::Display) => "Show",
        TextKey::Enter(EnterVerb::Execute) => "Run",
        TextKey::Enter(EnterVerb::Send) => "Send",
        TextKey::Term(term) => term_en(term),
        TextKey::Discovery(discovery) => discovery_en(discovery),
    }
}
