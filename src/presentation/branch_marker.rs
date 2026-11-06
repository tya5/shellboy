//! One-row branch marker (#237): where this conversation's branches diverge.
//! Display only, from the saved facts of `thread branches`. The row names the
//! alternative being previewed (`2/3`); previewing never switches Threads.
use crate::config::UiLanguage;
use crate::history::branches::Marker;
use crate::ui::text_width::{UnicodeWidthStr, glyphs};

/// Characters of an alternative's last request shown in the row.
const EXCERPT_COLUMNS: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alt {
    pub thread_id: String,
    pub excerpt: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchMarker {
    pub alternatives: Vec<Alt>,
    /// The alternative the viewed Thread follows here.
    pub current: usize,
    /// The alternative whose excerpt the row shows; moved with the arrow keys.
    pub preview: usize,
}
impl BranchMarker {
    pub fn from_facts(marker: &Marker) -> Self {
        Self {
            alternatives: marker
                .alternatives
                .iter()
                .map(|a| Alt {
                    thread_id: a.thread_id.clone(),
                    excerpt: a.excerpt.clone(),
                })
                .collect(),
            current: marker.current,
            preview: marker.current,
        }
    }
    pub fn step(&mut self, delta: isize) -> bool {
        let count = self.alternatives.len();
        if count < 2 {
            return false;
        }
        self.preview = (self.preview as isize + delta).rem_euclid(count as isize) as usize;
        true
    }
    /// The Thread Enter opens, when it is not the one already followed.
    pub fn target(&self) -> Option<&str> {
        (self.preview != self.current)
            .then(|| {
                self.alternatives
                    .get(self.preview)
                    .map(|a| a.thread_id.as_str())
            })
            .flatten()
    }
    pub fn render(&self, language: UiLanguage) -> String {
        let en = language == UiLanguage::En;
        let which = match (self.preview == self.current, en) {
            (true, true) => "current",
            (true, false) => "今の枝",
            (false, true) => "branch",
            (false, false) => "別の枝",
        };
        let excerpt = match self
            .alternatives
            .get(self.preview)
            .and_then(|a| a.excerpt.as_deref())
        {
            Some(text) if !text.is_empty() => format!("\"{}\"", clip(text)),
            _ if en => "(no request)".to_owned(),
            _ => "(依頼なし)".to_owned(),
        };
        format!(
            "{} {}/{} {} {which} {excerpt}",
            glyphs::CHILD,
            self.preview + 1,
            self.alternatives.len(),
            glyphs::WAIT,
        )
    }
}
/// Display width bound, not a character count: wide characters count double.
fn clip(text: &str) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = ch.to_string().as_str().width();
        if used + width > EXCERPT_COLUMNS {
            out.push_str(glyphs::ellipsis());
            return out;
        }
        used += width;
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn marker() -> BranchMarker {
        BranchMarker {
            alternatives: vec![
                Alt {
                    thread_id: "a".into(),
                    excerpt: Some("fix the tests".into()),
                },
                Alt {
                    thread_id: "b".into(),
                    excerpt: Some("try with mocks".into()),
                },
                Alt {
                    thread_id: "c".into(),
                    excerpt: None,
                },
            ],
            current: 1,
            preview: 1,
        }
    }
    #[test]
    fn renders_position_excerpt_and_which_alternative_in_both_languages() {
        let mut m = marker();
        assert_eq!(
            m.render(UiLanguage::En),
            "↳ 2/3 ⋯ current \"try with mocks\""
        );
        assert_eq!(
            m.render(UiLanguage::Ja),
            "↳ 2/3 ⋯ 今の枝 \"try with mocks\""
        );
        m.step(-1);
        assert_eq!(m.render(UiLanguage::En), "↳ 1/3 ⋯ branch \"fix the tests\"");
        assert_eq!(m.target(), Some("a"));
        m.step(2);
        assert_eq!(m.render(UiLanguage::Ja), "↳ 3/3 ⋯ 別の枝 (依頼なし)");
        m.step(1);
        assert_eq!(m.preview, 0, "wraps around");
    }
    #[test]
    fn the_followed_alternative_has_no_target_and_single_choices_do_not_step() {
        let mut m = marker();
        assert_eq!(m.target(), None);
        m.alternatives.truncate(1);
        m.current = 0;
        m.preview = 0;
        assert!(!m.step(1));
    }
    #[test]
    fn long_and_wide_excerpts_are_clipped_by_display_width() {
        let long = "テスト".repeat(30);
        let text = clip(&long);
        assert!(text.width() <= EXCERPT_COLUMNS + 1, "{text}");
        assert!(text.ends_with(glyphs::ellipsis()));
        assert_eq!(clip("short"), "short");
    }
}
