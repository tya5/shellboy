//! Completed UI diagnostics; never rewrite command payloads or raw errors.
use super::i18n;
use crate::config::UiLanguage;
use std::fmt::Display;

pub(super) fn model_settings(
    language: UiLanguage,
    connection: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    let missing = i18n::local!(language, "未設定", "not configured");
    let connection = connection.unwrap_or(missing);
    let model = model.unwrap_or(missing);
    let effort = effort
        .filter(|value| !value.is_empty())
        .unwrap_or(i18n::local!(language, "未指定", "not specified"));
    match language {
        UiLanguage::Ja => format!(
            "接続: {connection}\nモデル: {model}\n推論強度: {effort}\n\n/models  一覧からモデルを選ぶ\n/model 接続名 モデル名  名前を指定して変更\n/effort 値  推論強度を変更\n/effort none  推論強度の指定を解除"
        ),
        UiLanguage::En => format!(
            "Connection: {connection}\nModel: {model}\nReasoning effort: {effort}\n\n/models  Choose a model from the list\n/model <connection> <model>  Change by name\n/effort <value>  Change reasoning effort\n/effort none  Clear the reasoning effort override"
        ),
    }
}
pub(super) fn agents_path(language: UiLanguage, path: impl Display) -> String {
    match language {
        UiLanguage::Ja => format!("{path}\n\n変更するには /agents パス"),
        UiLanguage::En => format!("{path}\n\nTo change, use /agents <path>"),
    }
}
pub(super) fn cwd_path(language: UiLanguage, path: impl Display) -> String {
    match language {
        UiLanguage::Ja => format!("{path}\n\n変更するには /cwd パス"),
        UiLanguage::En => format!("{path}\n\nTo change, use /cwd <path>"),
    }
}
pub(super) fn agents_failure(language: UiLanguage, error: impl Display) -> String {
    match language {
        UiLanguage::Ja => format!("AGENTS参照を選択できません: {error}"),
        UiLanguage::En => format!("Cannot select AGENTS reference: {error}"),
    }
}
pub(super) fn refresh_failure(language: UiLanguage, message: &str) -> String {
    super::details_refresh_failure(language, message)
}

pub(super) fn failed_operation(
    language: UiLanguage,
    restore_revision: Option<u64>,
    error: &str,
) -> String {
    match (language, restore_revision) {
        (UiLanguage::Ja, None) => format!("操作は失敗しました | 入力を復元しました: {error}"),
        (UiLanguage::En, None) => format!("Operation failed | Input restored: {error}"),
        (UiLanguage::Ja, Some(revision)) => format!(
            "操作は失敗しました | 現在の入力を保持 | /restore {revision} で元の操作を復元: {error}"
        ),
        (UiLanguage::En, Some(revision)) => format!(
            "Operation failed | Current input kept | /restore {revision} restores the original operation: {error}"
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_arguments_are_not_translated_or_rewritten() {
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let raw = "原文 /id-42 C:\\data\"value\"";
            assert!(
                model_settings(language, Some(raw), Some(raw), Some(raw))
                    .matches(raw)
                    .count()
                    == 3
            );
            assert!(agents_path(language, raw).starts_with(raw));
            assert!(cwd_path(language, raw).starts_with(raw));
            assert!(agents_failure(language, raw).ends_with(raw));
            assert!(refresh_failure(language, raw).ends_with(raw));
            for revision in [None, Some(0), Some(1), Some(2), Some(u64::MAX)] {
                let rendered = failed_operation(language, revision, raw);
                assert!(rendered.ends_with(raw));
                if let Some(revision) = revision {
                    assert!(rendered.contains(&format!("/restore {revision}")));
                }
            }
        }
        assert!(model_settings(UiLanguage::En, None, None, Some("")).contains("not specified"));
        assert!(model_settings(UiLanguage::Ja, None, None, None).contains("未指定"));
    }
}
