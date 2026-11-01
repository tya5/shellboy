//! Typed, display-only sentences. None of this provenance is serialized.
use crate::config::UiLanguage;
use super::rows::RowKind;

macro_rules! sentences {
    ($($name:ident => ($en:literal, $ja:literal)),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Text { $($name),* }
        impl Text {
            pub fn render(self, language: UiLanguage) -> &'static str {
                match (language, self) {
                    $((UiLanguage::En, Self::$name) => $en,
                      (UiLanguage::Ja, Self::$name) => $ja),*
                }
            }
        }
    };
}

sentences! {
    User => ("You", "あなた"), Reasoning => ("Reasoning", "推論"),
    ToolResult => ("Execution result", "実行結果"), Event => ("State change", "状態変更"),
    ReviewChange => ("Review the change", "変更内容を確認"), History => ("History", "履歴"),
    Summary => ("Summary", "要約"), Spill => ("Original reference", "原文参照"),
    Interrupted => ("Previous work was interrupted", "前回処理の中断情報"),
    Uncached => ("Body not cached | Open details to load the original", "本文未キャッシュ | 詳細から原文を取得"),
    Details => ("Review content and references in details", "詳細で内容・参照を確認"),
    Collapsed => (". Body collapsed | Enter to open", ". 本文を折りたたみ | Enterで開く"),
    StreamGap => ("Live updates were missed. Reload after the final body is saved | F6 Details", "途中表示の更新欠落を検出。確定本文の保存後に再取得 | F6 詳細"),
    UnknownExtent => ("Output extent and omitted amount are unknown | F6 Details", "出力の表示範囲・省略量は未確認 | F6 詳細"),
    CommandPreview => ("Command preview | o Original command (paged)", "コマンド（プレビュー） | o コマンド原文（ページ表示）"),
    OutputFailed => ("Output retrieval failed", "出力取得に失敗"),
    RefreshFailed => ("Refresh failed | Showing the previous result", "更新失敗 | 前回の取得内容を表示"),
    Refreshing => ("Refreshing | Showing the previous result", "再取得中 | 前回の取得内容を表示"),
    OutputRetry => ("Output retrieval failed | Close and reopen to retry | F6 Original", "出力取得に失敗 | 閉じて開くと再試行 | F6 原文"),
    OutputLoading => ("Loading output | F6 Original", "出力を取得中 | F6 原文"),
    OutputUnavailable => ("Could not retrieve output | F6 Original", "出力を取得できませんでした | F6 原文"),
    OutputEvicted => ("Output not retained | Close and reopen to reload | F6 Original", "出力未保持 | 閉じて開くと再取得 | F6 原文"),
    OutputOriginal => ("Review output in the original | F6 Details", "出力は原文で確認 | F6 詳細"),
    SourceOutsideWindow => ("Source tool row is outside this window | F6 Saved reference", "元のtool行は表示範囲外です | F6で保存済み参照を確認"),
    ElapsedUnavailable => ("Elapsed time unavailable", "経過未取得"),
    Background => ("Background", "背景"), HumanBackground => ("Background (user)", "背景（利用者）"),
    InputBackground => ("Background (additional input)", "背景（追加入力）"),
    ResultUncertain => ("External execution results are unconfirmed | F6 Saved targets", "外部での実行結果は未確認 | F6で保存済みの対象を確認"),
    NoReplay => ("Commands will not be replayed automatically", "自動での再実行はしません"),
    SavedTargets => ("Review saved targets in F6 Details / Original", "保存した対象はF6 詳細・原文で確認"),
    WaitInterrupted => ("Previous wait observation was interrupted", "前回の待機観測は中断済み"),
    SavedObservation => ("Observation saved | F6 Original", "観測の記録を保持 | F6 原文"),
    UnknownCount => ("Review the count in the original", "件数は原文で確認"),
    PreviousShutdown => ("Previous shutdown record", "前回の終了時の記録"),
    PreviousInterruption => ("Previous interruption record", "前回の中断記録"),
    PreviousRecord => ("Previous work record", "前回処理の保存記録"),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Text(Text),
    UnknownTitle { kind: String },
    Reclaimed { reason: String },
    Summary { count: usize, replaced: bool },
    SourceCount { count: usize },
    Excerpt { rows: usize, omitted: usize },
    SourceWindow { start: usize, end: usize, total: usize, live: bool },
    BodyOmitted { count: usize },
    Exit { code: i64 },
    Elapsed { ms: u64, decimal: bool },
    EventTitle { kind: String },
    SectionTitle { section: String },
    SectionCount { section: String, count: u64 },
    DetailCount { count: usize },
    WaitCount { count: usize },
    RetryError { refresh: bool, error: String },
    Change { field: String, before: String, after: String },
}
impl Message {
    pub fn render(&self, language: UiLanguage) -> String {
        let en = language == UiLanguage::En;
        match self {
            Self::Text(text) => text.render(language).into(),
            Self::UnknownTitle { kind } => if en { format!("Unsupported item: {kind}") } else { format!("未対応の項目: {kind}") },
            Self::Reclaimed { reason } => if en { format!("Original reclaimed | {reason}") } else { format!("原文回収済み | {reason}") },
            Self::Summary { count, replaced } => match (en, replaced) {
                (true, true) => format!("Summary | {count} replaced | {count} sources | F6>O Original"),
                (true, false) => format!("Summary | {count} sources | F6>O Original"),
                (false, true) => format!("要約 | 置換{count}件 | 出典{count}件 | F6>O 原文"),
                (false, false) => format!("要約 | 出典{count}件 | F6>O 原文"),
            },
            Self::SourceCount { count } => match (en, count) {
                (true, 1) => "  1 original | F6 Details".into(),
                (true, _) => format!("  {count} originals | F6 Details"),
                (false, _) => format!("  原文{count}件 | F6 詳細"),
            },
            Self::Excerpt { rows, omitted } => if en { format!(". Output excerpt ({omitted} of {rows} rows omitted) | F6 Details") } else { format!(". 出力抜粋（表示{rows}行中{omitted}行省略）| F6 詳細") },
            Self::SourceWindow { start, end, total, live } => match (en, live) {
                (true, true) => format!("Live display window {start}-{end}/{total} characters | F6 Details"),
                (true, false) => format!("Original window {start}-{end}/{total} characters | F6 Details"),
                (false, true) => format!("生成中の表示範囲 {start}-{end}/{total}文字 | F6 詳細"),
                (false, false) => format!("原文の取得範囲 {start}-{end}/{total}文字 | F6 詳細"),
            },
            Self::BodyOmitted { count } => if en { format!("{count} body/output characters omitted | F6 Details") } else { format!("本文・出力{count}文字省略 | F6 詳細") },
            Self::Exit { code } => if en { format!("Exit {code}") } else { format!("終了 {code}") },
            Self::Elapsed { ms, decimal } => match (en, decimal) {
                (true, true) => format!("{}.{:01}s", ms / 1000, (ms % 1000) / 100),
                (false, true) => format!("{}.{:01}秒", ms / 1000, (ms % 1000) / 100),
                (true, false) => format!("{}s", ms / 1000),
                (false, false) => format!("{}秒", ms / 1000),
            },
            Self::EventTitle { kind } => event_title(kind, language).into(),
            Self::SectionTitle { section } => if en { format!("Previous saved record | {}", section_label(section, language)) } else { format!("前回の保存記録 | {}", section_label(section, language)) },
            Self::SectionCount { section, count } => if en { format!("{}: {count}", section_label(section, language)) } else { format!("{} {count}件", section_label(section, language)) },
            Self::DetailCount { count } => if en { format!("{count} items in this original | F6 Target details") } else { format!("この原文に{count}件 | 個別の対象はF6 詳細") },
            Self::WaitCount { count } => if en { format!("{count} wait conditions | F6 Referenced targets for current state") } else { format!("待機条件{count}件 | 対象の現在状態はF6の参照先で確認") },
            Self::RetryError { refresh, error } => match (en, refresh) {
                (true, true) => format!("Refresh failed | Showing the previous result: {error} | Close and reopen to retry"),
                (true, false) => format!("Output retrieval failed: {error} | Close and reopen to retry"),
                (false, true) => format!("更新失敗 | 前回の取得内容を表示: {error} | 閉じて開くと再試行"),
                (false, false) => format!("出力取得に失敗: {error} | 閉じて開くと再試行"),
            },
            Self::Change { field, before, after } => format!("{field}: {before} > {after}"),
        }
    }
    fn retained_bytes(&self) -> usize {
        match self {
            Self::UnknownTitle { kind } | Self::EventTitle { kind } => kind.capacity(),
            Self::Reclaimed { reason } => reason.capacity(),
            Self::SectionTitle { section } | Self::SectionCount { section, .. } => section.capacity(),
            Self::RetryError { error, .. } => error.capacity(),
            Self::Change { field, before, after } => field.capacity() + before.capacity() + after.capacity(),
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresentationMessages {
    pub title: Option<Message>,
    pub bodies: Vec<(usize, Vec<Message>)>,
}
impl PresentationMessages {
    pub fn retained_bytes(&self) -> usize {
        self.title.as_ref().map_or(0, Message::retained_bytes)
            + self.bodies.capacity() * std::mem::size_of::<(usize, Vec<Message>)>()
            + self.bodies.iter().map(|(_, messages)| messages.capacity() * std::mem::size_of::<Message>()
                + messages.iter().map(Message::retained_bytes).sum::<usize>()).sum::<usize>()
    }
}

pub fn row_label(kind: RowKind, language: UiLanguage) -> &'static str {
    if language == UiLanguage::Ja { return kind.spec().1; }
    match kind {
        RowKind::Human => "User", RowKind::External => "External", RowKind::Parent => "Parent",
        RowKind::SelfModel => "Self", RowKind::OtherModel => "Model", RowKind::Revisit => "Revisit",
        RowKind::Assistant => "Answer", RowKind::ToolCall => "Call", RowKind::Running => "Running",
        RowKind::Unknown => "Observation unknown", RowKind::Success => "Exited", RowKind::Failure => "Failed",
        RowKind::Cancelled => "Cancelled / not started", RowKind::Notice => "Notice", RowKind::Summary => "Summary",
        RowKind::Spill => "Spill", RowKind::LiveWork => "Live work",
    }
}
pub fn event_title(kind: &str, language: UiLanguage) -> &'static str {
    if language == UiLanguage::Ja { return super::events::event_title(kind); }
    match kind {
        "revisit" => "Revisit resumed", "index_state" => "Index state",
        "previous_instance_interrupted" => "Previous interruption record", "app_shutdown" => "Saved shutdown record",
        "turn_started" => "Turn started", "run_started" => "Run started",
        "thread_settings" | "connection_changed" | "model_changed" | "settings_changed" => "Connection / settings changed",
        "instructions_observed" | "instruction_changed" | "agents_changed" | "agents_reference" => "Instruction files updated",
        "recovery_resumed" => "Recovery resumed", "context_changed" => "State change",
        "child_recovery_wait" => "Child awaiting recovery", "recovery_wait" => "Awaiting recovery",
        "child_turn_completed" => "Child answer", "child_run_ended" => "Child run ended",
        "shell_ended" => "Command ended", "child_report" => "Child progress report", _ => "Other state change",
    }
}
pub fn section_label(section: &str, language: UiLanguage) -> &str {
    match (language, section) {
        (UiLanguage::En, "runs") => "Runs", (UiLanguage::Ja, "runs") => "作業",
        (UiLanguage::En, "turns") => "Turns", (UiLanguage::Ja, "turns") => "応答",
        (UiLanguage::En, "shells") => "Shells", (UiLanguage::Ja, "shells") => "shell",
        (UiLanguage::En, "tool_calls") => "Tool calls", (UiLanguage::Ja, "tool_calls") => "ツール呼出し",
        (UiLanguage::En, "revisits") => "Revisits", (UiLanguage::Ja, "revisits") => "再訪",
        (UiLanguage::En, "owned_children") => "Owned children", (UiLanguage::Ja, "owned_children") => "所有する子",
        (UiLanguage::En, "unreflected_messages") => "Unreflected messages", (UiLanguage::Ja, "unreflected_messages") => "未反映指示",
        (UiLanguage::En, "cancelled_waits") => "Cancelled waits", (UiLanguage::Ja, "cancelled_waits") => "解除済み待機",
        (UiLanguage::En, "pending_operations_with_unknown_targets") => "Pending operations with unknown targets",
        (UiLanguage::Ja, "pending_operations_with_unknown_targets") => "対象不明の未完了操作",
        _ => section,
    }
}
