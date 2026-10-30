//! Credential-free sender facts. Labels never participate in authorization.
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderKind {
    Model,
    ExternalCli,
    Human,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSender {
    #[serde(default)]
    pub kind: SenderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

pub fn validate_sender_label(label: Option<&str>) -> Result<(), ApiError> {
    if label.is_some_and(|text| {
        text.chars().count() > 64
            || text
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    }) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "sender_label must be at most 64 Unicode characters without controls or line separators",
        ));
    }
    Ok(())
}

impl MessageSender {
    pub(crate) fn recorded(origin: Option<&Origin>, label: Option<String>, human: bool) -> Self {
        if let Some(origin) = origin {
            Self {
                kind: SenderKind::Model,
                instance_id: Some(origin.instance_id.clone()),
                thread_id: Some(origin.thread_id.clone()),
                run_id: Some(origin.run_id.clone()),
                turn_id: Some(origin.turn_id.clone()),
                execution_id: Some(origin.execution_id.clone()),
                label,
            }
        } else {
            Self {
                kind: if human {
                    SenderKind::Human
                } else {
                    SenderKind::ExternalCli
                },
                label,
                ..Self::default()
            }
        }
    }

    /// Inspect only saved facts, including pre-kind OriginIdentity records.
    /// Never deserialize or echo a credential, or infer human from null.
    pub(crate) fn from_saved(raw: Option<&str>) -> Self {
        let Some(value) = raw.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        else {
            return Self::default();
        };
        let Ok(mut sender) = serde_json::from_value::<Self>(value.clone()) else {
            return Self::default();
        };
        let complete = [
            &sender.instance_id,
            &sender.thread_id,
            &sender.run_id,
            &sender.turn_id,
            &sender.execution_id,
        ]
        .iter()
        .all(|id| id.as_ref().is_some_and(|id| !id.is_empty()));
        if value.get("kind").is_none() && complete {
            sender.kind = SenderKind::Model;
        }
        if sender.kind == SenderKind::Model && !complete {
            return Self::default();
        }
        if sender.kind != SenderKind::Model {
            sender.instance_id = None;
            sender.thread_id = None;
            sender.run_id = None;
            sender.turn_id = None;
            sender.execution_id = None;
        }
        if validate_sender_label(sender.label.as_deref()).is_err() {
            sender.label = None;
        }
        sender
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn sender_labels_count_unicode_scalars_and_reject_controls_without_echoing() {
        for label in ["", "名前", &"界".repeat(64)] {
            validate_sender_label(Some(label)).unwrap();
        }
        for label in [
            "界".repeat(65),
            "a\nb".into(),
            "a\rb".into(),
            "a\tb".into(),
            "\0".into(),
            "\x1b".into(),
            "\u{85}".into(),
            "\u{2028}".into(),
            "\u{2029}".into(),
        ] {
            let error = validate_sender_label(Some(&label)).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument);
            assert!(!error.message.contains(&label));
        }
    }
    #[test]
    fn sender_projection_never_invents_legacy_identity_or_leaks_unknown_fields() {
        for raw in [
            None,
            Some("null"),
            Some("{}"),
            Some(r#"{"thread_id":"only"}"#),
            Some(r#"{"kind":"model","thread_id":"partial"}"#),
        ] {
            assert_eq!(MessageSender::from_saved(raw), MessageSender::default());
        }
        let legacy = json!({"instance_id":"i","thread_id":"t","run_id":"r","turn_id":"u","execution_id":"e","token":"DO_NOT_LEAK"});
        let sender = MessageSender::from_saved(Some(&legacy.to_string()));
        assert_eq!(sender.kind, SenderKind::Model);
        assert_eq!(sender.thread_id.as_deref(), Some("t"));
        assert!(
            !serde_json::to_string(&sender)
                .unwrap()
                .contains("DO_NOT_LEAK")
        );
        let raw = json!({"kind":"external_cli","label":"model leader","thread_id":"forged","token":"DO_NOT_LEAK"});
        let sender = MessageSender::from_saved(Some(&raw.to_string()));
        assert_eq!(sender.kind, SenderKind::ExternalCli);
        assert_eq!(sender.label.as_deref(), Some("model leader"));
        assert!(sender.thread_id.is_none());
    }
}
