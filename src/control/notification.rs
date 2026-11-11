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
    pub(super) fn encoded(&self) -> String {
        serde_json::to_string(self).expect("closed notification fact")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_snapshots_publish_only_selected_fact_fields() {
        let settings = Fact::ThreadSettingsChanged {
            changed_fields: vec![Setting::Cwd, Setting::Model],
        }
        .encoded();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&settings).unwrap(),
            serde_json::json!({"kind":"thread_settings_changed","changed_fields":["cwd","model"]})
        );
        let observation = Fact::InstructionsObserved {
            observation: Observation::Removed,
        }
        .encoded();
        assert!(!observation.contains("path"));
        assert!(!observation.contains("hash"));
        assert_eq!(Fact::TurnStarted.encoded(), "{\"kind\":\"turn_started\"}");
        assert_eq!(
            Fact::RecoveryResumed.encoded(),
            "{\"kind\":\"recovery_resumed\"}"
        );
    }
}
