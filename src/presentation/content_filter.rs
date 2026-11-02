//! Complete localized recovery messages; provider facts are never translated.
use crate::{config::UiLanguage, model::content_filter::ContentFilterFacts, ui::i18n};

fn unknown(language: UiLanguage) -> &'static str {
    i18n::local!(language, "不明", "Unknown")
}

fn fact_value(fact: &crate::model::content_filter::FilterFact, language: UiLanguage) -> String {
    if fact.recorded {
        fact.value
            .as_ref()
            .unwrap_or(&serde_json::Value::Null)
            .to_string()
    } else {
        unknown(language).into()
    }
}

pub fn summary(facts: &ContentFilterFacts, language: UiLanguage) -> String {
    let reason = if facts.reason_recorded {
        facts
            .reason
            .as_ref()
            .unwrap_or(&serde_json::Value::Null)
            .to_string()
    } else {
        unknown(language).into()
    };
    let verdicts = if facts.facts.is_empty() {
        unknown(language).to_owned()
    } else {
        let filtered: Vec<_> = facts
            .facts
            .iter()
            .filter(|fact| {
                fact.recorded
                    && fact.path.ends_with("/filtered")
                    && fact.value.as_ref().and_then(serde_json::Value::as_bool) == Some(true)
            })
            .collect();
        let selected: Vec<_> = if filtered.is_empty() {
            facts.facts.iter().take(2).collect()
        } else {
            filtered.into_iter().take(2).collect()
        };
        selected
            .into_iter()
            .map(|fact| {
                // A shortened path here is display-only; full paths remain in details.
                let path = fact.path.rsplitn(3, '/').take(2).collect::<Vec<_>>();
                let name = path.into_iter().rev().collect::<Vec<_>>().join("/");
                format!("{name}={}", fact_value(fact, language))
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let text = match language {
        UiLanguage::Ja => format!("incomplete_details.reason={reason} | 判定: {verdicts}"),
        UiLanguage::En => format!("incomplete_details.reason={reason} | Verdicts: {verdicts}"),
    };
    crate::presentation::sanitize_plain(&text, 0).0
}

pub fn compact_actions(language: UiLanguage) -> &'static str {
    i18n::local!(
        language,
        "/retry（判定は変わる可能性） | 言い換えて再開 | /detach | 既存ファイル: shell cp / curl | Ctrl+C 停止 | F8 詳細",
        "/retry (verdict may differ) | Rephrase | /detach | Existing files: shell cp / curl | Ctrl+C Stop | F8 Details"
    )
}

pub fn details(facts: &ContentFilterFacts, language: UiLanguage) -> Vec<String> {
    let mut lines = vec![summary(facts, language)];
    if facts.facts.is_empty() {
        lines.push(format!("content_filter_results: {}", unknown(language)));
    }
    lines.extend(
        facts
            .facts
            .iter()
            .map(|fact| format!("{}={}", fact.path, fact_value(fact, language))),
    );
    if facts.limited {
        lines.push(
            i18n::local!(
                language,
                "表示上限により判定の一部を省略しています。保存原文を確認してください。",
                "Some verdict facts are limited. Inspect the saved original response."
            )
            .into(),
        );
    }
    if facts.is_content_filter() {
        lines.push(i18n::local!(language,
            "providerのフィルターで応答が止まりました。/retryで再試行できます（判定は毎回同じとは限りません）。",
            "The provider filter stopped the response. Use /retry to try again (verdicts may differ).").into());
        lines.push(i18n::local!(language,
            "指示を言い換えて再開するか、関連するツール出力や添付を外してください（添付は/detach）。",
            "Rephrase instructions and resume, or remove relevant tool output or attachments (/detach for attachments).").into());
        lines.push(i18n::local!(language,
            "既存ファイルの複写が目的なら、モデルに本文を出力させず、shellのcpやcurlで複写する方法もあります。",
            "When copying existing files, shell cp or curl can copy them without having the model emit their body.").into());
    }
    lines
        .into_iter()
        .map(|line| crate::presentation::sanitize_plain(&line, 0).0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn both_languages_keep_provider_names_values_and_unknown_evidence() {
        let facts = ContentFilterFacts::from_response(&json!({
            "incomplete_details":{"reason":"content_filter"},
            "content_filter_results":{"protected_material_code":{"filtered":true,"detected":true}}
        }));
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let text = details(&facts, language).join("\n");
            assert!(text.contains("protected_material_code/filtered=true"));
            assert!(text.contains("protected_material_code/detected=true"));
            assert!(text.contains(&format!(
                "protected_material_code/severity={}",
                unknown(language)
            )));
            for command in ["/retry", "/detach", "shell", "cp", "curl"] {
                assert!(text.contains(command));
            }
            let missing = details(&ContentFilterFacts::default(), language).join("\n");
            assert!(missing.contains(unknown(language)));
            assert!(!missing.contains("provider filter stopped"));
        }
    }
}

/// The hint remains canonical JSON; this adds display-only, localized fields.
pub(crate) fn append_fields(
    fields: &mut Vec<super::PresentationField>,
    root: &serde_json::Value,
    language: UiLanguage,
) {
    let Some(reason) = root.get("recovery_hint") else {
        return;
    };
    let error = match reason.as_str() {
        Some(text) => serde_json::from_str::<crate::protocol::ApiError>(text),
        None => serde_json::from_value::<crate::protocol::ApiError>(reason.clone()),
    };
    let Ok(error) = error else {
        return;
    };
    let Some(facts) = error
        .details
        .as_ref()
        .and_then(|d| d.get("content_filter"))
        .and_then(|v| serde_json::from_value::<ContentFilterFacts>(v.clone()).ok())
    else {
        return;
    };
    fields.push(super::PresentationField {
        key: String::new(),
        label: i18n::local!(
            language,
            "providerの判定と次の手",
            "Provider verdicts and next steps"
        )
        .into(),
        value: details(&facts, language).join("\n"),
        ..Default::default()
    });
}

#[cfg(test)]
mod readback_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn display_projection_never_changes_raw_hint_copied_bytes_or_pointer() {
        let facts = ContentFilterFacts::from_response(&json!({
            "incomplete_details":{"reason":"content_filter"},
            "content_filter_results":{"protected_material_code":{"filtered":true}}
        }));
        let hint = serde_json::to_string(
            &crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::IoFailed,
                "unchanged provider diagnostic",
            )
            .details(json!({"content_filter":facts})),
        )
        .unwrap();
        let source = json!({"recovery_hint":hint});
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let mut fields = vec![super::super::PresentationField {
                key: "/recovery_hint".into(),
                value: hint.clone(),
                ..Default::default()
            }];
            append_fields(&mut fields, &source, language);
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].key, "/recovery_hint");
            assert_eq!(fields[0].value.as_bytes(), hint.as_bytes());
            assert!(
                fields[1].key.is_empty(),
                "generated advice is not a raw JSON pointer"
            );
            assert!(
                fields[1]
                    .displayed()
                    .contains("protected_material_code/filtered=true")
            );
            assert!(fields[1].displayed().contains(unknown(language)));
            for expected in ["/retry", "/detach", "cp", "curl"] {
                assert!(fields[1].displayed().contains(expected));
            }
        }
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn explicitly_filtered_category_precedes_unfiltered_opaque_verdicts() {
        let facts = ContentFilterFacts::from_response(
            &json!({"incomplete_details":{"reason":"content_filter"},"content_filter_results":{
                "aaa_unfiltered":{"filtered":false,"severity":"future-low"},
                "protected_material_code":{"filtered":true,"detected":true}
            }}),
        );
        for language in [UiLanguage::Ja, UiLanguage::En] {
            let text = summary(&facts, language);
            assert!(text.contains("protected_material_code/filtered=true"));
            assert!(!text.contains("aaa_unfiltered"));
            assert!(
                details(&facts, language)
                    .join("\n")
                    .contains("aaa_unfiltered/severity=\"future-low\"")
            );
        }
    }
}
