//! Typed, display-only sentences. None of this provenance is serialized.
use super::rows::RowKind;
use crate::config::UiLanguage;

// Language-independent provenance tokens; copy uses the one shared UI macro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    ReasoningExpanded,
    ContextTrim,
    MetadataLimited,
    NoticeSourceUnavailable,
    NoticeCallUnavailable,
    NoticeCommandUnrecorded,
    NoticeDeletedThread,
    NoticeThreadUnavailable,
    NoticeUntitled,
    User,
    Reasoning,
    ToolResult,
    Event,
    ReviewChange,
    History,
    Summary,
    Spill,
    Interrupted,
    Uncached,
    Details,
    Collapsed,
    StreamGap,
    UnknownExtent,
    CommandPreview,
    OutputFailed,
    RefreshFailed,
    Refreshing,
    OutputRetry,
    OutputLoading,
    OutputUnavailable,
    OutputEvicted,
    OutputOriginal,
    SourceOutsideWindow,
    ElapsedUnavailable,
    Background,
    HumanBackground,
    InputBackground,
    OutputIncomplete,
    AwaitingRecovery,
    Delivered,
    ResultUnknown,
    IndexPaused,
    ResultUncertain,
    NoReplay,
    SavedTargets,
    WaitInterrupted,
    SavedObservation,
    UnknownCount,
    PreviousShutdown,
    PreviousInterruption,
    PreviousRecord,
    ReceivedResponse,
    StreamDifference,
}
impl Text {
    pub fn render(self, language: UiLanguage) -> &'static str {
        match self {
            Self::ReasoningExpanded => {
                crate::ui::i18n::local!(language, "▸ 推論（展開中）", "▸ Reasoning (expanded)")
            }
            Self::ContextTrim => crate::ui::i18n::local!(
                language,
                "入力範囲を縮小（原文は保持）",
                "Input scope reduced (original retained)"
            ),
            Self::MetadataLimited => crate::ui::text_width::glyphs::localized!(
                language,
                "表示情報を制限 · 完全な情報はF6 原文",
                "表示情報を制限 | 完全な情報はF6 原文",
                "Display details limited · F6 Original for complete facts",
                "Display details limited | F6 Original for complete facts"
            ),
            Self::NoticeSourceUnavailable => crate::ui::i18n::local!(
                language,
                "元の結果を参照できません",
                "Source result is unavailable"
            ),
            Self::NoticeCallUnavailable => crate::ui::i18n::local!(
                language,
                "呼出し情報を参照できません",
                "Call information is unavailable"
            ),
            Self::NoticeCommandUnrecorded => {
                crate::ui::i18n::local!(language, "コマンド未記録", "Command not recorded")
            }
            Self::NoticeDeletedThread => crate::ui::i18n::local!(
                language,
                "参照先の会話は削除済み",
                "Referenced conversation was deleted"
            ),
            Self::NoticeThreadUnavailable => crate::ui::i18n::local!(
                language,
                "参照先の会話を取得できません",
                "Referenced conversation is unavailable"
            ),
            Self::NoticeUntitled => crate::ui::i18n::local!(language, "無題", "Untitled"),
            Self::User => crate::ui::i18n::local!(language, "あなた", "You"),
            Self::Reasoning => crate::ui::i18n::local!(language, "推論", "Reasoning"),
            Self::ToolResult => crate::ui::i18n::local!(language, "実行結果", "Execution result"),
            Self::Event => crate::ui::i18n::local!(language, "状態変更", "State change"),
            Self::ReviewChange => {
                crate::ui::i18n::local!(language, "変更内容を確認", "Review the change")
            }
            Self::History => crate::ui::i18n::local!(language, "履歴", "History"),
            Self::Summary => crate::ui::i18n::local!(language, "要約", "Summary"),
            Self::Spill => crate::ui::i18n::local!(language, "原文参照", "Original reference"),
            Self::Interrupted => crate::ui::i18n::local!(
                language,
                "前回処理の中断情報",
                "Previous work was interrupted"
            ),
            Self::Uncached => crate::ui::text_width::glyphs::localized!(
                language,
                "本文未キャッシュ · 詳細から原文を取得",
                "本文未キャッシュ | 詳細から原文を取得",
                "Body not cached · Open details to load the original",
                "Body not cached | Open details to load the original"
            ),
            Self::Details => crate::ui::i18n::local!(
                language,
                "詳細で内容・参照を確認",
                "Review content and references in details"
            ),
            Self::Collapsed => crate::ui::text_width::glyphs::localized!(
                language,
                "… 本文を折りたたみ · Enterで開く",
                ". 本文を折りたたみ | Enterで開く",
                "… Body collapsed · Enter to open",
                ". Body collapsed | Enter to open"
            ),
            Self::StreamGap => crate::ui::text_width::glyphs::localized!(
                language,
                "途中表示の更新欠落を検出。確定本文の保存後に再取得 · F6 詳細",
                "途中表示の更新欠落を検出。確定本文の保存後に再取得 | F6 詳細",
                "Live updates were missed. Reload after the final body is saved · F6 Details",
                "Live updates were missed. Reload after the final body is saved | F6 Details"
            ),
            Self::UnknownExtent => crate::ui::text_width::glyphs::localized!(
                language,
                "出力の表示範囲・省略量は未確認 · F6 詳細",
                "出力の表示範囲・省略量は未確認 | F6 詳細",
                "Output extent and omitted amount are unknown · F6 Details",
                "Output extent and omitted amount are unknown | F6 Details"
            ),
            Self::CommandPreview => crate::ui::text_width::glyphs::localized!(
                language,
                "コマンド（プレビュー） · o コマンド原文（ページ表示）",
                "コマンド（プレビュー） | o コマンド原文（ページ表示）",
                "Command preview · o Original command (paged)",
                "Command preview | o Original command (paged)"
            ),
            Self::OutputFailed => {
                crate::ui::i18n::local!(language, "出力取得に失敗", "Output retrieval failed")
            }
            Self::RefreshFailed => crate::ui::text_width::glyphs::localized!(
                language,
                "更新失敗 · 前回の取得内容を表示",
                "更新失敗 | 前回の取得内容を表示",
                "Refresh failed · Showing the previous result",
                "Refresh failed | Showing the previous result"
            ),
            Self::Refreshing => crate::ui::text_width::glyphs::localized!(
                language,
                "再取得中 · 前回の取得内容を表示",
                "再取得中 | 前回の取得内容を表示",
                "Refreshing · Showing the previous result",
                "Refreshing | Showing the previous result"
            ),
            Self::OutputRetry => crate::ui::text_width::glyphs::localized!(
                language,
                "出力取得に失敗 · 閉じて開くと再試行 · F6 原文",
                "出力取得に失敗 | 閉じて開くと再試行 | F6 原文",
                "Output retrieval failed · Close and reopen to retry · F6 Original",
                "Output retrieval failed | Close and reopen to retry | F6 Original"
            ),
            Self::OutputLoading => crate::ui::text_width::glyphs::localized!(
                language,
                "出力を取得中 · F6 原文",
                "出力を取得中 | F6 原文",
                "Loading output · F6 Original",
                "Loading output | F6 Original"
            ),
            Self::OutputUnavailable => crate::ui::text_width::glyphs::localized!(
                language,
                "出力を取得できませんでした · F6 原文",
                "出力を取得できませんでした | F6 原文",
                "Could not retrieve output · F6 Original",
                "Could not retrieve output | F6 Original"
            ),
            Self::OutputEvicted => crate::ui::text_width::glyphs::localized!(
                language,
                "出力未保持 · 閉じて開くと再取得 · F6 原文",
                "出力未保持 | 閉じて開くと再取得 | F6 原文",
                "Output not retained · Close and reopen to reload · F6 Original",
                "Output not retained | Close and reopen to reload | F6 Original"
            ),
            Self::OutputOriginal => crate::ui::text_width::glyphs::localized!(
                language,
                "出力は原文で確認 · F6 詳細",
                "出力は原文で確認 | F6 詳細",
                "Review output in the original · F6 Details",
                "Review output in the original | F6 Details"
            ),
            Self::SourceOutsideWindow => crate::ui::text_width::glyphs::localized!(
                language,
                "元のtool行は表示範囲外です · F6で保存済み参照を確認",
                "元のtool行は表示範囲外です | F6で保存済み参照を確認",
                "Source tool row is outside this window · F6 Saved reference",
                "Source tool row is outside this window | F6 Saved reference"
            ),
            Self::ElapsedUnavailable => {
                crate::ui::i18n::local!(language, "経過未取得", "Elapsed time unavailable")
            }
            Self::Background => crate::ui::i18n::local!(language, "背景", "Background"),
            Self::HumanBackground => {
                crate::ui::i18n::local!(language, "背景（利用者）", "Background (user)")
            }
            Self::InputBackground => crate::ui::i18n::local!(
                language,
                "背景（追加入力）",
                "Background (additional input)"
            ),
            Self::OutputIncomplete => {
                crate::ui::i18n::local!(language, "出力不完全", "Output incomplete")
            }
            Self::AwaitingRecovery => {
                crate::ui::i18n::local!(language, "復旧待ち", "Awaiting recovery")
            }
            Self::Delivered => crate::ui::text_width::glyphs::localized!(
                language,
                "モデルに渡した · Enter 参照先",
                "モデルに渡した | Enter 参照先",
                "Delivered to model · Enter Referenced target",
                "Delivered to model | Enter Referenced target"
            ),
            Self::ResultUnknown => {
                crate::ui::i18n::local!(language, "結果未記録", "Result not recorded")
            }
            Self::IndexPaused => crate::ui::text_width::glyphs::localized!(
                language,
                "索引更新の停止記録です。現在の状態確認と再試行: /index → T。会話は続行できます。",
                "索引更新の停止記録です。現在の状態確認と再試行: /index > T。会話は続行できます。",
                "Index update pause recorded | /index → T to inspect and retry. Conversation can continue.",
                "Index update pause recorded | /index > T to inspect and retry. Conversation can continue."
            ),
            Self::ResultUncertain => crate::ui::text_width::glyphs::localized!(
                language,
                "外部での実行結果は未確認 · F6で保存済みの対象を確認",
                "外部での実行結果は未確認 | F6で保存済みの対象を確認",
                "External execution results are unconfirmed · F6 Saved targets",
                "External execution results are unconfirmed | F6 Saved targets"
            ),
            Self::NoReplay => crate::ui::i18n::local!(
                language,
                "自動での再実行はしません",
                "Commands will not be replayed automatically"
            ),
            Self::SavedTargets => crate::ui::i18n::local!(
                language,
                "保存した対象はF6 詳細・原文で確認",
                "Review saved targets in F6 Details / Original"
            ),
            Self::WaitInterrupted => crate::ui::i18n::local!(
                language,
                "前回の待機観測は中断済み",
                "Previous wait observation was interrupted"
            ),
            Self::SavedObservation => crate::ui::text_width::glyphs::localized!(
                language,
                "観測の記録を保持 · F6 原文",
                "観測の記録を保持 | F6 原文",
                "Observation saved · F6 Original",
                "Observation saved | F6 Original"
            ),
            Self::UnknownCount => crate::ui::i18n::local!(
                language,
                "件数は原文で確認",
                "Review the count in the original"
            ),
            Self::PreviousShutdown => {
                crate::ui::i18n::local!(language, "前回の終了時の記録", "Previous shutdown record")
            }
            Self::PreviousInterruption => {
                crate::ui::i18n::local!(language, "前回の中断記録", "Previous interruption record")
            }
            Self::PreviousRecord => {
                crate::ui::i18n::local!(language, "前回処理の保存記録", "Previous work record")
            }
            Self::ReceivedResponse => crate::ui::i18n::local!(
                language,
                "途中で切れた応答（保存済み・会話には含めない）",
                "Interrupted response (saved; not part of conversation) | /received list"
            ),
            Self::StreamDifference => crate::ui::i18n::local!(
                language,
                "ストリーム差分 | 最終応答は確定済み",
                "Stream difference | Final response is complete | /received list"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    IndexPause {
        at: Option<String>,
        reason: Option<String>,
    },
    CommandNotice {
        command: Option<String>,
        background: Option<bool>,
    },
    ThreadNotice {
        title: Option<String>,
    },
    Text(Text),
    UnknownTitle {
        kind: String,
    },
    Reclaimed {
        reason: String,
    },
    Summary {
        count: usize,
        replaced: bool,
    },
    /// One-row compaction marker (#230), rendered from saved facts.
    CompactionMarker(super::compaction_marker::MarkerFacts),
    SourceCount {
        count: usize,
    },
    Excerpt {
        rows: usize,
        omitted: usize,
    },
    SourceWindow {
        start: usize,
        end: usize,
        total: usize,
        live: bool,
    },
    BodyOmitted {
        count: usize,
    },
    Reasoning {
        count: usize,
    },
    Exit {
        code: i64,
    },
    Elapsed {
        ms: u64,
        decimal: bool,
    },
    EventTitle {
        kind: String,
    },
    EventSummary {
        title: Box<Message>,
        facts: Vec<Message>,
    },
    SectionTitle {
        section: String,
    },
    SectionCount {
        section: String,
        count: u64,
    },
    DetailCount {
        count: usize,
    },
    WaitCount {
        count: usize,
    },
    RetryError {
        refresh: bool,
        error: String,
    },
    Raw {
        text: String,
    },
    Hint {
        text: String,
    },
    SavedState {
        state: String,
    },
    EventIdentity {
        kind: String,
        identity: String,
        status: Option<String>,
        code: Option<i64>,
        elapsed_ms: Option<u64>,
    },
    Change {
        field: String,
        before: String,
        after: String,
    },
}
impl Message {
    pub fn render(&self, language: UiLanguage) -> String {
        let en = language == UiLanguage::En;
        match self {
            Self::IndexPause { at, reason } => {
                let at = at.as_deref().unwrap_or(if en {
                    "Time unavailable"
                } else {
                    "時刻不明"
                });
                let reason = reason.as_deref().unwrap_or(if en {
                    "Reason unavailable"
                } else {
                    "理由不明"
                });
                if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "/index → T · Pause recorded at {at} · {reason}",
                        "/index > T | Pause recorded at {at} | {reason}"
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "/index → T · 索引更新の停止記録 · {at} · {reason}",
                        "/index > T | 索引更新の停止記録 | {at} | {reason}"
                    )
                }
            }
            Self::CommandNotice {
                command,
                background,
            } => {
                let command = command
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| display_value(s, 120))
                    .unwrap_or_else(|| Text::NoticeCommandUnrecorded.render(language).into());
                match (en, background) {
                    (true, Some(true)) => crate::ui::text_width::glyphs::formatted!(
                        "{command} · Background requested",
                        "{command} | Background requested"
                    ),
                    (true, Some(false)) => crate::ui::text_width::glyphs::formatted!(
                        "{command} · Background not requested",
                        "{command} | Background not requested"
                    ),
                    (true, None) => crate::ui::text_width::glyphs::formatted!(
                        "{command} · Background request not recorded",
                        "{command} | Background request not recorded"
                    ),
                    (false, Some(true)) => crate::ui::text_width::glyphs::formatted!(
                        "{command} · 背景指定あり",
                        "{command} | 背景指定あり"
                    ),
                    (false, Some(false)) => crate::ui::text_width::glyphs::formatted!(
                        "{command} · 背景指定なし",
                        "{command} | 背景指定なし"
                    ),
                    (false, None) => crate::ui::text_width::glyphs::formatted!(
                        "{command} · 背景指定未記録",
                        "{command} | 背景指定未記録"
                    ),
                }
            }
            Self::ThreadNotice { title } => {
                let title = title
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| display_value(s, 80))
                    .unwrap_or_else(|| Text::NoticeUntitled.render(language).into());
                if en {
                    format!("Current name: {title}")
                } else {
                    format!("現在の名前: {title}")
                }
            }
            Self::Text(text) => text.render(language).into(),
            Self::UnknownTitle { kind } => {
                if en {
                    format!("Unsupported item: {kind}")
                } else {
                    format!("未対応の項目: {kind}")
                }
            }
            Self::Reclaimed { reason } => {
                if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "Original reclaimed · {reason}",
                        "Original reclaimed | {reason}"
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "原文回収済み · {reason}",
                        "原文回収済み | {reason}"
                    )
                }
            }
            Self::Summary { count, replaced } => match (en, replaced) {
                (true, true) if *count == 1 => crate::ui::text_width::glyphs::select(
                    "Summary · 1 replaced · 1 source · F6→O Original",
                    "Summary | 1 replaced | 1 source | F6>O Original",
                )
                .into(),
                (true, false) if *count == 1 => crate::ui::text_width::glyphs::select(
                    "Summary · 1 source · F6→O Original",
                    "Summary | 1 source | F6>O Original",
                )
                .into(),
                (true, true) => {
                    crate::ui::text_width::glyphs::formatted!(
                        "Summary · {count} replaced · {count} sources · F6→O Original",
                        "Summary | {count} replaced | {count} sources | F6>O Original"
                    )
                }
                (true, false) => crate::ui::text_width::glyphs::formatted!(
                    "Summary · {count} sources · F6→O Original",
                    "Summary | {count} sources | F6>O Original"
                ),
                (false, true) => crate::ui::text_width::glyphs::formatted!(
                    "要約 · 置換{count}件 · 出典{count}件 · F6→O 原文",
                    "要約 | 置換{count}件 | 出典{count}件 | F6>O 原文"
                ),
                (false, false) => crate::ui::text_width::glyphs::formatted!(
                    "要約 · 出典{count}件 · F6→O 原文",
                    "要約 | 出典{count}件 | F6>O 原文"
                ),
            },
            Self::CompactionMarker(facts) => facts.render(language),
            Self::SourceCount { count } => match (en, count) {
                (true, 1) => crate::ui::text_width::glyphs::select(
                    "  1 original · F6 Details",
                    "  1 original | F6 Details",
                )
                .into(),
                (true, _) => crate::ui::text_width::glyphs::formatted!(
                    "  {count} originals · F6 Details",
                    "  {count} originals | F6 Details"
                ),
                (false, _) => crate::ui::text_width::glyphs::formatted!(
                    "  原文{count}件 · F6 詳細",
                    "  原文{count}件 | F6 詳細"
                ),
            },
            Self::Excerpt { rows, omitted } => {
                if en && *rows == 1 {
                    crate::ui::text_width::glyphs::formatted!(
                        "… Output excerpt ({omitted} of 1 row omitted) · F6 Details",
                        ". Output excerpt ({omitted} of 1 row omitted) | F6 Details"
                    )
                } else if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "… Output excerpt ({omitted} of {rows} rows omitted) · F6 Details",
                        ". Output excerpt ({omitted} of {rows} rows omitted) | F6 Details"
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "… 出力抜粋（表示{rows}行中{omitted}行省略）· F6 詳細",
                        ". 出力抜粋（表示{rows}行中{omitted}行省略）| F6 詳細"
                    )
                }
            }
            Self::SourceWindow {
                start,
                end,
                total,
                live,
            } => match (en, live) {
                (true, true) if *total == 1 => {
                    crate::ui::text_width::glyphs::formatted!(
                        "Live display window {start}–{end}/1 character · F6 Details",
                        "Live display window {start}-{end}/1 character | F6 Details"
                    )
                }
                (true, false) if *total == 1 => {
                    crate::ui::text_width::glyphs::formatted!(
                        "Original window {start}–{end}/1 character · F6 Details",
                        "Original window {start}-{end}/1 character | F6 Details"
                    )
                }
                (true, true) => {
                    crate::ui::text_width::glyphs::formatted!(
                        "Live display window {start}–{end}/{total} characters · F6 Details",
                        "Live display window {start}-{end}/{total} characters | F6 Details"
                    )
                }
                (true, false) => {
                    crate::ui::text_width::glyphs::formatted!(
                        "Original window {start}–{end}/{total} characters · F6 Details",
                        "Original window {start}-{end}/{total} characters | F6 Details"
                    )
                }
                (false, true) => crate::ui::text_width::glyphs::formatted!(
                    "生成中の表示範囲 {start}–{end}/{total}文字 · F6 詳細",
                    "生成中の表示範囲 {start}-{end}/{total}文字 | F6 詳細"
                ),
                (false, false) => crate::ui::text_width::glyphs::formatted!(
                    "原文の取得範囲 {start}–{end}/{total}文字 · F6 詳細",
                    "原文の取得範囲 {start}-{end}/{total}文字 | F6 詳細"
                ),
            },
            Self::BodyOmitted { count } => {
                if en && *count == 1 {
                    crate::ui::text_width::glyphs::select(
                        "1 body/output character omitted · F6 Details",
                        "1 body/output character omitted | F6 Details",
                    )
                    .into()
                } else if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "{count} body/output characters omitted · F6 Details",
                        "{count} body/output characters omitted | F6 Details"
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "本文・出力{count}文字省略 · F6 詳細",
                        "本文・出力{count}文字省略 | F6 詳細"
                    )
                }
            }
            Self::Reasoning { count } => {
                if en && *count == 1 {
                    crate::ui::text_width::glyphs::select(
                        "▸ Reasoning 1 character · Collapsed /reasoning",
                        "▸ Reasoning 1 character | Collapsed /reasoning",
                    )
                    .into()
                } else if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "▸ Reasoning {count} characters · Collapsed /reasoning",
                        "▸ Reasoning {count} characters | Collapsed /reasoning"
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "▸ 推論 {count}文字 · 折りたたみ /reasoning",
                        "▸ 推論 {count}文字 | 折りたたみ /reasoning"
                    )
                }
            }
            Self::Exit { code } => {
                if en {
                    format!("Exit {code}")
                } else {
                    format!("終了 {code}")
                }
            }
            Self::Elapsed { ms, decimal } => {
                if !en {
                    super::elapsed_text(*ms, *decimal)
                } else if *decimal {
                    format!("{}.{:01}s", ms / 1000, (ms % 1000) / 100)
                } else if *ms >= 60_000 {
                    format!("{}m{}s", ms / 60_000, (ms / 1000) % 60)
                } else {
                    format!("{}s", ms / 1000)
                }
            }
            Self::EventTitle { kind } => event_title(kind, language).into(),
            Self::EventSummary { title, facts } => std::iter::once(title.render(language))
                .chain(facts.iter().map(|fact| fact.render(language)))
                .collect::<Vec<_>>()
                .join(crate::ui::text_width::glyphs::separator()),
            Self::SectionTitle { section } => {
                if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "Previous saved record · {}",
                        "Previous saved record | {}",
                        section_label(section, language)
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "前回の保存記録 · {}",
                        "前回の保存記録 | {}",
                        section_label(section, language)
                    )
                }
            }
            Self::SectionCount { section, count } => {
                if en {
                    format!("{}: {count}", section_label(section, language))
                } else {
                    format!("{} {count}件", section_label(section, language))
                }
            }
            Self::DetailCount { count } => {
                if en && *count == 1 {
                    crate::ui::text_width::glyphs::select(
                        "1 item in this original · F6 Target details",
                        "1 item in this original | F6 Target details",
                    )
                    .into()
                } else if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "{count} items in this original · F6 Target details",
                        "{count} items in this original | F6 Target details"
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "この原文に{count}件 · 個別の対象はF6 詳細",
                        "この原文に{count}件 | 個別の対象はF6 詳細"
                    )
                }
            }
            Self::WaitCount { count } => {
                if en && *count == 1 {
                    crate::ui::text_width::glyphs::select(
                        "1 wait condition · F6 Referenced targets for current state",
                        "1 wait condition | F6 Referenced targets for current state",
                    )
                    .into()
                } else if en {
                    crate::ui::text_width::glyphs::formatted!(
                        "{count} wait conditions · F6 Referenced targets for current state",
                        "{count} wait conditions | F6 Referenced targets for current state"
                    )
                } else {
                    crate::ui::text_width::glyphs::formatted!(
                        "待機条件{count}件 · 対象の現在状態はF6の参照先で確認",
                        "待機条件{count}件 | 対象の現在状態はF6の参照先で確認"
                    )
                }
            }
            Self::RetryError { refresh, error } => match (en, refresh) {
                (true, true) => crate::ui::text_width::glyphs::formatted!(
                    "Refresh failed · Showing the previous result: {error} · Close and reopen to retry",
                    "Refresh failed | Showing the previous result: {error} | Close and reopen to retry"
                ),
                (true, false) => {
                    crate::ui::text_width::glyphs::formatted!(
                        "Output retrieval failed: {error} · Close and reopen to retry",
                        "Output retrieval failed: {error} | Close and reopen to retry"
                    )
                }
                (false, true) => {
                    crate::ui::text_width::glyphs::formatted!(
                        "更新失敗 · 前回の取得内容を表示: {error} · 閉じて開くと再試行",
                        "更新失敗 | 前回の取得内容を表示: {error} | 閉じて開くと再試行"
                    )
                }
                (false, false) => crate::ui::text_width::glyphs::formatted!(
                    "出力取得に失敗: {error} · 閉じて開くと再試行",
                    "出力取得に失敗: {error} | 閉じて開くと再試行"
                ),
            },
            Self::Raw { text } => text.clone(),
            Self::Hint { text } => {
                if en {
                    format!("Action: {text}")
                } else {
                    format!("対処: {text}")
                }
            }
            Self::SavedState { state } => {
                if state == "recovery_wait" {
                    Text::AwaitingRecovery.render(language).into()
                } else {
                    tool_state(state, None, language)
                }
            }
            Self::EventIdentity {
                kind,
                identity,
                status,
                code,
                elapsed_ms,
            } => {
                let title = event_title(kind, language);
                let state = status.as_deref().unwrap_or("");
                let outcome = code
                    .map(|code| Message::Exit { code }.render(language))
                    .unwrap_or_else(|| state.into());
                let elapsed = elapsed_ms
                    .map(|ms| Message::Elapsed { ms, decimal: false }.render(language))
                    .unwrap_or_default();
                [title, identity, &outcome, &elapsed]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(crate::ui::text_width::glyphs::separator())
            }
            Self::Change {
                field,
                before,
                after,
            } => crate::ui::text_width::glyphs::formatted!(
                "{}: {before} → {after}",
                "{}: {before} > {after}",
                field_label(field, language)
            ),
        }
    }
    fn retained_bytes(&self) -> usize {
        match self {
            Self::IndexPause { at, reason } => {
                at.as_ref().map_or(0, String::capacity)
                    + reason.as_ref().map_or(0, String::capacity)
            }
            Self::CommandNotice { command, .. } => command.as_ref().map_or(0, String::capacity),
            Self::ThreadNotice { title } => title.as_ref().map_or(0, String::capacity),
            Self::Raw { text } | Self::Hint { text } => text.capacity(),
            Self::SavedState { state } => state.capacity(),
            Self::EventSummary { title, facts } => {
                std::mem::size_of::<Message>()
                    + title.retained_bytes()
                    + facts.capacity() * std::mem::size_of::<Message>()
                    + facts.iter().map(Message::retained_bytes).sum::<usize>()
            }
            Self::EventIdentity {
                kind,
                identity,
                status,
                ..
            } => {
                kind.capacity() + identity.capacity() + status.as_ref().map_or(0, String::capacity)
            }
            Self::UnknownTitle { kind } | Self::EventTitle { kind } => kind.capacity(),
            Self::Reclaimed { reason } => reason.capacity(),
            Self::SectionTitle { section } | Self::SectionCount { section, .. } => {
                section.capacity()
            }
            Self::RetryError { error, .. } => error.capacity(),
            Self::Change {
                field,
                before,
                after,
            } => field.capacity() + before.capacity() + after.capacity(),
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresentationMessages {
    pub title: Option<Message>,
    pub notice: Option<Message>,
    pub source_error: Option<Text>,
    pub bodies: Vec<(usize, Vec<Message>)>,
    pub limited: bool,
}
impl PresentationMessages {
    pub fn retained_bytes(&self) -> usize {
        self.title.as_ref().map_or(0, Message::retained_bytes)
            + self.notice.as_ref().map_or(0, Message::retained_bytes)
            + self.bodies.capacity() * std::mem::size_of::<(usize, Vec<Message>)>()
            + self
                .bodies
                .iter()
                .map(|(_, messages)| {
                    messages.capacity() * std::mem::size_of::<Message>()
                        + messages.iter().map(Message::retained_bytes).sum::<usize>()
                })
                .sum::<usize>()
    }
}

pub fn row_label(kind: RowKind, language: UiLanguage) -> &'static str {
    if language == UiLanguage::Ja {
        return kind.spec().1;
    }
    match kind {
        RowKind::Human => "User",
        RowKind::External => "External",
        RowKind::Parent => "Parent",
        RowKind::SelfModel => "Self",
        RowKind::OtherModel => "Model",
        RowKind::Revisit => "Revisit",
        RowKind::Assistant => "Answer",
        RowKind::ToolCall => "Call",
        RowKind::Running => "Running",
        RowKind::Unknown => "Observation unknown",
        RowKind::Success => "Exited",
        RowKind::Failure => "Failed",
        RowKind::Cancelled => "Cancelled / not started",
        RowKind::Notice => "Notice",
        RowKind::Summary => "Summary",
        RowKind::Spill => "Spill",
        RowKind::LiveWork => "Live work",
    }
}
pub fn event_title(kind: &str, language: UiLanguage) -> &str {
    if language == UiLanguage::Ja {
        return super::events::event_title(kind);
    }
    match kind {
        "reply_reminder" => "Reply reminder",
        "reply_outcome" => "Reply outcome",
        "index_state" => "Index state",
        "previous_instance_interrupted" => "Previous interruption record",
        "app_shutdown" => "Saved shutdown record",
        "turn_started" => "Turn started",
        "run_started" => "Run started",
        "thread_settings" | "connection_changed" | "model_changed" | "settings_changed" => {
            "Connection / settings changed"
        }
        "instructions_observed" | "instruction_changed" | "agents_changed" | "agents_reference" => {
            "Instruction files updated"
        }
        "recovery_resumed" => "Recovery resumed",
        "thread_unarchived" => "Unarchived",
        "context_changed" => "State change",
        "child_recovery_wait" => "Child awaiting recovery",
        "recovery_wait" => "Awaiting recovery",
        "child_turn_completed" => "Child answer",
        "child_run_ended" => "Child run ended",
        "shell_ended" => "Command ended",
        "child_report" => "Child progress report",
        _ => "Other state change",
    }
}
pub fn section_label(section: &str, language: UiLanguage) -> &str {
    match (language, section) {
        (UiLanguage::En, "runs") => "Runs",
        (UiLanguage::Ja, "runs") => "作業",
        (UiLanguage::En, "turns") => "Turns",
        (UiLanguage::Ja, "turns") => "応答",
        (UiLanguage::En, "shells") => "Shells",
        (UiLanguage::Ja, "shells") => "shell",
        (UiLanguage::En, "tool_calls") => "Tool calls",
        (UiLanguage::Ja, "tool_calls") => "ツール呼出し",
        (UiLanguage::En, "owned_children") => "Owned children",
        (UiLanguage::Ja, "owned_children") => "所有する子",
        (UiLanguage::En, "unreflected_messages") => "Unreflected messages",
        (UiLanguage::Ja, "unreflected_messages") => "未反映指示",
        (UiLanguage::En, "cancelled_waits") => "Cancelled waits",
        (UiLanguage::Ja, "cancelled_waits") => "解除済み待機",
        (UiLanguage::En, "pending_operations_with_unknown_targets") => {
            "Pending operations with unknown targets"
        }
        (UiLanguage::Ja, "pending_operations_with_unknown_targets") => "対象不明の未完了操作",
        _ => section,
    }
}

fn field_label(field: &str, language: UiLanguage) -> &str {
    match (language, field) {
        (UiLanguage::En, "connection" | "profile") => "Connection",
        (UiLanguage::Ja, "connection" | "profile") => "接続",
        (UiLanguage::En, "model") => "Model",
        (UiLanguage::Ja, "model") => "モデル",
        (UiLanguage::En, "cwd") => "Working directory",
        (UiLanguage::Ja, "cwd") => "作業場所",
        (UiLanguage::En, "effort") => "Reasoning effort",
        (UiLanguage::Ja, "effort") => "推論effort",
        (UiLanguage::En, "title") => "Title",
        (UiLanguage::Ja, "title") => "件名",
        (UiLanguage::En, "path") => "Instruction file",
        (UiLanguage::Ja, "path") => "指示ファイル",
        (UiLanguage::En, "present") => "Present",
        (UiLanguage::Ja, "present") => "存在",
        (UiLanguage::En, "active_state") => "Index state",
        (UiLanguage::Ja, "active_state") => "索引状態",
        (UiLanguage::En, "paused") => "Paused",
        (UiLanguage::Ja, "paused") => "一時停止",
        (UiLanguage::En, "pause_kind") => "Pause reason",
        (UiLanguage::Ja, "pause_kind") => "停止理由",
        (UiLanguage::En, "dimension") => "Dimensions",
        (UiLanguage::Ja, "dimension") => "次元数",
        _ => field,
    }
}

/// Called only while decoding a known application event, never on arbitrary text.
pub(super) fn event_messages(
    value: &serde_json::Value,
    saved_kind: &str,
    interrupted: bool,
    fields: &[super::PresentationField],
) -> (Message, Vec<Message>) {
    use serde_json::Value;
    let clipped = |text: &str| display_value(&super::sanitize_plain(text, 0).0, 240);
    let mut body = Vec::new();
    if interrupted {
        if saved_kind == "interrupted_wait" {
            body.push(
                value
                    .get("conditions")
                    .and_then(Value::as_array)
                    .map(|v| Message::WaitCount { count: v.len() })
                    .unwrap_or(Message::Text(Text::SavedObservation)),
            );
            return (Message::Text(Text::WaitInterrupted), body);
        }
        if let Some(section) = value.get("section").and_then(Value::as_str) {
            body.push(
                value
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|v| Message::DetailCount { count: v.len() })
                    .unwrap_or(Message::Text(Text::UnknownCount)),
            );
            return (
                Message::SectionTitle {
                    section: clipped(section),
                },
                body,
            );
        }
        let change = &value["change"];
        // Safety facts precede optional counts in constrained viewports.
        if change["result_uncertain"] == Value::Bool(true) {
            body.push(Message::Text(Text::ResultUncertain));
        }
        if change["automatic_replay"] == Value::Bool(false) {
            body.push(Message::Text(Text::NoReplay));
        }
        for section in [
            "runs",
            "turns",
            "shells",
            "tool_calls",
            "owned_children",
            "unreflected_messages",
            "cancelled_waits",
            "pending_operations_with_unknown_targets",
        ] {
            if let Some(count) = change[section]["count"].as_u64().filter(|c| *c > 0) {
                body.push(Message::SectionCount {
                    section: section.into(),
                    count,
                });
            }
        }
        if body.is_empty() {
            body.push(Message::Text(Text::SavedTargets));
        }
        let title = match value["kind"].as_str() {
            Some("app_shutdown") => Text::PreviousShutdown,
            Some("previous_instance_interrupted") => Text::PreviousInterruption,
            _ => Text::PreviousRecord,
        };
        return (Message::Text(title), body);
    }
    let event = if value["kind"] == "context_changed" {
        value.get("payload").unwrap_or(value)
    } else {
        value
    };
    let kind = event["kind"].as_str().unwrap_or("context_changed");
    let payload = event.get("payload").unwrap_or(event);
    let identity = [
        "execution_id",
        "child_thread_id",
        "thread_id",
        "run_id",
        "turn_id",
    ]
    .iter()
    .find_map(|key| payload.get(*key).and_then(Value::as_str))
    .map(clipped);
    let title = if kind == "index_state"
        && event.pointer("/change/after/paused") == Some(&Value::Bool(true))
    {
        let title = Message::IndexPause {
            at: event
                .pointer("/change/after/paused_at")
                .and_then(Value::as_str)
                .map(clipped),
            reason: event
                .pointer("/change/after/pause_kind")
                .and_then(Value::as_str)
                .map(clipped),
        };
        body.push(title.clone());
        title
    } else if let Some(identity) = identity {
        Message::EventIdentity {
            kind: clipped(kind),
            identity,
            status: payload
                .get("status")
                .or_else(|| payload.get("state"))
                .and_then(Value::as_str)
                .map(clipped),
            code: payload.get("exit_code").and_then(Value::as_i64),
            elapsed_ms: payload.get("elapsed_ms").and_then(Value::as_u64),
        }
    } else {
        Message::EventTitle {
            kind: clipped(kind),
        }
    };
    for field in fields.iter().take(super::MAX_FIELDS) {
        let key = field.key.as_str();
        let value = field.value.as_str();
        let message = if (key.ends_with("/state") || key.ends_with("/status"))
            && matches!(value, "recovery_wait" | "failed" | "stopped" | "cancelled")
        {
            Some(Message::SavedState {
                state: value.into(),
            })
        } else if key.ends_with("/exit_code") {
            value
                .parse::<i64>()
                .ok()
                .filter(|code| *code != 0)
                .map(|code| Message::Exit { code })
        } else if key.ends_with("/output_complete") && value == "false" {
            Some(Message::Text(Text::OutputIncomplete))
        } else if key.ends_with("/hint") {
            Some(Message::Hint {
                text: clipped(value),
            })
        } else if ((key.contains("/error/") && key.ends_with("/message"))
            || key.ends_with("/error"))
            && value != "null"
        {
            Some(Message::Raw {
                text: clipped(value),
            })
        } else {
            None
        };
        if let Some(message) = message
            && !body.contains(&message)
        {
            body.push(message);
        }
    }
    if kind == "index_state" && event.pointer("/change/after/paused") == Some(&Value::Bool(true)) {
        body.push(Message::Text(Text::IndexPaused));
    }
    let change = &event["change"];
    for key in [
        "connection",
        "model",
        "cwd",
        "effort",
        "title",
        "profile",
        "active_state",
        "paused",
        "pause_kind",
        "dimension",
        "path",
        "present",
    ] {
        if let Some(after) = change["after"].get(key) {
            let before = change["before"].get(key).unwrap_or(&Value::Null);
            if before != after {
                let show = |v: &Value| {
                    clipped(
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string())
                            .as_str(),
                    )
                };
                body.push(Message::Change {
                    field: key.into(),
                    before: show(before),
                    after: show(after),
                });
            }
        }
    }
    if kind == "reply_reminder" {
        if let Some(messages) = payload.get("messages").and_then(Value::as_array) {
            for message in messages.iter().take(128) {
                if let Some(id) = message["message_id"].as_str() {
                    body.push(Message::Raw {
                        text: format!(
                            "{} {}",
                            clipped(id),
                            clipped(&message["sender"].to_string())
                        ),
                    });
                }
            }
        }
    } else if kind == "reply_outcome" {
        for key in ["message_id", "state"] {
            if let Some(value) = payload[key].as_str() {
                body.push(Message::Raw {
                    text: format!("{key}: {}", clipped(value)),
                });
            }
        }
    }
    // Provider/model messages and saved instructions are verbatim data, not catalogue keys.
    for key in ["message", "detail", "instruction"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            body.push(Message::Raw {
                text: clipped(text),
            });
        }
    }
    if event
        .get("event_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
        && matches!(
            kind,
            "shell_ended" | "child_turn_completed" | "child_run_ended" | "child_report"
        )
    {
        body.push(Message::Text(Text::Delivered));
    }
    if body.is_empty() {
        body.push(title.clone());
    }
    let facts: Vec<_> = body
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::Change { .. }
                    | Message::SavedState { .. }
                    | Message::Exit { .. }
                    | Message::Text(Text::OutputIncomplete)
            )
        })
        .cloned()
        .collect();
    let title = if facts.is_empty() {
        title
    } else {
        Message::EventSummary {
            title: Box::new(title),
            facts,
        }
    };
    (title, body)
}

impl PresentationMessages {
    /// Admit complete sentences only; never trim a safety sentence or its arguments.
    pub(super) fn set_body(&mut self, index: usize, messages: Vec<Message>) -> bool {
        self.bodies.push((index, messages));
        if self.retained_bytes() <= 8 * 1024 {
            return true;
        }
        self.bodies.pop();
        self.bodies.shrink_to_fit();
        self.limited = true;
        false
    }
    pub(super) fn clear(&mut self) {
        *self = Self {
            limited: true,
            ..Self::default()
        };
    }
}

pub fn tool_heading(tool: &super::ToolPresentation, request: bool, language: UiLanguage) -> String {
    if language == UiLanguage::Ja {
        return if request {
            tool.call_heading_ja()
        } else {
            tool.heading_ja()
        };
    }
    let current = tool
        .current_state
        .as_deref()
        .filter(|s| *s != "observation_unavailable");
    let state = current.unwrap_or(&tool.status);
    let code = if current.is_some() {
        tool.current_exit_code
    } else {
        tool.exit_code
    };
    let kind = super::rows::tool_kind(state, tool.requires_attention());
    let identity = tool
        .ordinal
        .map(|n| format!("#{}", n + 1))
        .unwrap_or_else(|| format!("Ref {}", display_value(&tool.call_id, 16)));
    let command = if tool.command.is_empty() {
        "Original command in details".into()
    } else {
        display_value(
            &tool
                .command
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            120,
        )
    };
    let mark = if request {
        RowKind::ToolCall.mark()
    } else {
        kind.mark()
    };
    let mut facts = vec![format!("{mark} {identity} {command}")];
    if !request || current.is_some() {
        facts.push(tool_state(state, code, language));
    }
    if !request {
        if current.is_some() && (state != tool.status || code != tool.exit_code) {
            facts.push(format!(
                "At return: {}",
                tool_state(&tool.status, tool.exit_code, language)
            ));
        } else if tool.current_state.as_deref() == Some("observation_unavailable") {
            facts.push("Current state unconfirmed".into());
        }
        if let Some(ms) = tool.elapsed_ms {
            facts.push(Message::Elapsed { ms, decimal: true }.render(language));
        }
        if let Some(id) = &tool.execution_id {
            facts.push(format!("Execution {} | F6", display_value(id, 8)));
        }
    }
    facts.join(" | ")
}
pub fn tool_state(state: &str, code: Option<i64>, language: UiLanguage) -> String {
    if language == UiLanguage::Ja {
        return super::tool_state_text(state, code);
    }
    let label = match state {
        "requested" => "Requested",
        "pending" => "Pending",
        "starting" | "running_current" => "Running",
        "running" => "Background at return",
        "background_current" => "Running in background",
        "stopping" => "Stop requested",
        "completed" => "Result saved",
        "exited" if code.is_none() => "Exited (code unknown)",
        "exited" => "Exit",
        "failed" | "error" => "Failed",
        "cancelled" => "Cancelled / not executed",
        "not_started" => "Not started",
        "interrupted" => "Interrupted / result uncertain",
        "stopped" => "Stopped",
        _ => "State in details",
    };
    code.map(|code| format!("{label} {code}"))
        .unwrap_or_else(|| label.into())
}
pub fn tool_reason(tool: &super::ToolPresentation, language: UiLanguage) -> String {
    use super::ToolReasonKind as R;
    if language == UiLanguage::Ja {
        return tool.reason.clone();
    }
    if tool.reason == "エラー詳細はF6" {
        return "Error details: F6".into();
    }
    if let Some(prefix) = tool
        .reason
        .strip_suffix(" · 詳細はF6")
        .or_else(|| tool.reason.strip_suffix(" | 詳細はF6"))
    {
        return format!("{prefix} | F6 details");
    }
    match tool.reason_kind {
        R::RunStopped => "Cancelled because the run stopped",
        R::PriorCallInterrupted => {
            "Cancelled after an earlier call failed or foreground wait was interrupted"
        }
        R::PriorCallFailed => "Not executed because an earlier call failed",
        R::PriorOutputIncomplete => "Not executed because earlier output was incomplete",
        R::PriorWaitNewInput => "Not executed after additional input released the earlier wait",
        R::PriorWaitHumanBackground => "Not executed after the user backgrounded the earlier call",
        R::PriorWaitReleased => "Not executed due to the earlier wait outcome",
        R::HumanTurnInterrupted => "User interrupted the turn",
        R::PriorTurnInterrupted => "Not executed because the user interrupted the turn",
        R::OwnerShutdown => "Not executed because the application was shutting down",
        R::GroupValidation => "Not executed because tool group validation failed",
        R::AdditionalInput => "Backgrounded after additional input",
        R::HumanBackgrounded => "Backgrounded by the user",
        R::ForegroundTimeout => "Backgrounded at the foreground wait time limit",
        R::OutputLimit => "Output limit reached",
        R::Unknown => return tool.reason.clone(),
    }
    .into()
}

pub fn phase_label(phase: crate::activity::ActivityPhase, language: UiLanguage) -> &'static str {
    use crate::activity::ActivityPhase as P;
    if language == UiLanguage::Ja {
        return phase.label();
    }
    match phase {
        P::Idle => "Ready for input",
        P::Sending => "Accepting input",
        P::Reflecting => "Saved / loading",
        P::Preparing => "Preparing",
        P::AwaitingModel => "Waiting for model",
        P::Retrying => "Retrying after a temporary error",
        P::Receiving => "Receiving answer",
        P::Tools => "Running tools",
        P::Running => "Running",
        P::Waiting => "Waiting",
        P::Background => "Answered / background work",
        P::Continuing => "Continuing",
        P::Stopping => "Stopping",
        P::Recovery => "Awaiting recovery",
        P::Failed => "Failed",
        P::Stopped => "Stopped",
        P::Completed => "Completed",
    }
}

/// Display-only clipping is grapheme-aligned and uses the measured cell policy.
pub fn display_value(text: &str, width: usize) -> String {
    use crate::ui::text_width::UnicodeWidthStr;
    use unicode_segmentation::UnicodeSegmentation;
    if text.width() <= width {
        return text.into();
    }
    let mut used = 0;
    let mut result = String::new();
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if used + cells > width.saturating_sub(1) {
            break;
        }
        result.push_str(grapheme);
        used += cells;
    }
    if width > 0 {
        result.push_str(crate::ui::text_width::glyphs::ellipsis());
    }
    result
}

impl PresentationMessages {
    pub(super) fn set_title(&mut self, title: Message) -> bool {
        self.title = Some(title);
        if self.retained_bytes() <= 8 * 1024 {
            return true;
        }
        // Fall back to the complete canonical display title, not an earlier
        // generic translated label that would conceal its saved facts.
        self.title = None;
        self.limited = true;
        false
    }
}

impl PresentationMessages {
    pub fn set_notice(&mut self, notice: Message, source_error: Option<Text>) -> bool {
        self.notice = Some(notice);
        self.source_error = source_error;
        if self.retained_bytes() <= 8 * 1024 {
            return true;
        }
        self.notice = None;
        self.limited = true;
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_error_detail_hints_follow_language_and_keep_raw_prefixes() {
        let tool = crate::presentation::ToolPresentation {
            reason: "Provider code | 詳細はF6".into(),
            ..Default::default()
        };
        assert_eq!(
            super::tool_reason(&tool, crate::config::UiLanguage::En),
            "Provider code | F6 details"
        );
        assert_eq!(
            super::tool_reason(&tool, crate::config::UiLanguage::Ja),
            "Provider code | 詳細はF6"
        );
    }
    #[test]
    fn reply_events_show_each_message_and_sender() {
        let event = serde_json::json!({"kind":"reply_reminder","payload":{"messages":[{"message_id":"request-one","sender":{"kind":"human","label":"fixture"}},{"message_id":"request-two","sender":{"kind":"external_cli"}}]}});
        let (_, body) = super::event_messages(&event, "event", false, &[]);
        let rendered = format!("{body:?}");
        for text in ["request-one", "request-two", "fixture", "external_cli"] {
            assert!(rendered.contains(text), "{rendered}");
        }
    }

    use super::*;

    #[test]
    fn display_clipping_uses_measured_ellipsis_without_changing_source() {
        use crate::ui::text_width::{self, UnicodeWidthStr};
        let raw = "user | · → │ e\u{301} data";
        for policy in [Some(1), Some(2), None] {
            text_width::set_terminal_width(policy);
            assert_eq!(display_value(raw, 120), raw);
            for width in 0..16 {
                let shown = display_value(raw, width);
                assert!(shown.width() <= width, "{policy:?} {width}: {shown}");
                if width > 0 && raw.width() > width {
                    assert!(
                        shown.ends_with(if policy == Some(1) { "…" } else { "." }),
                        "{shown}"
                    );
                }
            }
            assert_eq!(raw, "user | · → │ e\u{301} data");
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn generated_glyphs_follow_policy_without_rewriting_user_values() {
        use crate::ui::text_width::{self, UnicodeWidthStr};
        let raw = "user | · > → │ content";
        for (observed, separator, arrow, ellipsis) in [
            (Some(1), " · ", " → ", "…"),
            (Some(2), " | ", " > ", "."),
            (None, " | ", " > ", "."),
        ] {
            text_width::set_terminal_width(observed);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let command = Message::CommandNotice {
                    command: Some(raw.into()),
                    background: Some(true),
                }
                .render(language);
                assert!(
                    command.contains(raw),
                    "{observed:?} {language:?}: {command}"
                );
                assert!(command.contains(separator), "{command}");
                let change = Message::Change {
                    field: "model".into(),
                    before: raw.into(),
                    after: raw.into(),
                }
                .render(language);
                assert_eq!(change.matches(raw).count(), 2, "{change}");
                assert!(change.contains(arrow), "{change}");
                assert_eq!(Message::Raw { text: raw.into() }.render(language), raw);
                assert!(Text::Collapsed.render(language).starts_with(ellipsis));
                assert!(Text::OutputRetry.render(language).contains(separator));
                let range = Message::SourceWindow {
                    start: 5,
                    end: 7,
                    total: 9,
                    live: false,
                }
                .render(language);
                let range_mark = if observed == Some(1) {
                    "5–7/9"
                } else {
                    "5-7/9"
                };
                assert!(
                    range.contains(range_mark),
                    "{observed:?} {language:?}: {range}"
                );
                assert!(command.width() <= 120);
            }
        }
        text_width::clear_terminal_width();
    }
    #[test]
    fn complete_count_sentences_cover_zero_one_and_many() {
        for (count, expected) in [(0, "0 originals"), (1, "1 original"), (2, "2 originals")] {
            let message = Message::SourceCount { count };
            assert!(message.render(UiLanguage::En).contains(expected));
            assert!(
                message
                    .render(UiLanguage::Ja)
                    .contains(&format!("原文{count}件"))
            );
        }
    }
    #[test]
    fn elapsed_render_preserves_existing_minute_and_hour_boundaries() {
        for (ms, decimal, japanese, english) in [
            (59_999, true, "59.9秒", "59.9s"),
            (60_000, true, "1分0秒", "60.0s"),
            (60_000, false, "1分0秒", "1m0s"),
            (3_600_000, false, "1時間0分", "60m0s"),
        ] {
            let message = Message::Elapsed { ms, decimal };
            assert_eq!(message.render(UiLanguage::Ja), japanese);
            assert_eq!(message.render(UiLanguage::En), english);
        }
    }

    #[test]
    fn dynamic_sentences_use_singular_only_for_one() {
        for count in [0, 1, 2] {
            let singular = count == 1;
            for (message, one, many) in [
                (
                    Message::Summary {
                        count,
                        replaced: false,
                    },
                    "1 source |",
                    "sources |",
                ),
                (Message::DetailCount { count }, "1 item in", "items in"),
                (
                    Message::WaitCount { count },
                    "1 wait condition |",
                    "wait conditions |",
                ),
                (
                    Message::BodyOmitted { count },
                    "1 body/output character omitted",
                    "characters omitted",
                ),
                (
                    Message::Reasoning { count },
                    "1 character |",
                    "characters |",
                ),
            ] {
                assert!(
                    message
                        .render(UiLanguage::En)
                        .contains(if singular { one } else { many })
                );
                assert!(message.render(UiLanguage::Ja).contains(&count.to_string()));
            }
        }
    }
    #[test]
    fn every_terminal_tool_state_and_reason_has_a_bilingual_presentation() {
        for state in [
            "requested",
            "pending",
            "starting",
            "running_current",
            "running",
            "background_current",
            "stopping",
            "completed",
            "exited",
            "failed",
            "error",
            "cancelled",
            "not_started",
            "interrupted",
            "stopped",
            "future-state",
        ] {
            for language in [UiLanguage::En, UiLanguage::Ja] {
                assert!(!tool_state(state, None, language).is_empty());
                assert!(!tool_state(state, Some(7), language).is_empty());
            }
        }
    }
}
