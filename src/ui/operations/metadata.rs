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
            return "空欄は起動時data-dir/rag.db · 相対パスは起動場所が基準 · F1で説明".into();
        }
        if field.name == "input_mode" {
            return "fileは本文ファイル、inlineは本文入力。CLI stdin相当でも端末stdinは読みません · F1で説明".into();
        }
        let Some(metadata) = self.selected_metadata() else {
            return "F1で入力条件を確認できます".into();
        };
        if field.value != field.initial_value {
            return "編集した値 · F1で入力条件とIDの探し方".into();
        }
        let discovery = metadata.field.discovery;
        if field.value.is_empty() && discovery != Discovery::None {
            return format!("未入力 · {}", text(discovery.text()));
        }
        let origin = if !field.initial_value.is_empty()
            && matches!(field.kind, Kind::Choice(_) | Kind::Boolean | Kind::Number)
        {
            "このフォームの初期選択"
        } else {
            self.origin(metadata.default_origin)
        };
        format!("初期値: {origin} · F1で入力条件とIDの探し方")
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
                "{} · {}",
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
        for argument in id.primary_cli().arguments {
            let suffix = if argument.repeat {
                "（繰り返し指定）"
            } else if argument.takes_value {
                " 値"
            } else {
                "（フラグ）"
            };
            let default = argument
                .default
                .map(|value| format!(" · 既定 {value}"))
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
                "{} · {}: {}",
                argument.spelling, argument.label, argument.meaning
            )));
        }
        // Standalone forms use this same help projection once G3 supplies their
        // typed invocation. No unsupported action is advertised in the G2 list.
        if spec.route == crate::operations::Route::Rag {
            let argument = crate::operations::RAG_DB_ARGUMENT;
            lines.push(Line::from(format!(
                "{} · {}: {}",
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
