//! Closed producer snapshots; never reconstruct facts from private event JSON.
use serde::Serialize;
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Setting {
    Cwd,
    Connection,
    Model,
    Effort,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Observation {
    FirstSeen,
    Updated,
    Removed,
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Fact {
    TurnStarted,
    RecoveryResumed,
    ThreadSettingsChanged {
        changed_fields: Vec<Setting>,
    },
    InstructionsObserved {
        observation: Observation,
    },
    ShellEnded {
        execution_id: String,
        state: Terminal,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Terminal {
    Exited,
    Failed,
    Stopped,
}
impl Fact {
    pub(super) fn encoded(&self) -> Option<String> {
        match self {
            Self::ShellEnded { execution_id, .. }
                if execution_id.trim().is_empty() || execution_id.len() > 256 =>
            {
                return None;
            }
            Self::ThreadSettingsChanged { changed_fields }
                if changed_fields.is_empty() || changed_fields.len() > 4 =>
            {
                return None;
            }
            Self::ThreadSettingsChanged { changed_fields } => {
                let mut seen = 0u8;
                for field in changed_fields {
                    let bit = match field {
                        Setting::Cwd => 1,
                        Setting::Connection => 2,
                        Setting::Model => 4,
                        Setting::Effort => 8,
                    };
                    if seen & bit != 0 {
                        return None;
                    }
                    seen |= bit;
                }
            }
            _ => {}
        }
        let encoded = serde_json::to_string(self).expect("closed notification fact");
        (encoded.len() <= 2048).then_some(encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_bounds_count_utf8_bytes_and_reject_duplicate_fields() {
        let fact = |id| Fact::ShellEnded {
            execution_id: id,
            state: Terminal::Exited,
            exit_code: Some(0),
        };
        assert!(fact("x".repeat(256)).encoded().is_some());
        assert!(fact("x".repeat(257)).encoded().is_none());
        assert!(fact("\u{e9}".repeat(128)).encoded().is_some());
        assert!(fact("\u{e9}".repeat(129)).encoded().is_none());
        assert!(fact(" ".into()).encoded().is_none());
        assert!(
            Fact::ThreadSettingsChanged {
                changed_fields: vec![]
            }
            .encoded()
            .is_none()
        );
        assert!(
            Fact::ThreadSettingsChanged {
                changed_fields: vec![Setting::Cwd, Setting::Cwd]
            }
            .encoded()
            .is_none()
        );
    }
    #[test]
    fn closed_snapshots_publish_only_selected_fact_fields() {
        let settings = Fact::ThreadSettingsChanged {
            changed_fields: vec![Setting::Cwd, Setting::Model],
        }
        .encoded()
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&settings).unwrap(),
            serde_json::json!({"kind":"thread_settings_changed","changed_fields":["cwd","model"]})
        );
        let observation = Fact::InstructionsObserved {
            observation: Observation::Removed,
        }
        .encoded()
        .unwrap();
        assert!(!observation.contains("path"));
        assert!(!observation.contains("hash"));
        assert_eq!(
            Fact::TurnStarted.encoded().unwrap(),
            "{\"kind\":\"turn_started\"}"
        );
        assert_eq!(
            Fact::RecoveryResumed.encoded().unwrap(),
            "{\"kind\":\"recovery_resumed\"}"
        );
    }
}
