//! Bounded canonical copy preparation in existing read/presentation workers.
use super::*;
use crate::config::UiLanguage;
use crate::ui::{CopyBlock, CopyOffer, i18n::local};
const SOURCE_BYTES: usize = 1024 * 1024;
const SOURCE_PAGES: usize = 64;
const BLOCKS: usize = 128;

#[derive(Debug)]
enum CopyError {
    Limit,
    ReferenceChanged,
    Unavailable(String),
    Stalled,
    SourceChanged,
    MissingBlock,
}
impl CopyError {
    fn error(&self, language: UiLanguage) -> ApiError {
        let (code, message) = match self {
            Self::Limit => (ErrorCode::InputLimit, local!(language,
                "コピーの取得上限を超えました。oで原文を開き、nで続きを確認してください（history getでも取得できます）",
                "Copy retrieval limit exceeded. Press o to open the source and n to continue (also available through history get)").into()),
            Self::ReferenceChanged => (ErrorCode::TargetUnavailable, local!(language,
                "原文の参照先が変わりました", "The source reference has changed").into()),
            Self::Unavailable(reason) => (ErrorCode::OutputExpired, match language {
                UiLanguage::Ja => format!("原文を取得できません: {reason}"),
                UiLanguage::En => format!("Cannot retrieve the source: {reason}"),
            }),
            Self::Stalled => (ErrorCode::IoFailed, local!(language,
                "原文の続きが進みません", "Source pagination is not advancing").into()),
            Self::SourceChanged => (ErrorCode::TargetUnavailable, local!(language,
                "原文が更新されました。bでコードの塊を選び直してください",
                "The source has changed. Press b to select the code block again").into()),
            Self::MissingBlock => (ErrorCode::InvalidArgument, local!(language,
                "指定したコードの塊がありません", "The selected code block does not exist").into()),
        };
        ApiError::new(code, message)
    }
}
fn limit(language: UiLanguage) -> ApiError {
    CopyError::Limit.error(language)
}
async fn source(
    control: &Control,
    node_id: String,
    budget: usize,
    language: UiLanguage,
) -> Result<String, ApiError> {
    let history = crate::history::History::new(control.store.database.clone());
    let mut text = String::new();
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..SOURCE_PAGES {
        let body = history
            .get(HistoryGetArgs {
                final_answer_only: false,
                node_id: node_id.clone(),
                limit_bytes: MAX_BODY_BYTES,
                cursor,
                encoding: DEFAULT_ENCODING.into(),
            })
            .await?;
        if body.node_id != node_id {
            return Err(CopyError::ReferenceChanged.error(language));
        }
        if let Some(reason) = body.tombstone_reason {
            return Err(CopyError::Unavailable(reason).error(language));
        }
        if text.len().saturating_add(body.text.len()) > budget {
            return Err(limit(language));
        }
        text.push_str(&body.text);
        let Some(next) = body.next_cursor else {
            return Ok(text);
        };
        if !seen.insert(next.clone()) {
            return Err(CopyError::Stalled.error(language));
        }
        cursor = Some(next);
    }
    Err(limit(language))
}
fn summaries(text: &str, language: UiLanguage) -> Result<Vec<CopyBlock>, ApiError> {
    let mut blocks = Vec::new();
    for block in crate::presentation::markdown_code_blocks(text) {
        if blocks.len() == BLOCKS {
            return Err(limit(language));
        }
        blocks.push(CopyBlock {
            index: blocks.len(),
            chars: block.text.chars().count(),
            preview: crate::presentation::sanitize_plain(
                &block
                    .text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>(),
                0,
            )
            .0,
        });
    }
    Ok(blocks)
}
pub(super) fn source_hash(text: &str) -> String {
    crate::digest::sha256_hex(text.as_bytes())
}
pub(super) async fn choices(control: &Control, node_id: String) -> Result<CopyOffer, ApiError> {
    let language = control.store.config.ui.language;
    let text = source(control, node_id, SOURCE_BYTES, language).await?;
    presentation_work(move || {
        Ok(CopyOffer {
            source_hash: source_hash(&text),
            blocks: summaries(&text, language)?,
        })
    })
    .await?
}
pub(super) async fn text(
    control: &Control,
    node_id: String,
    block: Option<usize>,
    expected_hash: Option<String>,
) -> Result<String, ApiError> {
    let language = control.store.config.ui.language;
    if block.is_some_and(|index| index >= BLOCKS) {
        return Err(limit(language));
    }
    let text = source(
        control,
        node_id,
        if block.is_some() {
            SOURCE_BYTES
        } else {
            MAX_BODY_BYTES as usize
        },
        language,
    )
    .await?;
    presentation_work(move || {
        if expected_hash
            .as_ref()
            .is_some_and(|expected| expected != &source_hash(&text))
        {
            return Err(CopyError::SourceChanged.error(language));
        }
        let result = if let Some(index) = block {
            crate::presentation::markdown_code_blocks(&text)
                .nth(index)
                .map(|b| b.text)
                .ok_or_else(|| CopyError::MissingBlock.error(language))?
        } else {
            text
        };
        if result.len() > MAX_BODY_BYTES as usize {
            return Err(limit(language));
        }
        Ok(result)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_copy_failures_honor_both_languages_without_changing_source_reason() {
        let reason = "fixture reason: \u{4fdd}\u{5b58}\u{6e08}\u{307f} \u{25c9}";
        for (failure, code, japanese, english) in [
            (CopyError::Limit, ErrorCode::InputLimit,
             "コピーの取得上限を超えました。oで原文を開き、nで続きを確認してください（history getでも取得できます）".into(),
             "Copy retrieval limit exceeded. Press o to open the source and n to continue (also available through history get)".into()),
            (CopyError::ReferenceChanged, ErrorCode::TargetUnavailable,
             "原文の参照先が変わりました".into(), "The source reference has changed".into()),
            (CopyError::Unavailable(reason.into()), ErrorCode::OutputExpired,
             format!("原文を取得できません: {reason}"), format!("Cannot retrieve the source: {reason}")),
            (CopyError::Stalled, ErrorCode::IoFailed,
             "原文の続きが進みません".into(), "Source pagination is not advancing".into()),
            (CopyError::SourceChanged, ErrorCode::TargetUnavailable,
             "原文が更新されました。bでコードの塊を選び直してください".into(),
             "The source has changed. Press b to select the code block again".into()),
            (CopyError::MissingBlock, ErrorCode::InvalidArgument,
             "指定したコードの塊がありません".into(), "The selected code block does not exist".into()),
        ] {
            for (language, expected) in [(UiLanguage::Ja, japanese), (UiLanguage::En, english)] {
                let error = failure.error(language);
                assert_eq!(error.code, code);
                assert_eq!(error.message, expected);
                assert!(error.hint.is_none());
                assert!(error.details.is_none());
            }
        }
        assert!(limit(UiLanguage::default()).message.is_ascii());
        assert!(
            limit(UiLanguage::Ja)
                .message
                .contains("（history getでも取得できます）")
        );
    }

    #[tokio::test]
    async fn workers_use_default_en_and_explicit_ja_without_changing_copy_bytes() {
        for (settings, language) in [
            ("", UiLanguage::En),
            ("[ui]\nlanguage='ja'\n", UiLanguage::Ja),
        ] {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join("config.toml");
            std::fs::write(&config, format!("default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n{settings}")).unwrap();
            let owner = crate::app::DataOwner::start(
                Some(config),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            let original = "本文 e\u{301} 🦀（source）";
            control.store.database.call(move |db| {
                let tx = db.transaction()?;
                tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('copy','/','/AGENTS.md','fixture','fixture-model','now')", [])?;
                for (id, body) in [("plain", original.to_owned()), ("blocks", "```text\nsource\n```\n\n".repeat(BLOCKS + 1))] {
                    crate::history::append_node(&tx, "copy", id, None, "user", &serde_json::json!({"role":"user","content":body}), "2026-10-31T12:34:00Z")?;
                }
                tx.commit()?;
                Ok(())
            }).await.unwrap();
            assert_eq!(control.store.config.ui.language, language);
            let expected = limit(language);
            let rejected = text(&control, "plain".into(), Some(BLOCKS), None)
                .await
                .unwrap_err();
            assert_eq!(
                (rejected.code, rejected.message),
                (expected.code, expected.message.clone())
            );
            let rejected = choices(&control, "blocks".into()).await.unwrap_err();
            assert_eq!(
                (rejected.code, rejected.message),
                (expected.code, expected.message.clone())
            );
            let rejected = source(&control, "plain".into(), 1, language)
                .await
                .unwrap_err();
            assert_eq!(
                (rejected.code, rejected.message),
                (expected.code, expected.message)
            );
            let rejected = text(&control, "plain".into(), None, Some("stale".into()))
                .await
                .unwrap_err();
            assert_eq!(
                rejected.message,
                CopyError::SourceChanged.error(language).message
            );
            assert_eq!(
                text(&control, "plain".into(), None, None).await.unwrap(),
                original
            );
            drop(control);
            owner.shutdown().await.unwrap();
        }
    }

    #[test]
    fn summaries_count_unicode_and_retain_parser_order_without_rendered_labels() {
        let items = summaries(
            "text\n```rust\n界e\u{301}\n```\n\n    indented\n",
            UiLanguage::En,
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].chars, 4);
        assert_eq!(items[0].preview, "界e\u{301}");
        assert_eq!(items[1].index, 1);
        for language in [UiLanguage::En, UiLanguage::Ja] {
            assert_eq!(
                summaries(&"```\nx\n```\n\n".repeat(129), language)
                    .unwrap_err()
                    .message,
                limit(language).message
            );
        }
    }
}

#[cfg(test)]
mod image_guidance_tests {
    use super::*;
    #[test]
    fn image_failure_preserves_facts_and_offers_file_attachment_without_claiming_acquisition() {
        let mut error = ApiError::new(ErrorCode::InvalidState, "fixture image unavailable");
        error.hint = Some("original hint".into());
        error.details = Some(serde_json::json!({"fixture":true}));
        let result = super::super::image_acquisition_error(error.clone());
        assert_eq!(result.code, error.code);
        assert_eq!(result.details, error.details);
        assert_eq!(result.hint, error.hint);
        assert!(
            result.message.contains("fixture image unavailable")
                && result.message.contains("/attach")
        );
    }
}
