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
    ReasoningExpanded => ("▸ Reasoning (expanded)", "▸ 推論（展開中）"),
    ContextTrim => ("Input scope reduced (original retained)", "入力範囲を縮小（原文は保持）"),
    MetadataLimited => ("Display details limited | F6 Original for complete facts", "表示情報を制限 | 完全な情報はF6 原文"),
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
    Reasoning { count: usize },
    Exit { code: i64 },
    Elapsed { ms: u64, decimal: bool },
    EventTitle { kind: String },
    SectionTitle { section: String },
    SectionCount { section: String, count: u64 },
    DetailCount { count: usize },
    WaitCount { count: usize },
    RetryError { refresh: bool, error: String },
    Raw { text: String },
    EventIdentity { kind: String, identity: String, status: Option<String>, code: Option<i64>, elapsed_ms: Option<u64> },
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
            Self::Reasoning { count } => if en { format!("▸ Reasoning {count} characters | Collapsed /reasoning") } else { format!("▸ 推論 {count}文字 | 折りたたみ /reasoning") },
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
            Self::Raw { text } => text.clone(),
            Self::EventIdentity { kind, identity, status, code, elapsed_ms } => {
                let title = event_title(kind, language);
                let state = status.as_deref().unwrap_or("");
                let outcome = code.map(|code| Message::Exit { code }.render(language)).unwrap_or_else(|| state.into());
                let elapsed = elapsed_ms.map(|ms| Message::Elapsed { ms, decimal: false }.render(language)).unwrap_or_default();
                [title, identity, &outcome, &elapsed].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" | ")
            }
            Self::Change { field, before, after } => format!("{}: {before} > {after}", field_label(field, language)),
        }
    }
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Raw { text } => text.capacity(),
            Self::EventIdentity { kind, identity, status, .. } => kind.capacity() + identity.capacity() + status.as_ref().map_or(0, String::capacity),
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
pub fn event_title(kind: &str, language: UiLanguage) -> &str {
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

fn field_label(field: &str, language: UiLanguage) -> &str {
    match (language, field) {
        (UiLanguage::En, "connection" | "profile") => "Connection",
        (UiLanguage::Ja, "connection" | "profile") => "接続",
        (UiLanguage::En, "model") => "Model", (UiLanguage::Ja, "model") => "モデル",
        (UiLanguage::En, "cwd") => "Working directory", (UiLanguage::Ja, "cwd") => "作業場所",
        (UiLanguage::En, "effort") => "Reasoning effort", (UiLanguage::Ja, "effort") => "推論effort",
        (UiLanguage::En, "title") => "Title", (UiLanguage::Ja, "title") => "件名",
        (UiLanguage::En, "path") => "Instruction file", (UiLanguage::Ja, "path") => "指示ファイル",
        (UiLanguage::En, "present") => "Present", (UiLanguage::Ja, "present") => "存在",
        (UiLanguage::En, "active_state") => "Index state", (UiLanguage::Ja, "active_state") => "索引状態",
        (UiLanguage::En, "paused") => "Paused", (UiLanguage::Ja, "paused") => "一時停止",
        (UiLanguage::En, "pause_kind") => "Pause reason", (UiLanguage::Ja, "pause_kind") => "停止理由",
        (UiLanguage::En, "dimension") => "Dimensions", (UiLanguage::Ja, "dimension") => "次元数",
        _ => field,
    }
}

/// Called only while decoding a known application event, never on arbitrary text.
pub(super) fn event_messages(value: &serde_json::Value, saved_kind: &str, interrupted: bool) -> (Message, Vec<Message>) {
    use serde_json::Value;
    let clipped = |text: &str| super::sanitize_plain(&super::short(text, 240), 0).0;
    let mut body = Vec::new();
    if interrupted {
        if saved_kind == "interrupted_wait" {
            body.push(value.get("conditions").and_then(Value::as_array)
                .map(|v| Message::WaitCount { count: v.len() }).unwrap_or(Message::Text(Text::SavedObservation)));
            return (Message::Text(Text::WaitInterrupted), body);
        }
        if let Some(section) = value.get("section").and_then(Value::as_str) {
            body.push(value.get("items").and_then(Value::as_array)
                .map(|v| Message::DetailCount { count: v.len() }).unwrap_or(Message::Text(Text::UnknownCount)));
            return (Message::SectionTitle { section: clipped(section) }, body);
        }
        let change = &value["change"];
        // Safety facts precede optional counts in constrained viewports.
        if change["result_uncertain"] == Value::Bool(true) { body.push(Message::Text(Text::ResultUncertain)); }
        if change["automatic_replay"] == Value::Bool(false) { body.push(Message::Text(Text::NoReplay)); }
        for section in ["runs", "turns", "shells", "tool_calls", "revisits", "owned_children", "unreflected_messages", "cancelled_waits", "pending_operations_with_unknown_targets"] {
            if let Some(count) = change[section]["count"].as_u64().filter(|c| *c > 0) {
                body.push(Message::SectionCount { section: section.into(), count });
            }
        }
        if body.is_empty() { body.push(Message::Text(Text::SavedTargets)); }
        let title = match value["kind"].as_str() {
            Some("app_shutdown") => Text::PreviousShutdown,
            Some("previous_instance_interrupted") => Text::PreviousInterruption,
            _ => Text::PreviousRecord,
        };
        return (Message::Text(title), body);
    }
    let event = if value["kind"] == "context_changed" { value.get("payload").unwrap_or(value) } else { value };
    let kind = event["kind"].as_str().unwrap_or("context_changed");
    let payload = event.get("payload").unwrap_or(event);
    let identity = ["execution_id", "child_thread_id", "thread_id", "run_id", "turn_id"].iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str)).map(clipped);
    let title = if let Some(identity) = identity {
        Message::EventIdentity { kind: clipped(kind), identity,
            status: payload.get("status").and_then(Value::as_str).map(clipped),
            code: payload.get("exit_code").and_then(Value::as_i64),
            elapsed_ms: payload.get("elapsed_ms").and_then(Value::as_u64) }
    } else { Message::EventTitle { kind: clipped(kind) } };
    let change = &event["change"];
    for key in ["connection", "model", "cwd", "effort", "title", "profile", "active_state", "paused", "pause_kind", "dimension", "path", "present"] {
        if let Some(after) = change["after"].get(key) {
            let before = change["before"].get(key).unwrap_or(&Value::Null);
            if before != after {
                let show = |v: &Value| clipped(v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()).as_str());
                body.push(Message::Change { field: key.into(), before: show(before), after: show(after) });
            }
        }
    }
    // Provider/model messages and saved instructions are verbatim data, not catalogue keys.
    for key in ["message", "detail", "instruction"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) { body.push(Message::Raw { text: clipped(text) }); }
    }
    if body.is_empty() { body.push(title.clone()); }
    (title, body)
}

impl PresentationMessages {
    /// Admit complete sentences only; never trim a safety sentence or its arguments.
    pub(super) fn set_body(&mut self, index: usize, messages: Vec<Message>) -> bool {
        self.bodies.push((index, messages));
        if self.retained_bytes() <= 8 * 1024 { return true; }
        self.bodies.pop();
        self.bodies.shrink_to_fit();
        false
    }
    pub(super) fn clear(&mut self) { *self = Self::default(); }
}

pub fn tool_heading(tool: &super::ToolPresentation, request: bool, language: UiLanguage) -> String {
    if language == UiLanguage::Ja { return if request { tool.call_heading() } else { tool.heading() }; }
    let current = tool.current_state.as_deref().filter(|s| *s != "observation_unavailable");
    let state = current.unwrap_or(&tool.status);
    let code = if current.is_some() { tool.current_exit_code } else { tool.exit_code };
    let kind = super::rows::tool_kind(state, tool.requires_attention());
    let identity = tool.ordinal.map(|n| format!("#{}", n + 1)).unwrap_or_else(|| format!("Ref {}", super::short(&tool.call_id, 16)));
    let command = if tool.command.is_empty() { "Original command in details".into() } else { super::short(&tool.command.split_whitespace().collect::<Vec<_>>().join(" "), 120) };
    let mark = if request { RowKind::ToolCall.mark() } else { kind.mark() };
    let mut facts = vec![format!("{mark} {identity} {command}")];
    if !request || current.is_some() {
        facts.push(tool_state(state, code, language));
    }
    if !request {
        if current.is_some() && (state != tool.status || code != tool.exit_code) {
            facts.push(format!("At return: {}", tool_state(&tool.status, tool.exit_code, language)));
        } else if tool.current_state.as_deref() == Some("observation_unavailable") {
            facts.push("Current state unconfirmed".into());
        }
        if let Some(ms) = tool.elapsed_ms { facts.push(Message::Elapsed { ms, decimal: true }.render(language)); }
        if let Some(id) = &tool.execution_id { facts.push(format!("Execution {} | F6", super::short(id, 8))); }
    }
    facts.join(" | ")
}
pub fn tool_state(state: &str, code: Option<i64>, language: UiLanguage) -> String {
    if language == UiLanguage::Ja { return super::tool_state_text(state, code); }
    let label = match state {
        "requested" => "Requested", "pending" => "Pending", "starting" | "running_current" => "Running",
        "running" => "Background at return", "background_current" => "Running in background", "stopping" => "Stop requested",
        "completed" => "Result saved", "exited" if code.is_none() => "Exited (code unknown)", "exited" => "Exit",
        "failed" | "error" => "Failed", "cancelled" => "Cancelled / not executed", "not_started" => "Not started",
        "interrupted" => "Interrupted / result uncertain", "stopped" => "Stopped", _ => "State in details",
    };
    code.map(|code| format!("{label} {code}")).unwrap_or_else(|| label.into())
}
pub fn tool_reason(tool: &super::ToolPresentation, language: UiLanguage) -> String {
    use super::ToolReasonKind as R;
    if language == UiLanguage::Ja { return tool.reason.clone(); }
    match tool.reason_kind {
        R::RunStopped => "Cancelled because the run stopped",
        R::PriorCallInterrupted => "Cancelled after an earlier call failed or foreground wait was interrupted",
        R::PriorCallFailed => "Not executed because an earlier call failed",
        R::PriorOutputIncomplete => "Not executed because earlier output was incomplete",
        R::PriorWaitNewInput => "Not executed after additional input released the earlier wait",
        R::PriorWaitHumanBackground => "Not executed after the user backgrounded the earlier call",
        R::PriorWaitReleased => "Not executed due to the earlier wait outcome",
        R::HumanTurnInterrupted => "User interrupted the turn",
        R::PriorTurnInterrupted => "Not executed because the user interrupted the turn",
        R::OwnerShutdown => "Not executed because the application was shutting down",
        R::GroupValidation => "Not executed because tool group validation failed",
        R::AdditionalInput => "Backgrounded after additional input", R::HumanBackgrounded => "Backgrounded by the user",
        R::ForegroundTimeout => "Backgrounded at the foreground wait time limit", R::OutputLimit => "Output limit reached",
        R::Unknown => return tool.reason.clone(),
    }.into()
}
