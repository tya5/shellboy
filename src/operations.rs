//! Shared operation meanings and CLI bindings; ordinary typed metadata only.
//! Builders stay in the CLI/client/TUI modules. This catalogue does not execute
//! commands, interpret field expressions or claim an unsupported form succeeded.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum OperationId {
    Status,
    UsageGet,
    ModelList,
    ThreadList,
    ThreadGet,
    ThreadCreate,
    ThreadFork,
    ThreadDelete,
    ThreadDeletePreview,
    ThreadPrune,
    ThreadSet,
    MessageSend,
    MessageReply,
    MessageGet,
    MessageCancel,
    OperationGet,
    RunGet,
    RunRetry,
    RunStop,
    TurnGet,
    Wait,
    HistoryList,
    HistoryGet,
    HistorySearch,
    HistoryRelations,
    ShellList,
    ShellGet,
    ShellRead,
    ShellWrite,
    ShellEof,
    ShellStop,
    AttachmentAdd,
    AttachmentList,
    AttachmentRemove,
    CompactList,
    Compact,
    IndexStatus,
    IndexPreview,
    IndexBackfill,
    IndexRetry,
    IndexRebuild,
    IndexProbe,
    StorageStatus,
    StorageCollect,
    Exec,
    ConfigInit,
    ConfigCheck,
    DocsList,
    DocsGet,
    DocsSearch,
    RagInit,
    RagSourceAdd,
    RagSourceList,
    RagSourceRemove,
    RagIndex,
    RagIngest,
    RagStatus,
    RagInspect,
    RagSearch,
    RagGet,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Inspect,
    SavedMutation,
    RuntimeControl,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnterVerb {
    Display,
    Execute,
    Send,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirmation {
    None,
    ThreadDeletePreview,
    IndexCostPreview,
    PaidRequest,
    StorageCollect,
    ContextEditPreview,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Owner,
    ComposedClient,
    Config,
    Docs,
    Rag,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestIdPolicy {
    OwnerOperation,
    StandaloneUnavailable,
    ComposedUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum IdKind {
    Thread,
    Run,
    Turn,
    Node,
    Message,
    Operation,
    Execution,
    Attachment,
    Space,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldType {
    Text,
    TextList,
    Multiline,
    Path,
    Signed,
    Unsigned,
    Decimal,
    Boolean,
    Choice,
    Conditions,
    Timestamp,
    Cursor,
    Id(IdKind),
    Ids(IdKind),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DefaultOrigin {
    None,
    Required,
    Literal,
    SelectedThread,
    SelectedRun,
    SelectedTurn,
    FocusedNode,
    FocusedExecution,
    ReferencedMessage,
    ReferencedOperation,
    TargetThreadSettings,
    ThreadThenOwnerConfig,
    ProcessCwd,
    ActiveIndexSpace,
    ConfiguredEmbedding,
    ParentOrOwnerDefaults,
    SavedShellEncoding,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Discovery {
    None,
    ThreadList,
    RunFromThread,
    TurnFromRun,
    HistoryList,
    MessageReceipt,
    OperationReceipt,
    ShellFromToolOrStatus,
    SameReadCursor,
    Status,
    Filesystem,
    ConnectionConfig,
    ModelList,
    AttachmentList,
    IndexStatus,
    DocsList,
    DocumentList,
    DocsSections,
    RagSourceList,
    RagSearch,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum FieldId {
    Attempt,
    Thread,
    Run,
    Turn,
    Node,
    Before,
    Parent,
    Message,
    Messages,
    Operation,
    Execution,
    Query,
    GroupBy,
    Timezone,
    GroupLimit,
    Limit,
    Cursor,
    Since,
    Until,
    After,
    Memory,
    CommandContains,
    ExitCode,
    States,
    State,
    All,
    IncludeChildren,
    Agents,
    Cwd,
    Connection,
    Model,
    Effort,
    Title,
    Descendants,
    SenderLabel,
    ExpectReply,
    Deliver,
    Text,
    Conditions,
    SettlementStates,
    Timeout,
    ExcerptChars,
    LimitBytes,
    EndMessageOnly,
    Parts,
    Encoding,
    Relation,
    Kinds,
    ExcludeKinds,
    From,
    To,
    Path,
    AttachmentKind,
    Attachment,
    CompactAction,
    FromNode,
    ToNode,
    SummaryMaxChars,
    SummaryMaxPercent,
    IndexScope,
    Space,
    Threads,
    Sources,
    Dimensions,
    Category,
    Schema,
    MaxRepairs,
    ExecResult,
    DocParent,
    Document,
    Section,
    Source,
    SourceName,
    Excludes,
    ClearExcludes,
    DryRun,
    SearchMode,
    IngestKind,
    SourceFile,
    TextFile,
    MetadataFile,
    RagScopes,
    Reference,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Term {
    Run,
    Turn,
    Message,
    Execution,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextKey {
    Label(OperationId),
    Meaning(OperationId),
    FieldLabel(OperationId, FieldId),
    FieldHelp(OperationId, FieldId),
    Effect(Effect),
    EffectMarker(Effect),
    Enter(EnterVerb),
    Term(Term),
    Discovery(Discovery),
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Text {
    pub key: TextKey,
    pub fallback: &'static str,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct FieldSpec {
    pub id: FieldId,
    pub default_origin: DefaultOrigin,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: FieldType,
    pub discovery: Discovery,
}
#[derive(Clone, Debug)]
pub(crate) struct OperationField {
    pub field: &'static FieldSpec,
    pub default_origin: DefaultOrigin,
    pub cli_default: Option<&'static str>,
    pub tui_default: Option<&'static str>,
    pub choices: Vec<&'static str>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct OperationSpec {
    pub id: OperationId,
    pub label: &'static str,
    pub meaning: &'static str,
    pub effect: Effect,
    pub confirmation: Confirmation,
    pub route: Route,
    pub fields: &'static [FieldId],
}
#[derive(Clone, Debug)]
pub(crate) struct CliArgument {
    pub spelling: String,
    pub field: FieldId,
    pub takes_value: bool,
    pub repeat: bool,
    pub default: Option<String>,
    pub choices: Vec<String>,
}
#[derive(Clone, Debug)]
pub(crate) struct CliBinding {
    pub operation: OperationId,
    pub path: &'static [&'static str],
    pub arguments: Vec<CliArgument>,
    pub alias_of: Option<&'static [&'static str]>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct GlobalArgument {
    pub spelling: &'static str,
    pub label: &'static str,
    pub meaning: &'static str,
}
pub(crate) const GLOBAL_ARGUMENTS: &[GlobalArgument] = &[
    GlobalArgument {
        spelling: "--data-dir",
        label: "所有者のデータパス",
        meaning: "CLI指定、環境変数、TOML、OS既定の優先順です。TUIの所有者操作は現在の所有者へ固定されます。",
    },
    GlobalArgument {
        spelling: "--config",
        label: "設定ファイル",
        meaning: "CLI指定がOS既定より優先されます。config checkはそのファイルの既定接続を確認し、実行中の所有者設定は再起動時に反映します。",
    },
    GlobalArgument {
        spelling: "--operation-id",
        label: "この要求のoperation ID",
        meaning: "所有者操作の再送または照会要求の識別子です。execと独立config/docs/RAGでは使用できません。operation getの照会対象は--operationです。",
    },
];
pub(crate) const RAG_DB_ARGUMENT: GlobalArgument = GlobalArgument {
    spelling: "--db",
    label: "独立RAGデータベース",
    meaning: "明示パス、未指定は解決済みDATA_DIR/rag.dbです。所有者の会話DBとは別です。",
};

pub(crate) const DEFAULT_SETTLEMENT_STATES: &str = "completed recovery_wait failed stopped";
pub(crate) const FIELD_SPECS: &[FieldSpec] = &[
    FieldSpec {
        id: FieldId::SettlementStates,
        default_origin: DefaultOrigin::Literal,
        label: "会話の観測状態",
        help: "thread_settledのstatesを空白で区切ります。フォーム初期値はcompleted recovery_wait failed stoppedです。CLIではconditions-jsonのstates配列で明示します。",
        kind: FieldType::TextList,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Until,
        default_origin: DefaultOrigin::None,
        label: "終了日時（未満）",
        help: "RFC3339の終了境界です。開始日時以上・終了日時未満で保存済みの値を選びます。",
        kind: FieldType::Timestamp,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::After,
        default_origin: DefaultOrigin::None,
        label: "前回の待機cursor",
        help: "前回waitのcursorを指定し、同じ条件で保存された後続の状態変更を観測します。",
        kind: FieldType::Cursor,
        discovery: Discovery::SameReadCursor,
    },
    FieldSpec {
        id: FieldId::Memory,
        default_origin: DefaultOrigin::None,
        label: "メモリ状態",
        help: "保存済み作業の条件とは併用せず、ownerのcacheとOSの現在値を観測します。",
        kind: FieldType::Boolean,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::CommandContains,
        default_origin: DefaultOrigin::None,
        label: "コマンドを含む",
        help: "保存されたコマンドの部分文字列で絞ります。コマンド自体は実行しません。",
        kind: FieldType::Text,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::ExitCode,
        default_origin: DefaultOrigin::None,
        label: "終了コード",
        help: "保存された終了コードで絞ります。負数も指定できます。",
        kind: FieldType::Signed,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Thread,
        default_origin: DefaultOrigin::SelectedThread,
        label: "会話ID",
        help: "対象の保存Threadを指定します。CLI省略時は認証済みモデルの自分のThreadだけが既定です。",
        kind: FieldType::Id(IdKind::Thread),
        discovery: Discovery::ThreadList,
    },
    FieldSpec {
        id: FieldId::Run,
        default_origin: DefaultOrigin::SelectedRun,
        label: "作業ID（Run）",
        help: "対象Runを指定します。人のCLIは明示IDが必要です。",
        kind: FieldType::Id(IdKind::Run),
        discovery: Discovery::RunFromThread,
    },
    FieldSpec {
        id: FieldId::Attempt,
        default_origin: DefaultOrigin::None,
        label: "モデル試行ID（単独指定）",
        help: "保存attemptの使用量とキャッシュ診断を取得します。他の集計指定は空にします。",
        kind: FieldType::Text,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Turn,
        default_origin: DefaultOrigin::SelectedTurn,
        label: "応答処理ID（Turn）",
        help: "一つの回答区間の保存Turn IDを指定します。Run IDとは別です。",
        kind: FieldType::Id(IdKind::Turn),
        discovery: Discovery::TurnFromRun,
    },
    FieldSpec {
        id: FieldId::Node,
        default_origin: DefaultOrigin::FocusedNode,
        label: "履歴ノードID",
        help: "原文または関係を調べる保存ノードIDを指定します。",
        kind: FieldType::Id(IdKind::Node),
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::Before,
        default_origin: DefaultOrigin::None,
        label: "この履歴ノードより前",
        help: "選択パス上のノードIDです。これより前で採用済みの要約・spillの原文をこのThreadの保持から外します。",
        kind: FieldType::Id(IdKind::Node),
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::Parent,
        default_origin: DefaultOrigin::None,
        label: "親の会話ID",
        help: "新しい会話の親Threadです。省略時は親のない会話になります。",
        kind: FieldType::Id(IdKind::Thread),
        discovery: Discovery::ThreadList,
    },
    FieldSpec {
        id: FieldId::Message,
        default_origin: DefaultOrigin::ReferencedMessage,
        label: "入力ID",
        help: "message sendの受付結果に返されたmessage_idです。",
        kind: FieldType::Id(IdKind::Message),
        discovery: Discovery::MessageReceipt,
    },
    FieldSpec {
        id: FieldId::Messages,
        default_origin: DefaultOrigin::None,
        label: "取消対象の入力ID",
        help: "取消す未採用message_idを明示します。CLIは--messageを繰り返し、最大128件です。",
        kind: FieldType::Ids(IdKind::Message),
        discovery: Discovery::MessageReceipt,
    },
    FieldSpec {
        id: FieldId::Operation,
        default_origin: DefaultOrigin::ReferencedOperation,
        label: "受付照会のoperation ID",
        help: "受付結果またはエラーに返されたoperation_idです。照会自身のrequest IDとは別です。",
        kind: FieldType::Id(IdKind::Operation),
        discovery: Discovery::OperationReceipt,
    },
    FieldSpec {
        id: FieldId::Execution,
        default_origin: DefaultOrigin::FocusedExecution,
        label: "コマンド実行ID",
        help: "対象作業のshellツールが返したexecution_idを使います。観測CLI自身のexecution_idと区別します。",
        kind: FieldType::Id(IdKind::Execution),
        discovery: Discovery::ShellFromToolOrStatus,
    },
    FieldSpec {
        id: FieldId::Query,
        default_origin: DefaultOrigin::None,
        label: "検索・絞込み文字列",
        help: "この操作の検索語または一覧の絞込み文字列です。",
        kind: FieldType::Text,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::GroupBy,
        default_origin: DefaultOrigin::None,
        label: "使用量の内訳",
        help: "空は従来の合計。hour/day/thread は有界な保存済み内訳です。",
        kind: FieldType::Choice,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Timezone,
        default_origin: DefaultOrigin::None,
        label: "内訳の時間帯",
        help: "内訳指定時のみ。空は owner local、UTC または固定 +HH:MM/-HH:MM。",
        kind: FieldType::Text,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::GroupLimit,
        default_origin: DefaultOrigin::None,
        label: "内訳の区切り上限",
        help: "内訳指定時のみ1..168。空は有効上限168で、CLI引数自体の既定値はありません。",
        kind: FieldType::Unsigned,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Limit,
        default_origin: DefaultOrigin::Literal,
        label: "取得件数",
        help: "CLIの件数既定を表示します。一覧のTUI表示既定20件は別に明示します。",
        kind: FieldType::Unsigned,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Cursor,
        default_origin: DefaultOrigin::None,
        label: "続きcursor",
        help: "直前の同じ対象・条件の読み取り結果cursorです。空は先頭です。",
        kind: FieldType::Cursor,
        discovery: Discovery::SameReadCursor,
    },
    FieldSpec {
        id: FieldId::Since,
        default_origin: DefaultOrigin::None,
        label: "記録日時以降",
        help: "タイムゾーン付きRFC3339です。操作の保存時刻の意味に従います。",
        kind: FieldType::Timestamp,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::States,
        default_origin: DefaultOrigin::None,
        label: "保存された状態",
        help: "保存native stateを列挙します。CLIは--stateを繰り返します。",
        kind: FieldType::TextList,
        discovery: Discovery::Status,
    },
    FieldSpec {
        id: FieldId::State,
        default_origin: DefaultOrigin::None,
        label: "関連shellの保存状態",
        help: "履歴に関連したshellのstarting/running/stopping/exited/failed/stoppedを絞り込みます。",
        kind: FieldType::Text,
        discovery: Discovery::ShellFromToolOrStatus,
    },
    FieldSpec {
        id: FieldId::All,
        default_origin: DefaultOrigin::Literal,
        label: "終端状態も含める",
        help: "通常のpending範囲に加えて終端状態も表示します。",
        kind: FieldType::Boolean,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::IncludeChildren,
        default_origin: DefaultOrigin::Literal,
        label: "子の使用量も含める",
        help: "Thread範囲で子の使用量も合計します。Run/Turn指定とは同時に使えません。",
        kind: FieldType::Boolean,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Agents,
        default_origin: DefaultOrigin::TargetThreadSettings,
        label: "指示ファイル",
        help: "対象の実効cwdから解決する指示ファイルです。継承・未指定の意味は作成／設定操作に従います。",
        kind: FieldType::Path,
        discovery: Discovery::Filesystem,
    },
    FieldSpec {
        id: FieldId::Cwd,
        default_origin: DefaultOrigin::TargetThreadSettings,
        label: "作業フォルダ",
        help: "作成時の相対パスは起動場所、設定変更時は対象Threadの保存cwdから解決します。未指定はその操作の既定・継承です。",
        kind: FieldType::Path,
        discovery: Discovery::Filesystem,
    },
    FieldSpec {
        id: FieldId::Connection,
        default_origin: DefaultOrigin::ThreadThenOwnerConfig,
        label: "接続名",
        help: "設定ファイルに定義された接続名です。model listではThread接続、所有者既定の順で選びます。",
        kind: FieldType::Text,
        discovery: Discovery::ConnectionConfig,
    },
    FieldSpec {
        id: FieldId::Model,
        default_origin: DefaultOrigin::TargetThreadSettings,
        label: "モデル名",
        help: "接続から取得したモデル名を指定します。内蔵の価格・能力カタログはありません。",
        kind: FieldType::Text,
        discovery: Discovery::ModelList,
    },
    FieldSpec {
        id: FieldId::Effort,
        default_origin: DefaultOrigin::TargetThreadSettings,
        label: "推論設定",
        help: "未指定は既定・継承です。CLIの空文字は明示解除として扱います。",
        kind: FieldType::Text,
        discovery: Discovery::ModelList,
    },
    FieldSpec {
        id: FieldId::Title,
        default_origin: DefaultOrigin::TargetThreadSettings,
        label: "表示タイトル",
        help: "保存タイトルを変更します。空文字は自動タイトルへ戻します。",
        kind: FieldType::Text,
        discovery: Discovery::ThreadList,
    },
    FieldSpec {
        id: FieldId::Descendants,
        default_origin: DefaultOrigin::Literal,
        label: "子孫も対象",
        help: "親子関係上の子孫Threadも削除／削除影響確認へ含めます。",
        kind: FieldType::Boolean,
        discovery: Discovery::ThreadList,
    },
    FieldSpec {
        id: FieldId::SenderLabel,
        default_origin: DefaultOrigin::None,
        label: "送信者の表示名",
        help: "自己申告の表示名であり権限を与えません。message sendは最大64Unicode文字です。",
        kind: FieldType::Text,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::ExpectReply,
        default_origin: DefaultOrigin::Literal,
        label: "返信を依頼",
        help: "指定メッセージへの返信を依頼します。",
        kind: FieldType::Boolean,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Deliver,
        default_origin: DefaultOrigin::None,
        label: "届けるとき",
        help: "空は次の安全な区切り。nowは前景shell待ちも解除します（応答生成は割込みません）。completeは動いているRunが完了するときに届け、動いていなければすぐ届けます。",
        kind: FieldType::Choice,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Text,
        default_origin: DefaultOrigin::None,
        label: "入力本文",
        help: "TUIでは保持した本文を使います。CLIの--text-stdinをTUIの端末stdinへ接続しません。",
        kind: FieldType::Multiline,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Conditions,
        default_origin: DefaultOrigin::None,
        label: "観測条件",
        help: "turn_completedはTurn ID、run_endedはRun ID、shell_endedとshell_outputは実行ID、thread_settledはThread IDの一覧で観測します。種類を変えたら対応するIDを明示します。出力条件はshellのcursor、再開は前回waitのafterを別々に指定します。",
        kind: FieldType::Conditions,
        discovery: Discovery::Status,
    },
    FieldSpec {
        id: FieldId::Timeout,
        default_origin: DefaultOrigin::Literal,
        label: "観測・実行の上限ms",
        help: "期限は観測クライアントの上限です。期限切れだけで対象Runやshellは停止しません。",
        kind: FieldType::Unsigned,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::ExcerptChars,
        default_origin: DefaultOrigin::Literal,
        label: "抜粋文字数",
        help: "0..4096Unicode scalarです。0は抜粋本文を省略し、CLI既定は512です。",
        kind: FieldType::Unsigned,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::LimitBytes,
        default_origin: DefaultOrigin::Literal,
        label: "取得byte数",
        help: "原文の一回の取得上限です。CLI既定と操作ごとの上限に従います。",
        kind: FieldType::Unsigned,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::EndMessageOnly,
        default_origin: DefaultOrigin::Literal,
        label: "終了メッセージのみ",
        help: "保存された最終assistant messageだけを取得し、推論と途中説明を除きます。",
        kind: FieldType::Boolean,
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::Parts,
        default_origin: DefaultOrigin::Literal,
        label: "要約partの一覧",
        help: "要約ノードの保存済みpart(map、reduce)を、範囲・段・費用・本文つきで取得します。",
        kind: FieldType::Boolean,
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::Encoding,
        default_origin: DefaultOrigin::Literal,
        label: "文字コード",
        help: "shell readの省略は保存値です。継続読み取りは同じ文字コードと対象を保持します。",
        kind: FieldType::Text,
        discovery: Discovery::SameReadCursor,
    },
    FieldSpec {
        id: FieldId::Relation,
        default_origin: DefaultOrigin::Literal,
        label: "履歴の関係",
        help: "previous/next/sources/replacementsを指定します。前後と出典は別の関係です。",
        kind: FieldType::Choice,
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::Kinds,
        default_origin: DefaultOrigin::None,
        label: "含めるnode種類",
        help: "検索へ含める保存node kindを列挙します。CLIは--kindを繰り返します。",
        kind: FieldType::TextList,
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::ExcludeKinds,
        default_origin: DefaultOrigin::None,
        label: "除外node種類",
        help: "検索から除く保存node kindを列挙します。CLIは--exclude-kindを繰り返します。",
        kind: FieldType::TextList,
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::From,
        default_origin: DefaultOrigin::None,
        label: "開始日時",
        help: "タイムゾーン付きRFC3339の開始境界を含めます。",
        kind: FieldType::Timestamp,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::To,
        default_origin: DefaultOrigin::None,
        label: "終了日時",
        help: "タイムゾーン付きRFC3339の終了境界を含めません。",
        kind: FieldType::Timestamp,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Path,
        default_origin: DefaultOrigin::ProcessCwd,
        label: "参照するパス",
        help: "指定操作で解決するファイル／フォルダです。既存外部資料は履歴のために複製しません。",
        kind: FieldType::Path,
        discovery: Discovery::Filesystem,
    },
    FieldSpec {
        id: FieldId::AttachmentKind,
        default_origin: DefaultOrigin::Required,
        label: "添付の種類",
        help: "fileまたはimageを指定します。",
        kind: FieldType::Choice,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Attachment,
        default_origin: DefaultOrigin::None,
        label: "添付参照ID",
        help: "attachment listが返したattachment_idを指定します。",
        kind: FieldType::Id(IdKind::Attachment),
        discovery: Discovery::AttachmentList,
    },
    FieldSpec {
        id: FieldId::CompactAction,
        default_origin: DefaultOrigin::Literal,
        label: "圧縮・編集方法",
        help: "未指定は安全境界での圧縮要求です。maximal/spill/summary/trim/omit/restoreは排他的です。",
        kind: FieldType::Choice,
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::FromNode,
        default_origin: DefaultOrigin::None,
        label: "範囲先頭node ID",
        help: "編集範囲の先頭を含みます。restoreは選択済みmarkerまたは最初の省略元も指定できます。",
        kind: FieldType::Id(IdKind::Node),
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::ToNode,
        default_origin: DefaultOrigin::None,
        label: "範囲末尾node ID",
        help: "編集範囲の末尾を含みます。trimは削除するprefix末尾です。",
        kind: FieldType::Id(IdKind::Node),
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::SummaryMaxChars,
        default_origin: DefaultOrigin::None,
        label: "要約文字数上限（空=設定値）",
        help: "summaryだけで指定できます。完成した要約の文字数上限です。",
        kind: FieldType::Unsigned,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::SummaryMaxPercent,
        default_origin: DefaultOrigin::None,
        label: "要約割合上限（空=設定値）",
        help: "summaryだけで指定できます。元の文字数に対する割合で、0より大きく100以下です。",
        kind: FieldType::Decimal,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::IndexScope,
        default_origin: DefaultOrigin::None,
        label: "索引対象範囲",
        help: "threads/allを指定します。実行時は現在のpreviewと明示対象の規則を保持します。",
        kind: FieldType::Choice,
        discovery: Discovery::IndexStatus,
    },
    FieldSpec {
        id: FieldId::Space,
        default_origin: DefaultOrigin::ActiveIndexSpace,
        label: "索引領域ID",
        help: "現在の有効領域または明示した保存space IDです。",
        kind: FieldType::Id(IdKind::Space),
        discovery: Discovery::IndexStatus,
    },
    FieldSpec {
        id: FieldId::Threads,
        default_origin: DefaultOrigin::SelectedThread,
        label: "索引対象Thread ID",
        help: "過去履歴を索引化するThread IDを明示します。CLIは--threadを繰り返します。",
        kind: FieldType::Ids(IdKind::Thread),
        discovery: Discovery::ThreadList,
    },
    FieldSpec {
        id: FieldId::Sources,
        default_origin: DefaultOrigin::None,
        label: "索引対象source node ID",
        help: "索引元の保存node IDを指定します。CLIは--sourceを繰り返します。",
        kind: FieldType::Ids(IdKind::Node),
        discovery: Discovery::HistoryList,
    },
    FieldSpec {
        id: FieldId::Dimensions,
        default_origin: DefaultOrigin::ConfiguredEmbedding,
        label: "要求する埋め込み次元",
        help: "省略時は現在の設定／領域の契約に従います。モデル側が受け付ける次元を内蔵推測しません。",
        kind: FieldType::Unsigned,
        discovery: Discovery::IndexStatus,
    },
    FieldSpec {
        id: FieldId::Category,
        default_origin: DefaultOrigin::Literal,
        label: "保存カテゴリ",
        help: "回収対象を選びます。保存状態の照会は選択に関係なく全体を表示します。",
        kind: FieldType::Choice,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::Schema,
        default_origin: DefaultOrigin::Required,
        label: "最終JSONのschemaファイル",
        help: "最大64KiBの対応JSON Schemaファイルです。未対応keywordは入力エラーです。",
        kind: FieldType::Path,
        discovery: Discovery::Filesystem,
    },
    FieldSpec {
        id: FieldId::ExecResult,
        default_origin: DefaultOrigin::Literal,
        label: "結果モード",
        help: "reply、turn-end、run-endから選択します。",
        kind: FieldType::Choice,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::MaxRepairs,
        default_origin: DefaultOrigin::Literal,
        label: "追加修復の上限",
        help: "0..5回、CLI既定2回です。各追加依頼はモデル・tool費用を生じ得ます。",
        kind: FieldType::Unsigned,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::DocParent,
        default_origin: DefaultOrigin::None,
        label: "ガイドの親ID",
        help: "階層を絞る同梱ガイドカテゴリIDです。",
        kind: FieldType::Text,
        discovery: Discovery::DocsList,
    },
    FieldSpec {
        id: FieldId::Document,
        default_origin: DefaultOrigin::Required,
        label: "文書ID",
        help: "同梱ガイドまたは独立RAGの保存文書IDです。操作の文書領域を確認します。",
        kind: FieldType::Text,
        discovery: Discovery::DocumentList,
    },
    FieldSpec {
        id: FieldId::Section,
        default_origin: DefaultOrigin::None,
        label: "ガイド見出し",
        help: "docs getが示す見出し・sectionを指定します。",
        kind: FieldType::Text,
        discovery: Discovery::DocsSections,
    },
    FieldSpec {
        id: FieldId::Source,
        default_origin: DefaultOrigin::None,
        label: "登録資料ソース",
        help: "登録ソースのIDまたは名前です。rag indexのdocsは同梱ガイドです。",
        kind: FieldType::Text,
        discovery: Discovery::RagSourceList,
    },
    FieldSpec {
        id: FieldId::SourceName,
        default_origin: DefaultOrigin::Required,
        label: "登録ソース名",
        help: "資料を識別する登録名です。",
        kind: FieldType::Text,
        discovery: Discovery::RagSourceList,
    },
    FieldSpec {
        id: FieldId::Excludes,
        default_origin: DefaultOrigin::None,
        label: "除外するglob",
        help: "ソース相対globを繰り返します。clear-excludesとは同時に使えません。",
        kind: FieldType::TextList,
        discovery: Discovery::RagSourceList,
    },
    FieldSpec {
        id: FieldId::ClearExcludes,
        default_origin: DefaultOrigin::Literal,
        label: "既存の除外設定を消去",
        help: "ソース再登録時の既存除外を明示解除します。excludeとの併用はできません。",
        kind: FieldType::Boolean,
        discovery: Discovery::RagSourceList,
    },
    FieldSpec {
        id: FieldId::DryRun,
        default_origin: DefaultOrigin::Literal,
        label: "実行対象だけを確認",
        help: "操作を適用せず、対象や変更の見込みを確認します。",
        kind: FieldType::Boolean,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::SearchMode,
        default_origin: DefaultOrigin::Literal,
        label: "検索・索引方式",
        help: "操作が対応するkeyword/semantic/hybridから選びます。semantic方式はAPIを使用し得ます。",
        kind: FieldType::Choice,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::IngestKind,
        default_origin: DefaultOrigin::Required,
        label: "登録本文の種類",
        help: "extracted/llm-explanation/summaryを指定し、抽出結果の由来を保持します。",
        kind: FieldType::Choice,
        discovery: Discovery::None,
    },
    FieldSpec {
        id: FieldId::SourceFile,
        default_origin: DefaultOrigin::Required,
        label: "元資料ファイル",
        help: "抽出済み本文の元となった資料ファイルです。",
        kind: FieldType::Path,
        discovery: Discovery::Filesystem,
    },
    FieldSpec {
        id: FieldId::TextFile,
        default_origin: DefaultOrigin::None,
        label: "登録本文のファイル",
        help: "--text-stdinと排他的なUTF-8本文ファイルです。既存の16MiB上限を保持します。",
        kind: FieldType::Path,
        discovery: Discovery::Filesystem,
    },
    FieldSpec {
        id: FieldId::MetadataFile,
        default_origin: DefaultOrigin::None,
        label: "補助metadataファイル",
        help: "任意の登録metadataファイルです。読み取りと検証はworkerで行います。",
        kind: FieldType::Path,
        discovery: Discovery::Filesystem,
    },
    FieldSpec {
        id: FieldId::RagScopes,
        default_origin: DefaultOrigin::None,
        label: "検索する資料領域",
        help: "docs/history/history:THREAD/source:IDまたは名前を繰り返します。",
        kind: FieldType::TextList,
        discovery: Discovery::RagSourceList,
    },
    FieldSpec {
        id: FieldId::Reference,
        default_origin: DefaultOrigin::Required,
        label: "原文の版付きref",
        help: "rag searchが返したrefを指定します。cursorは同じrefへ束縛されます。",
        kind: FieldType::Text,
        discovery: Discovery::RagSearch,
    },
];
pub(crate) const SPECS: &[OperationSpec] = &[
    OperationSpec {
        id: OperationId::CompactList,
        label: "圧縮 · 保存済み一覧",
        meaning: "保存された圧縮の履歴を期間で絞って表示します。新しい圧縮は要求しません。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Thread,
            FieldId::Since,
            FieldId::Until,
            FieldId::Limit,
        ],
    },
    OperationSpec {
        id: OperationId::ShellList,
        label: "shell · 一覧",
        meaning: "保存されたshell実行を会話・期間・状態で絞って表示します。出力本文は読みません。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Thread,
            FieldId::Since,
            FieldId::Until,
            FieldId::CommandContains,
            FieldId::State,
            FieldId::ExitCode,
            FieldId::Limit,
            FieldId::Cursor,
        ],
    },
    OperationSpec {
        id: OperationId::Status,
        label: "作業 · 状態一覧",
        meaning: "保存されたRun・Turn・shell・入力の状態と関連IDを指定範囲で表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Memory,
            FieldId::Thread,
            FieldId::Run,
            FieldId::Since,
            FieldId::States,
            FieldId::All,
            FieldId::Limit,
            FieldId::Cursor,
        ],
    },
    OperationSpec {
        id: OperationId::UsageGet,
        label: "使用量 · 合計",
        meaning: "指定したThread・Run・Turnの保存済みトークン使用量と処理時間を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Since,
            FieldId::Until,
            FieldId::Thread,
            FieldId::Run,
            FieldId::Turn,
            FieldId::IncludeChildren,
            FieldId::GroupBy,
            FieldId::Timezone,
            FieldId::GroupLimit,
            FieldId::Cursor,
            FieldId::Attempt,
        ],
    },
    OperationSpec {
        id: OperationId::ModelList,
        label: "モデル · 一覧",
        meaning: "指定接続、対象Threadの接続、所有者の既定接続の順でモデル一覧をAPIから取得します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Connection, FieldId::Thread],
    },
    OperationSpec {
        id: OperationId::ThreadList,
        label: "会話 · 一覧",
        meaning: "保存された会話のタイトル、親子関係、現在の状態を絞り込んで表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Limit, FieldId::Cursor, FieldId::Query],
    },
    OperationSpec {
        id: OperationId::ThreadGet,
        label: "会話 · 詳細",
        meaning: "指定したThreadの設定、現在のRun・Turn、所有作業と復旧理由を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Thread],
    },
    OperationSpec {
        id: OperationId::ThreadCreate,
        label: "会話 · 作成",
        meaning: "保存済みThreadを作成します。モデル処理は別の入力送信で開始します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Agents,
            FieldId::Cwd,
            FieldId::Connection,
            FieldId::Model,
            FieldId::Effort,
            FieldId::Parent,
        ],
    },
    OperationSpec {
        id: OperationId::ThreadFork,
        label: "会話 · 分岐",
        meaning: "指定した履歴ノードを境界に、出典を保持した新しいThreadを作成します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Thread,
            FieldId::Node,
            FieldId::Cwd,
            FieldId::Agents,
        ],
    },
    OperationSpec {
        id: OperationId::ThreadDelete,
        label: "会話 · 削除",
        meaning: "指定Threadと選択した子孫を、実行中の作業や参照を再確認して削除します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::ThreadDeletePreview,
        route: Route::Owner,
        fields: &[FieldId::Thread, FieldId::Descendants],
    },
    OperationSpec {
        id: OperationId::ThreadDeletePreview,
        label: "会話 · 削除影響",
        meaning: "指定Threadと選択した子孫の削除を妨げる作業・参照・回収見込みを表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Thread, FieldId::Descendants],
    },
    OperationSpec {
        id: OperationId::ThreadPrune,
        label: "会話 · 原文の解放",
        meaning: "指定ノードより前で採用済みの要約・spillの原文を、このThreadの保持から外します。回収は既存の到達性GCが行い、元に戻せません。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Thread, FieldId::Before, FieldId::DryRun],
    },
    OperationSpec {
        id: OperationId::ThreadSet,
        label: "会話 · 設定",
        meaning: "Threadの表示タイトル、作業場所、接続、モデル、推論設定、指示ファイルを変更します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Thread,
            FieldId::Title,
            FieldId::Cwd,
            FieldId::Connection,
            FieldId::Model,
            FieldId::Effort,
            FieldId::Agents,
        ],
    },
    OperationSpec {
        id: OperationId::MessageSend,
        label: "依頼 · 送信",
        meaning: "指定Threadへ入力を保存します。受付、Run・Turnへの採用、回答完了は個別に追跡します。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Thread,
            FieldId::SenderLabel,
            FieldId::Deliver,
            FieldId::ExpectReply,
            FieldId::Text,
        ],
    },
    OperationSpec {
        id: OperationId::MessageReply,
        label: "依頼 · 返信",
        meaning: "受信した依頼へ一度だけ返信します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Message, FieldId::SenderLabel, FieldId::Text],
    },
    OperationSpec {
        id: OperationId::MessageGet,
        label: "依頼 · 割当と回答",
        meaning: "保存された入力の受付状態と不変のRun・Turn割当、回答への参照を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Message],
    },
    OperationSpec {
        id: OperationId::MessageCancel,
        label: "依頼 · 未採用入力の取消",
        meaning: "明示した未採用の入力を取り消します。既に履歴へ採用された入力は対象外です。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Messages],
    },
    OperationSpec {
        id: OperationId::OperationGet,
        label: "操作 · 受付結果",
        meaning: "指定operation_idの保存済み受付結果と後続処理を確認し、不確実な再送を判断できます。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Operation],
    },
    OperationSpec {
        id: OperationId::RunGet,
        label: "作業 · 詳細",
        meaning: "Runの保存状態、現在のTurn、所有関係、子作業・shellと保存された復旧理由を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Run],
    },
    OperationSpec {
        id: OperationId::RunRetry,
        label: "作業 · 復旧を再試行",
        meaning: "原因を確認した復旧待ちTurnを、明示したRunで再開します。終了済みRunは再開できません。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Run],
    },
    OperationSpec {
        id: OperationId::RunStop,
        label: "作業 · 停止",
        meaning: "指定Runとその所有作業の停止を要求します。保存状態で実際の終了を確認します。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Run],
    },
    OperationSpec {
        id: OperationId::TurnGet,
        label: "応答処理 · 詳細",
        meaning: "一つのTurnの保存状態と回答ノードIDを表示します。回答本文は履歴原文から参照できます。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Turn],
    },
    OperationSpec {
        id: OperationId::Wait,
        label: "待機 · 条件を観測",
        meaning: "指定条件のいずれかが成立するまで観測します。観測の期限切れ後も対象の作業は続行します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::SettlementStates,
            FieldId::Cursor,
            FieldId::After,
            FieldId::Conditions,
            FieldId::Timeout,
        ],
    },
    OperationSpec {
        id: OperationId::HistoryList,
        label: "履歴 · 一覧",
        meaning: "指定Threadの採用経路を、出典のRun・日時・関連shell状態で絞り込んで表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Thread,
            FieldId::Run,
            FieldId::Since,
            FieldId::State,
            FieldId::ExcerptChars,
            FieldId::Limit,
            FieldId::Cursor,
        ],
    },
    OperationSpec {
        id: OperationId::HistoryGet,
        label: "履歴 · 原文",
        meaning: "指定ノードの保存原文と出典を取得します。終了メッセージのみを選ぶと推論や途中説明を除きます。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Node,
            FieldId::LimitBytes,
            FieldId::Cursor,
            FieldId::Encoding,
            FieldId::EndMessageOnly,
            FieldId::Parts,
        ],
    },
    OperationSpec {
        id: OperationId::HistorySearch,
        label: "履歴 · 検索",
        meaning: "指定範囲の会話履歴を埋め込み検索し、原文と出典への参照を返します。API費用が発生する場合があります。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Query,
            FieldId::Thread,
            FieldId::Kinds,
            FieldId::ExcludeKinds,
            FieldId::From,
            FieldId::To,
            FieldId::Limit,
        ],
    },
    OperationSpec {
        id: OperationId::HistoryRelations,
        label: "履歴 · 関係",
        meaning: "履歴ノードの前後、出典、置換関係を、不変DAGと現在の採用経路を区別して表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Node,
            FieldId::Relation,
            FieldId::Limit,
            FieldId::Cursor,
        ],
    },
    OperationSpec {
        id: OperationId::ShellGet,
        label: "shell · 詳細",
        meaning: "対象コマンドの元のcommand、cwd、Run・Turn、保存状態と出力完了を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Execution],
    },
    OperationSpec {
        id: OperationId::ShellRead,
        label: "shell · 保存出力",
        meaning: "指定Executionの保存済みstdout・stderrを、文字コードとcursorを指定して読み取ります。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::Execution,
            FieldId::Cursor,
            FieldId::Encoding,
            FieldId::LimitBytes,
        ],
    },
    OperationSpec {
        id: OperationId::ShellWrite,
        label: "shell · 入力",
        meaning: "指定Executionのstdinへ入力を送ります。送信結果と相手の処理完了は別に確認します。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Execution, FieldId::Text, FieldId::Encoding],
    },
    OperationSpec {
        id: OperationId::ShellEof,
        label: "shell · 入力を閉じる",
        meaning: "指定Executionのstdinを閉じ、これ以降の入力を終了します。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Execution],
    },
    OperationSpec {
        id: OperationId::ShellStop,
        label: "shell · 停止",
        meaning: "指定ExecutionのOSプロセス群へ停止を要求します。Run全体の停止とは対象が異なります。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Execution],
    },
    OperationSpec {
        id: OperationId::AttachmentAdd,
        label: "添付 · 追加",
        meaning: "対象Threadへファイルまたは画像のパス参照を追加します。読み取り・更新・出典は既存の添付規則に従います。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Thread, FieldId::Path, FieldId::AttachmentKind],
    },
    OperationSpec {
        id: OperationId::AttachmentList,
        label: "添付 · 一覧",
        meaning: "対象Threadで選択されている添付参照とその状態を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Thread],
    },
    OperationSpec {
        id: OperationId::AttachmentRemove,
        label: "添付 · 除外",
        meaning: "指定attachment_idをThreadの添付選択から外します。外部ファイルは削除しません。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Thread, FieldId::Attachment],
    },
    OperationSpec {
        id: OperationId::Compact,
        label: "履歴 · 圧縮と編集",
        meaning: "安全な境界で圧縮を要求するか、指定範囲の退避・要約・削除・復元を行います。要約はモデルAPIを使用します。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::ContextEditPreview,
        route: Route::Owner,
        fields: &[
            FieldId::Thread,
            FieldId::CompactAction,
            FieldId::FromNode,
            FieldId::ToNode,
            FieldId::SummaryMaxChars,
            FieldId::SummaryMaxPercent,
            FieldId::DryRun,
        ],
    },
    OperationSpec {
        id: OperationId::IndexStatus,
        label: "索引 · 状態",
        meaning: "会話索引の設定、領域、処理件数と停止・失敗理由を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::IndexScope,
            FieldId::Space,
            FieldId::Threads,
            FieldId::Sources,
        ],
    },
    OperationSpec {
        id: OperationId::IndexPreview,
        label: "索引 · 費用確認",
        meaning: "指定資料の索引対象件数と読み取り可能な入力量を、埋め込みAPIを呼ばずに確認します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[
            FieldId::IndexScope,
            FieldId::Space,
            FieldId::Threads,
            FieldId::Sources,
        ],
    },
    OperationSpec {
        id: OperationId::IndexBackfill,
        label: "索引 · 過去履歴を追加",
        meaning: "明示した過去履歴の埋め込み索引を開始します。事前確認とAPI費用の確認が必要です。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::IndexCostPreview,
        route: Route::Owner,
        fields: &[
            FieldId::IndexScope,
            FieldId::Space,
            FieldId::Threads,
            FieldId::Sources,
        ],
    },
    OperationSpec {
        id: OperationId::IndexRetry,
        label: "索引 · 失敗分を再試行",
        meaning: "対象の失敗済み索引処理を、現在の設定で再試行します。API費用が発生する場合があります。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::IndexCostPreview,
        route: Route::Owner,
        fields: &[
            FieldId::IndexScope,
            FieldId::Space,
            FieldId::Threads,
            FieldId::Sources,
        ],
    },
    OperationSpec {
        id: OperationId::IndexRebuild,
        label: "索引 · 作り直す",
        meaning: "指定した資料と任意の次元数で候補索引を構築します。検証成功まで既存の有効領域を保持します。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::IndexCostPreview,
        route: Route::Owner,
        fields: &[
            FieldId::IndexScope,
            FieldId::Space,
            FieldId::Threads,
            FieldId::Sources,
            FieldId::Dimensions,
        ],
    },
    OperationSpec {
        id: OperationId::IndexProbe,
        label: "索引 · 接続と次元を試す",
        meaning: "一つの固定入力を埋め込みAPIへ送り、接続・次元の結果を保存します。API費用が発生する場合があります。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::PaidRequest,
        route: Route::Owner,
        fields: &[FieldId::Dimensions],
    },
    OperationSpec {
        id: OperationId::StorageStatus,
        label: "保存 · 状態",
        meaning: "指定カテゴリの論理使用量、上限、保持と回収に関する保存情報を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Owner,
        fields: &[FieldId::Category],
    },
    OperationSpec {
        id: OperationId::StorageCollect,
        label: "保存 · 回収",
        meaning: "指定カテゴリで回収可能な保存データを明示的に回収します。保護された原文と参照は保持します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::StorageCollect,
        route: Route::Owner,
        fields: &[FieldId::Category],
    },
    OperationSpec {
        id: OperationId::Exec,
        label: "作業 · JSON契約で実行",
        meaning: "保存Threadへ依頼し、最終JSON回答をローカルschemaで検証します。修復回数の範囲で追加のモデル・tool処理を行います。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::PaidRequest,
        route: Route::ComposedClient,
        fields: &[
            FieldId::Schema,
            FieldId::Thread,
            FieldId::Agents,
            FieldId::Cwd,
            FieldId::Connection,
            FieldId::Model,
            FieldId::Effort,
            FieldId::Parent,
            FieldId::Text,
            FieldId::MaxRepairs,
            FieldId::ExecResult,
            FieldId::Timeout,
            FieldId::SenderLabel,
        ],
    },
    OperationSpec {
        id: OperationId::ConfigInit,
        label: "設定 · テンプレート作成",
        meaning: "選択したconfigパスへテンプレートを作成します。既存ファイルは上書きしません。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Config,
        fields: &[],
    },
    OperationSpec {
        id: OperationId::ConfigCheck,
        label: "設定 · 接続確認",
        meaning: "設定ファイルと環境変数を読み、既定接続のモデル一覧を取得して設定を確認します。実行中の所有者への反映は再起動時です。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Config,
        fields: &[],
    },
    OperationSpec {
        id: OperationId::DocsList,
        label: "ガイド · 一覧",
        meaning: "この実行ファイルに同梱されたガイドを階層ごとに表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Docs,
        fields: &[FieldId::DocParent, FieldId::Limit, FieldId::Cursor],
    },
    OperationSpec {
        id: OperationId::DocsGet,
        label: "ガイド · 本文",
        meaning: "同梱ガイドの文書IDと任意の見出しから、版付き本文を読み取ります。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Docs,
        fields: &[
            FieldId::Document,
            FieldId::Section,
            FieldId::LimitBytes,
            FieldId::Cursor,
        ],
    },
    OperationSpec {
        id: OperationId::DocsSearch,
        label: "ガイド · 検索",
        meaning: "同梱ガイドの文字列を検索します。所有者や埋め込みAPIは不要です。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Docs,
        fields: &[
            FieldId::Query,
            FieldId::DocParent,
            FieldId::Limit,
            FieldId::Cursor,
        ],
    },
    OperationSpec {
        id: OperationId::RagInit,
        label: "資料検索 · DB作成",
        meaning: "指定した独立RAGデータベースを初期化します。会話の所有者は起動しません。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[],
    },
    OperationSpec {
        id: OperationId::RagSourceAdd,
        label: "資料検索 · ソース登録",
        meaning: "資料フォルダまたはファイルを名前付きソースとして登録し、除外パターンを保存します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[
            FieldId::Path,
            FieldId::SourceName,
            FieldId::Excludes,
            FieldId::ClearExcludes,
        ],
    },
    OperationSpec {
        id: OperationId::RagSourceList,
        label: "資料検索 · ソース一覧",
        meaning: "独立RAGに登録した資料ソースと除外設定を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[],
    },
    OperationSpec {
        id: OperationId::RagSourceRemove,
        label: "資料検索 · ソース解除",
        meaning: "指定した資料ソースの登録と対応する独立索引を削除します。元の外部資料は保持します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[FieldId::Source],
    },
    OperationSpec {
        id: OperationId::RagIndex,
        label: "資料検索 · 索引",
        meaning: "登録資料をkeywordまたはsemanticで索引化します。dry-runは実行対象を表示し、semantic処理はAPI費用が発生する場合があります。",
        effect: Effect::RuntimeControl,
        confirmation: Confirmation::PaidRequest,
        route: Route::Rag,
        fields: &[FieldId::Source, FieldId::DryRun, FieldId::SearchMode],
    },
    OperationSpec {
        id: OperationId::RagIngest,
        label: "資料検索 · 抽出済み本文を登録",
        meaning: "外部抽出した本文を、元ファイル・種類・任意のメタデータと対応付けて登録します。",
        effect: Effect::SavedMutation,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[
            FieldId::Document,
            FieldId::SourceFile,
            FieldId::IngestKind,
            FieldId::TextFile,
            FieldId::Text,
            FieldId::MetadataFile,
        ],
    },
    OperationSpec {
        id: OperationId::RagStatus,
        label: "資料検索 · 状態",
        meaning: "独立RAGの登録ソース、索引状態と未索引・失敗情報を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[],
    },
    OperationSpec {
        id: OperationId::RagInspect,
        label: "資料検索 · 文書詳細",
        meaning: "指定文書IDの保存された版、出典、抽出情報と索引情報を表示します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[FieldId::Document],
    },
    OperationSpec {
        id: OperationId::RagSearch,
        label: "資料検索 · 検索",
        meaning: "ガイド、会話、登録資料の明示した範囲を検索します。semantic・hybridではAPI費用が発生する場合があります。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[
            FieldId::Query,
            FieldId::RagScopes,
            FieldId::SearchMode,
            FieldId::Limit,
            FieldId::Cursor,
        ],
    },
    OperationSpec {
        id: OperationId::RagGet,
        label: "資料検索 · 原文",
        meaning: "検索結果の版付きrefを使って、対応する原文を有限ページで取得します。",
        effect: Effect::Inspect,
        confirmation: Confirmation::None,
        route: Route::Rag,
        fields: &[FieldId::Reference, FieldId::LimitBytes, FieldId::Cursor],
    },
];
mod cli_bindings;
pub(crate) static CLI_BINDINGS: std::sync::LazyLock<Vec<CliBinding>> =
    std::sync::LazyLock::new(cli_bindings::build);
impl OperationId {
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Status,
        Self::UsageGet,
        Self::ModelList,
        Self::ThreadList,
        Self::ThreadGet,
        Self::ThreadCreate,
        Self::ThreadFork,
        Self::ThreadDelete,
        Self::ThreadDeletePreview,
        Self::ThreadPrune,
        Self::ThreadSet,
        Self::MessageSend,
        Self::MessageReply,
        Self::MessageGet,
        Self::MessageCancel,
        Self::OperationGet,
        Self::RunGet,
        Self::RunRetry,
        Self::RunStop,
        Self::TurnGet,
        Self::Wait,
        Self::HistoryList,
        Self::HistoryGet,
        Self::HistorySearch,
        Self::HistoryRelations,
        Self::ShellList,
        Self::ShellGet,
        Self::ShellRead,
        Self::ShellWrite,
        Self::ShellEof,
        Self::ShellStop,
        Self::AttachmentAdd,
        Self::AttachmentList,
        Self::AttachmentRemove,
        Self::CompactList,
        Self::Compact,
        Self::IndexStatus,
        Self::IndexPreview,
        Self::IndexBackfill,
        Self::IndexRetry,
        Self::IndexRebuild,
        Self::IndexProbe,
        Self::StorageStatus,
        Self::StorageCollect,
        Self::Exec,
        Self::ConfigInit,
        Self::ConfigCheck,
        Self::DocsList,
        Self::DocsGet,
        Self::DocsSearch,
        Self::RagInit,
        Self::RagSourceAdd,
        Self::RagSourceList,
        Self::RagSourceRemove,
        Self::RagIndex,
        Self::RagIngest,
        Self::RagStatus,
        Self::RagInspect,
        Self::RagSearch,
        Self::RagGet,
    ];
    pub(crate) fn from_owner_command(command: &crate::protocol::Command) -> Option<Self> {
        use crate::protocol::Command;
        Some(match command {
            Command::OwnerShutdown(_) => return None,
            Command::Status(_) => Self::Status,
            Command::ModelList(_) => Self::ModelList,
            Command::ThreadList(_) => Self::ThreadList,
            Command::ThreadGet(_) => Self::ThreadGet,
            Command::CompactList(_) => Self::CompactList,
            Command::ShellList(_) => Self::ShellList,
            Command::UsageGet(_) => Self::UsageGet,
            Command::ThreadCreate(_) => Self::ThreadCreate,
            Command::ThreadFork(_) => Self::ThreadFork,
            Command::ThreadDelete(_) => Self::ThreadDelete,
            Command::ThreadDeletePreview(_) => Self::ThreadDeletePreview,
            Command::ThreadPrune(_) => Self::ThreadPrune,
            Command::ThreadSet(_) => Self::ThreadSet,
            Command::MessageReply(_) => Self::MessageReply,
            Command::MessageSend(_) => Self::MessageSend,
            Command::MessageCancel(_) => Self::MessageCancel,
            Command::MessageGet(_) => Self::MessageGet,
            Command::OperationGet(_) => Self::OperationGet,
            Command::RunGet(_) => Self::RunGet,
            Command::RunRetry(_) => Self::RunRetry,
            Command::RunStop(_) => Self::RunStop,
            Command::TurnGet(_) => Self::TurnGet,
            Command::Wait(_) => Self::Wait,
            Command::HistoryList(_) => Self::HistoryList,
            Command::HistoryGet(_) => Self::HistoryGet,
            Command::HistorySearch(_) => Self::HistorySearch,
            Command::HistoryRelations(_) => Self::HistoryRelations,
            Command::ShellGet(_) => Self::ShellGet,
            Command::ShellRead(_) => Self::ShellRead,
            Command::ShellWrite(_) => Self::ShellWrite,
            Command::ShellEof(_) => Self::ShellEof,
            Command::ShellStop(_) => Self::ShellStop,
            Command::AttachmentAdd(_) => Self::AttachmentAdd,
            Command::AttachmentList(_) => Self::AttachmentList,
            Command::AttachmentRemove(_) => Self::AttachmentRemove,
            Command::Compact(_) | Command::CompactPreview(_) => Self::Compact,
            Command::ContextEdit(_) => Self::Compact,
            Command::IndexStatus(_) => Self::IndexStatus,
            Command::IndexPreview(_) => Self::IndexPreview,
            Command::IndexBackfill(_) => Self::IndexBackfill,
            Command::IndexRetry(_) => Self::IndexRetry,
            Command::IndexProbe(_) => Self::IndexProbe,
            Command::IndexRebuild(_) => Self::IndexRebuild,
            Command::StorageStatus(_) => Self::StorageStatus,
            Command::StorageCollect(_) => Self::StorageCollect,
        })
    }
    pub(crate) fn spec(self) -> &'static OperationSpec {
        SPECS
            .iter()
            .find(|spec| spec.id == self)
            .expect("every operation has one const specification")
    }
    pub(crate) fn primary_cli(self) -> &'static CliBinding {
        CLI_BINDINGS
            .iter()
            .find(|binding| binding.operation == self && binding.alias_of.is_none())
            .expect("every operation has one primary CLI path")
    }
    pub(crate) fn label(self) -> &'static str {
        self.spec().label
    }
    pub(crate) fn label_text(self) -> Text {
        Text {
            key: TextKey::Label(self),
            fallback: self.label(),
        }
    }
    pub(crate) fn meaning_text(self) -> Text {
        Text {
            key: TextKey::Meaning(self),
            fallback: self.spec().meaning,
        }
    }
    pub(crate) fn enter_verb(self, effect: Effect) -> EnterVerb {
        if self == Self::MessageSend {
            EnterVerb::Send
        } else if effect == Effect::Inspect {
            EnterVerb::Display
        } else {
            EnterVerb::Execute
        }
    }
    pub(crate) fn request_id_policy(self) -> RequestIdPolicy {
        match self.spec().route {
            Route::Owner => RequestIdPolicy::OwnerOperation,
            Route::ComposedClient => RequestIdPolicy::ComposedUnavailable,
            _ => RequestIdPolicy::StandaloneUnavailable,
        }
    }
}
impl OperationSpec {
    pub(crate) fn field(&self, id: FieldId) -> Option<OperationField> {
        if !self.fields.contains(&id) {
            return None;
        }
        let field = FIELD_SPECS
            .iter()
            .find(|field| field.id == id)
            .expect("every typed field has metadata");
        let args = &self.id.primary_cli().arguments;
        let cli_default = args
            .iter()
            .filter(|arg| arg.field == id && (arg.takes_value || field.kind == FieldType::Boolean))
            .find_map(|arg| arg.default.as_deref());
        let tui_default = if self.id == OperationId::Wait && id == FieldId::SettlementStates {
            Some(DEFAULT_SETTLEMENT_STATES)
        } else if id == FieldId::CompactAction {
            Some("deferred")
        } else if id == FieldId::Limit
            && matches!(self.id, OperationId::ThreadList | OperationId::HistoryList)
        {
            Some("20")
        } else {
            cli_default
        };
        let default_origin = match (self.id, id) {
            (
                OperationId::ThreadCreate,
                FieldId::Cwd
                | FieldId::Agents
                | FieldId::Connection
                | FieldId::Model
                | FieldId::Effort,
            ) => DefaultOrigin::ParentOrOwnerDefaults,
            (OperationId::Exec, FieldId::Cwd) => DefaultOrigin::ProcessCwd,
            (OperationId::HistorySearch, FieldId::Thread) => DefaultOrigin::None,
            (OperationId::ShellRead, FieldId::Encoding) => DefaultOrigin::SavedShellEncoding,
            (OperationId::Wait, FieldId::Conditions) => DefaultOrigin::SelectedTurn,
            _ => field.default_origin,
        };
        let choices = if id == FieldId::CompactAction {
            vec![
                "deferred", "maximal", "spill", "summary", "trim", "omit", "restore",
            ]
        } else if id == FieldId::AttachmentKind {
            vec!["file", "image"]
        } else {
            args.iter()
                .filter(|arg| arg.field == id)
                .find(|arg| !arg.choices.is_empty())
                .map(|arg| arg.choices.iter().map(String::as_str).collect())
                .unwrap_or_default()
        };
        Some(OperationField {
            field,
            default_origin,
            cli_default,
            tui_default,
            choices,
        })
    }
}
impl FieldSpec {
    pub(crate) fn label_text(self, operation: OperationId) -> Text {
        Text {
            key: TextKey::FieldLabel(operation, self.id),
            fallback: self.label,
        }
    }
    pub(crate) fn help_text(self, operation: OperationId) -> Text {
        Text {
            key: TextKey::FieldHelp(operation, self.id),
            fallback: self.help,
        }
    }
}
impl Effect {
    pub(crate) fn list_marker(self) -> Text {
        Text {
            key: TextKey::EffectMarker(self),
            fallback: match self {
                Self::Inspect => "",
                Self::SavedMutation => "[変更] ",
                Self::RuntimeControl => "[制御] ",
            },
        }
    }
    pub(crate) fn text(self) -> Text {
        Text {
            key: TextKey::Effect(self),
            fallback: match self {
                Self::Inspect => "保存された状態や原文を表示します。",
                Self::SavedMutation => "保存データや設定を変更します。",
                Self::RuntimeControl => "モデル・shell・索引などの処理を開始、再開、制御します。",
            },
        }
    }
}
impl EnterVerb {
    pub(crate) fn text(self) -> Text {
        Text {
            key: TextKey::Enter(self),
            fallback: match self {
                Self::Display => "表示",
                Self::Execute => "実行",
                Self::Send => "送信",
            },
        }
    }
}
impl Term {
    pub(crate) fn text(self) -> Text {
        Text {
            key: TextKey::Term(self),
            fallback: match self {
                Self::Run => {
                    "作業（Run）は依頼を進める実行単位です。Turnの回答後も、所有するshellや子Runの終了まで続くことがあります。"
                }
                Self::Turn => {
                    "応答処理（Turn）は一つの回答へ進む区間です。複数のモデル要求やtool実行を含み、Runと別に完了・停止・復旧待ちになります。"
                }
                Self::Message => {
                    "入力（Message）は保存された依頼です。受付、Run・Turnへの採用、回答完了は別の段階です。"
                }
                Self::Execution => {
                    "コマンド実行（Execution）はshellツールが開始した一つのOSプロセス群です。観測CLI自身のIDと対象作業IDを区別します。"
                }
            },
        }
    }
}
impl Discovery {
    pub(crate) fn text(self) -> Text {
        Text {
            key: TextKey::Discovery(self),
            fallback: match self {
                Self::None => "操作の入力条件を確認してください。",
                Self::ThreadList => "thread listまたは会話一覧でThread IDを確認できます。",
                Self::RunFromThread => "thread getまたはstatusで対象のRun IDを確認できます。",
                Self::TurnFromRun => "run getで対象のTurn IDを確認できます。",
                Self::HistoryList => "history listで保存ノードIDと採用経路を確認できます。",
                Self::MessageReceipt => {
                    "message sendの返却message_idまたはキューの入力IDを使います。"
                }
                Self::OperationReceipt => "受付結果またはエラーのoperation_idを使います。",
                Self::ShellFromToolOrStatus => {
                    "shellツールの返却execution_idまたはstatusの作業一覧から確認できます。"
                }
                Self::SameReadCursor => {
                    "直前の同じ対象の読み取り結果にあるcursorを使います。空は先頭です。"
                }
                Self::Status => "statusで保存された作業と関連IDを確認できます。",
                Self::Filesystem => {
                    "対象ファイル／フォルダのパスを確認してください。読み取りはworkerで行います。"
                }
                Self::ConnectionConfig => {
                    "設定ファイルのconnections名を確認できます。apiはresponsesまたはchat_completionsです。"
                }
                Self::ModelList => "model listで接続が返す利用可能モデル名を確認できます。",
                Self::AttachmentList => "attachment listでattachment_idを確認できます。",
                Self::IndexStatus => "index statusで領域ID、現在の設定と次元を確認できます。",
                Self::DocsList => "docs listで同梱ガイドのIDと階層を確認できます。",
                Self::DocumentList => {
                    "同梱ガイドはdocs list、独立資料はrag searchで文書IDを確認できます。"
                }
                Self::DocsSections => "docs getの見出し一覧からsectionを確認できます。",
                Self::RagSourceList => "rag source listで登録ソースのID・名前を確認できます。",
                Self::RagSearch => "rag searchが返す版付きrefを使います。",
            },
        }
    }
}
/// Mode-dependent meaning stays explicit Rust; no generic field evaluator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompactMode {
    Deferred,
    Maximal,
    Spill,
    Summary,
    Trim,
    Omit,
    Restore,
}
pub(crate) fn compact_effect(mode: CompactMode) -> Effect {
    match mode {
        CompactMode::Deferred | CompactMode::Summary => Effect::RuntimeControl,
        _ => Effect::SavedMutation,
    }
}
pub(crate) fn rag_index_effect(dry_run: bool) -> Effect {
    if dry_run {
        Effect::Inspect
    } else {
        Effect::RuntimeControl
    }
}

#[cfg(test)]
#[path = "operations/catalogue_tests.rs"]
mod catalogue_tests;
