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
