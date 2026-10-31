//! One display-only vocabulary for conversation row marks and boundaries.
use crate::ui::theme;
use ratatui::style::Color;

pub const NOTICE_SOURCE_OUTSIDE_WINDOW: &str =
    "元のtool行は表示範囲外です · F6で保存済み参照を確認";

pub const EXTERNAL_MARK_CANDIDATES: [&str; 3] = ["⤷", "⇥", "⤳"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Human,
    External,
    Parent,
    SelfModel,
    OtherModel,
    Revisit,
    Assistant,
    ToolCall,
    Running,
    Unknown,
    Success,
    Failure,
    Cancelled,
    Notice,
    Summary,
    Spill,
    LiveWork,
}
impl RowKind {
    pub const ALL: [Self; 17] = [
        Self::Human,
        Self::External,
        Self::Parent,
        Self::SelfModel,
        Self::OtherModel,
        Self::Revisit,
        Self::Assistant,
        Self::ToolCall,
        Self::Running,
        Self::Unknown,
        Self::Success,
        Self::Failure,
        Self::Cancelled,
        Self::Notice,
        Self::Summary,
        Self::Spill,
        Self::LiveWork,
    ];
    /// Symbols are Unicode East Asian Width=N (not ambiguous-width glyphs).
    pub fn spec(self) -> (&'static str, &'static str, Color) {
        match self {
            Self::Human => ("›", "利用者", theme::USER_MARKER),
            Self::External => (EXTERNAL_MARK_CANDIDATES[1], "外から", theme::USER_MARKER),
            Self::Parent => ("↳", "親", theme::USER_MARKER),
            Self::SelfModel => ("↺", "自分", theme::USER_MARKER),
            Self::OtherModel => ("↳", "モデル", theme::USER_MARKER),
            Self::Revisit => ("↻", "再訪", theme::USER_MARKER),
            Self::Assistant => ("▸", "回答", theme::ASSISTANT_MARKER),
            Self::ToolCall => ("▹", "呼出し", theme::TOOL),
            Self::Running => ("⏵", "実行中", theme::TOOL),
            Self::Unknown => ("◌", "観測不明", theme::MUTED),
            Self::Success => ("✓", "終了", theme::TOOL),
            Self::Failure => ("✗", "失敗", theme::ATTENTION),
            Self::Cancelled => ("⊘", "取消・未開始", theme::ATTENTION),
            Self::Notice => ("⁂", "通知", theme::TEXT),
            Self::Summary => ("⬦", "要約", theme::REPLACEMENT),
            Self::Spill => ("↪", "退避", theme::REPLACEMENT),
            Self::LiveWork => ("◉", "抱えている作業", theme::TOOL),
        }
    }
    pub(crate) fn metadata_color(self, palette: theme::Palette) -> Color {
        if palette.plain() {
            return theme::MUTED;
        }
        match self {
            Self::Failure | Self::Cancelled => self.spec().2,
            _ => palette.muted(),
        }
    }
    pub fn mark(self) -> &'static str {
        self.spec().0
    }
}

pub fn tool_kind(state: &str, failed: bool) -> RowKind {
    match state {
        "cancelled" | "not_started" | "interrupted" | "stopped" => RowKind::Cancelled,
        _ if failed => RowKind::Failure,
        "exited" | "completed" | "succeeded" => RowKind::Success,
        "starting" | "running" | "running_current" | "background_current" | "stopping" => {
            RowKind::Running
        }
        "requested" | "pending" => RowKind::ToolCall,
        _ => RowKind::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowGroup {
    Input,
    Body,
    Tool,
    Notice,
    Replacement,
}
/// One unselectable blank between unlike groups; consecutive tools stay dense.
pub fn blank_between(before: RowGroup, after: RowGroup) -> bool {
    before != after || matches!(before, RowGroup::Body | RowGroup::Input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;
    #[test]
    fn marks_are_one_column_even_with_cjk_width_rules() {
        for kind in RowKind::ALL {
            let (mark, label, _) = kind.spec();
            assert_eq!(mark.width(), 1);
            assert_eq!(mark.width_cjk(), 1);
            assert!(!label.is_empty());
        }
        for mark in EXTERNAL_MARK_CANDIDATES {
            assert_eq!(mark.width(), 1);
            assert_eq!(mark.width_cjk(), 1);
            assert_ne!(mark, RowKind::Human.mark());
        }
        assert_eq!(RowKind::External.mark(), "⇥");
        assert_eq!(RowKind::External.spec().1, "外から");
        assert_eq!(RowKind::External.mark().width_cjk(), 1);
        assert_ne!(RowKind::Running.mark(), RowKind::Unknown.mark());
        assert_ne!(RowKind::Notice.mark(), RowKind::Parent.mark());
    }
    #[test]
    fn metadata_is_muted_except_attention_and_unchanged_without_color() {
        use crate::config::PaletteMode;
        for mode in [PaletteMode::Dark, PaletteMode::Light] {
            let palette = theme::Palette::from_capabilities(mode, false, true);
            for kind in RowKind::ALL {
                let expected = if matches!(kind, RowKind::Failure | RowKind::Cancelled) {
                    theme::ATTENTION
                } else {
                    palette.muted()
                };
                assert_eq!(kind.metadata_color(palette), expected);
                let plain = theme::Palette::from_capabilities(mode, true, true);
                assert_eq!(kind.metadata_color(plain), theme::MUTED);
            }
        }
    }
    #[test]
    fn dense_tools_and_unlike_boundaries_share_one_rule() {
        assert!(!blank_between(RowGroup::Tool, RowGroup::Tool));
        assert!(blank_between(RowGroup::Body, RowGroup::Tool));
        assert!(blank_between(RowGroup::Tool, RowGroup::Body));
        assert!(blank_between(RowGroup::Notice, RowGroup::Tool));
    }
}
