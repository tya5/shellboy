//! One-row compaction marker (#230): what a compaction did, from saved facts of
//! a `compact list` event. Display only; nothing is estimated, an absent or
//! unreported value is named as unknown.
use crate::config::UiLanguage;
use crate::ui::text_width::glyphs;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Adopted,
    /// Proactive attempt found no safe target (`no_target`/`no_safe_reduction`).
    Skipped,
    /// Failed or rejected, so nothing was adopted.
    Rejected,
    Unknown,
}
/// Summary request tokens: a known sum, and whether some requests reported none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Amount {
    pub known: Option<u64>,
    pub partial: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// No later model request has started yet.
    Pending,
    /// The request exists but the provider reported no input tokens.
    Unreported,
    Tokens(u64),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerFacts {
    pub automatic: bool,
    pub outcome: Outcome,
    pub has_summary: bool,
    pub fired_at: Option<u64>,
    pub threshold: Option<u64>,
    pub source_nodes: Option<u64>,
    pub parts: Option<u64>,
    pub summary_chars: Option<u64>,
    pub spilled_groups: Option<u64>,
    /// A spill was adopted but its summary was rejected.
    pub summary_rejected: bool,
    pub cost: Option<(Amount, Amount)>,
    pub after: After,
}
fn amount(value: &Value) -> Amount {
    Amount {
        known: value["known_sum"].as_u64(),
        partial: value["unknown_count"].as_u64().is_none_or(|n| n > 0),
    }
}
impl MarkerFacts {
    /// Facts of one `compact list` item; None for kinds that are not markers.
    pub fn from_event(event: &Value) -> Option<Self> {
        let kind = event["kind"].as_str()?;
        if !matches!(kind, "automatic_reduction" | "compact" | "summary") {
            return None;
        }
        let automatic = kind == "automatic_reduction";
        let metrics = &event["metrics"];
        let has_summary = metrics["summary_node_id"].is_string() || kind == "summary";
        let outcome = if has_summary {
            Outcome::Adopted
        } else {
            match event["state"].as_str() {
                Some("adopted") => Outcome::Adopted,
                Some("no_target" | "no_safe_reduction") => Outcome::Skipped,
                Some("rejected") => Outcome::Rejected,
                _ if metrics["no_target"] == true => Outcome::Skipped,
                _ if metrics["summary_rejected"] == true => Outcome::Rejected,
                _ => Outcome::Unknown,
            }
        };
        let usage = &metrics["summary_usage"];
        let after = match (
            event["post_compaction_attempt_id"].is_string(),
            event["post_compaction_input_tokens"].as_u64(),
        ) {
            (_, Some(tokens)) => After::Tokens(tokens),
            (true, None) => After::Unreported,
            (false, None) => After::Pending,
        };
        Some(Self {
            automatic,
            outcome,
            has_summary,
            fired_at: event["trigger"]["input_tokens"].as_u64(),
            threshold: event["trigger"]["auto_at_tokens"].as_u64(),
            source_nodes: metrics["source_nodes"].as_u64(),
            parts: metrics["summary_parts"].as_u64(),
            summary_chars: metrics["summary_characters"].as_u64(),
            spilled_groups: metrics["spilled_groups"].as_u64(),
            summary_rejected: !has_summary && metrics["summary_rejected"] == true,
            cost: usage.is_object().then(|| {
                (
                    amount(&usage["input_tokens"]),
                    amount(&usage["output_tokens"]),
                )
            }),
            after,
        })
    }
    pub fn render(&self, language: UiLanguage) -> String {
        match self.outcome {
            Outcome::Adopted => self.adopted(language),
            _ => self.not_adopted(language),
        }
    }
    fn adopted(&self, language: UiLanguage) -> String {
        let en = language == UiLanguage::En;
        let sep = glyphs::separator();
        let arrow = glyphs::select("→", ">");
        let k = crate::ui::chrome::compact_tokens;
        let label = match (self.automatic, self.has_summary, en) {
            (true, true, false) => "自動要約",
            (true, false, false) => "自動退避",
            (false, _, false) => "手動要約",
            (true, true, true) => "Auto summary",
            (true, false, true) => "Auto spill",
            (false, _, true) => "Manual summary",
        };
        let head = format!("{label}{}", if en { ": " } else { "：" });
        let mut parts: Vec<String> = Vec::new();
        if self.automatic {
            parts.push(match (self.fired_at, en) {
                (Some(n), false) => format!("{} で発動", k(n)),
                (None, false) => "発動時の入力量は不明".into(),
                (Some(n), true) => format!("triggered at {}", k(n)),
                (None, true) => "trigger input unknown".into(),
            });
        }
        if self.has_summary {
            let nodes = match (self.source_nodes, en) {
                (Some(n), false) => format!("{n} ノード"),
                (None, false) => "ノード数不明".into(),
                (Some(1), true) => "1 node".into(),
                (Some(n), true) => format!("{n} nodes"),
                (None, true) => "node count unknown".into(),
            };
            let made = if en {
                format!(
                    "{} ({})",
                    self.parts
                        .map_or("summary count unknown".into(), |p| if p == 1 {
                            "1 summary".into()
                        } else {
                            format!("{p} summaries")
                        }),
                    self.summary_chars
                        .map_or("size unknown".into(), |c| format!("{} chars", k(c)))
                )
            } else {
                format!(
                    "要約 {}（{}）",
                    self.parts.map_or("件数不明".into(), |p| format!("{p} 件")),
                    self.summary_chars
                        .map_or("字数不明".into(), |c| format!("{} 字", k(c)))
                )
            };
            parts.push(format!("{nodes} {arrow} {made}"));
        }
        match (self.spilled_groups, en) {
            (Some(n), false) if n > 0 => parts.push(format!("退避 {n} 件")),
            (Some(n), true) if n > 0 => parts.push(format!("{n} spilled")),
            _ => {}
        }
        if self.summary_rejected {
            parts.push(if en {
                "summary failed or rejected".into()
            } else {
                "要約は失敗または不採用".into()
            });
        }
        let show = |a: Amount| match (a.known, a.partial) {
            (Some(n), false) => k(n),
            (Some(n), true) => format!("{}+", k(n)),
            (None, _) => if en { "unknown" } else { "不明" }.into(),
        };
        parts.push(match (self.cost, en) {
            (Some((i, o)), false) => format!("費用 in {} / out {}", show(i), show(o)),
            (Some((i, o)), true) => format!("cost in {} / out {}", show(i), show(o)),
            (None, false) => "費用 不明".into(),
            (None, true) => "cost unknown".into(),
        });
        // A spill-only attempt has no summary to compare, so no later input.
        if self.has_summary {
            parts.push(match (self.after, en) {
                (After::Tokens(n), false) => format!("要約後の入力 {}", k(n)),
                (After::Unreported, false) => "要約後の入力 未報告または未完了".into(),
                (After::Pending, false) => "要約後の入力 次の要求待ち".into(),
                (After::Tokens(n), true) => format!("input after {}", k(n)),
                (After::Unreported, true) => "input after: not reported or not yet finished".into(),
                (After::Pending, true) => "input after: awaiting next request".into(),
            });
        }
        head + &parts.join(sep)
    }
    fn not_adopted(&self, language: UiLanguage) -> String {
        let en = language == UiLanguage::En;
        let sep = glyphs::separator();
        let k = crate::ui::chrome::compact_tokens;
        let (head, reason) = match (self.outcome, en) {
            (Outcome::Skipped, false) => ("自動要約を見送り", "要約できる対象なし"),
            (Outcome::Rejected, false) => ("自動要約を採用せず", "失敗または不採用"),
            (_, false) => ("自動要約の結果", "不明"),
            (Outcome::Skipped, true) => ("Auto summary skipped", "no eligible content"),
            (Outcome::Rejected, true) => ("Auto summary not adopted", "failed or rejected"),
            (_, true) => ("Auto summary outcome", "unknown"),
        };
        let input = match (self.fired_at, self.threshold, en) {
            (Some(n), Some(t), false) => format!("入力 {}（閾値 {}）", k(n), k(t)),
            (Some(n), None, false) => format!("入力 {}（閾値は不明）", k(n)),
            (None, _, false) => "入力 不明".into(),
            (Some(n), Some(t), true) => format!("input {} (threshold {})", k(n), k(t)),
            (Some(n), None, true) => format!("input {} (threshold unknown)", k(n)),
            (None, _, true) => "input unknown".into(),
        };
        // The retry gate is the saved eligible-source witness of the Turn; no
        // growth percentage exists in the product, so none is shown.
        let next = if en {
            "next: when new eligible content appears"
        } else {
            "次の発動：新しい要約対象が増えたとき"
        };
        let colon = if en { ": " } else { "：" };
        format!("{head}{colon}{reason}{sep}{input}{sep}{next}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text_width::{clear_terminal_width, set_terminal_width};
    use serde_json::json;
    fn adopted(kind: &str) -> Value {
        json!({"kind":kind,"state":"adopted","trigger":{"input_tokens":205000,"auto_at_tokens":200000},
            "metrics":{"summary_node_id":"s","source_nodes":412,"summary_parts":1,"summary_characters":31000,"spilled_groups":8,
            "summary_usage":{"input_tokens":{"known_sum":30000,"unknown_count":0,"overflow":false},
              "output_tokens":{"known_sum":2000,"unknown_count":0,"overflow":false}}},
            "post_compaction_attempt_id":"a","post_compaction_input_tokens":32000})
    }
    fn render(event: &Value, language: UiLanguage, unicode: bool) -> String {
        set_terminal_width(unicode.then_some(1));
        let text = MarkerFacts::from_event(event).unwrap().render(language);
        clear_terminal_width();
        text
    }
    #[test]
    fn automatic_adopted_marker_has_both_languages_and_both_glyph_sets() {
        let event = adopted("automatic_reduction");
        assert_eq!(
            render(&event, UiLanguage::Ja, true),
            "自動要約：205K で発動 · 412 ノード → 要約 1 件（31K 字） · 退避 8 件 · 費用 in 30K / out 2K · 要約後の入力 32K"
        );
        assert_eq!(
            render(&event, UiLanguage::En, true),
            "Auto summary: triggered at 205K · 412 nodes → 1 summary (31K chars) · 8 spilled · cost in 30K / out 2K · input after 32K"
        );
        assert_eq!(
            render(&event, UiLanguage::En, false),
            "Auto summary: triggered at 205K | 412 nodes > 1 summary (31K chars) | 8 spilled | cost in 30K / out 2K | input after 32K"
        );
    }
    #[test]
    fn manual_marker_has_no_trigger_and_unknowns_are_named() {
        let mut event = adopted("compact");
        event["metrics"] = json!({"summary_node_id":"s"});
        event["post_compaction_input_tokens"] = Value::Null;
        assert_eq!(
            render(&event, UiLanguage::Ja, true),
            "手動要約：ノード数不明 → 要約 件数不明（字数不明） · 費用 不明 · 要約後の入力 未報告または未完了"
        );
        event["post_compaction_attempt_id"] = Value::Null;
        event["metrics"]["summary_usage"] = json!({"input_tokens":{"known_sum":30000,"unknown_count":1,"overflow":false},
            "output_tokens":{"known_sum":null,"unknown_count":1,"overflow":false}});
        assert_eq!(
            render(&event, UiLanguage::En, true),
            "Manual summary: node count unknown → summary count unknown (size unknown) · cost in 30K+ / out unknown · input after: awaiting next request"
        );
    }
    #[test]
    fn skipped_and_failed_attempts_give_reason_and_next_condition() {
        let mut event = json!({"kind":"automatic_reduction","state":"no_target",
            "trigger":{"input_tokens":205000,"auto_at_tokens":200000},"metrics":{}});
        assert_eq!(
            render(&event, UiLanguage::Ja, true),
            "自動要約を見送り：要約できる対象なし · 入力 205K（閾値 200K） · 次の発動：新しい要約対象が増えたとき"
        );
        event["state"] = json!("rejected");
        event["trigger"] = json!({});
        assert_eq!(
            render(&event, UiLanguage::En, true),
            "Auto summary not adopted: failed or rejected · input unknown · next: when new eligible content appears"
        );
        event["state"] = json!("unknown");
        assert!(
            render(&event, UiLanguage::En, false)
                .starts_with("Auto summary outcome: unknown | input unknown | next")
        );
    }
    #[test]
    fn spill_only_attempt_with_rejected_summary_shows_the_rejection_and_no_later_input() {
        let event = json!({"kind":"automatic_reduction","state":"adopted",
            "trigger":{"input_tokens":205000,"auto_at_tokens":200000},
            "metrics":{"summary_rejected":true,"spilled_groups":3}});
        let text = render(&event, UiLanguage::En, true);
        assert_eq!(
            text,
            "Auto spill: triggered at 205K · 3 spilled · summary failed or rejected · cost unknown"
        );
        assert!(render(&event, UiLanguage::Ja, true).contains("要約は失敗または不採用"));
        assert!(!text.contains("input after"));
    }
    #[test]
    fn non_marker_kinds_are_ignored() {
        assert!(MarkerFacts::from_event(&json!({"kind":"restore"})).is_none());
        assert!(MarkerFacts::from_event(&json!({"kind":"spill","metrics":{}})).is_none());
    }
}
