//! Closed notification facts; optional history attachment is qualified separately.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NotificationDescriptor {
    TurnStarted,
    RecoveryResumed,
    ThreadSettingsChanged {
        changed_fields: Vec<NotificationSetting>,
    },
    InstructionsObserved {
        observation: InstructionObservation,
    },
    ShellEnded {
        execution_id: String,
        state: NotificationTerminal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSetting {
    Cwd,
    Connection,
    Model,
    Effort,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstructionObservation {
    FirstSeen,
    Updated,
    Removed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationTerminal {
    Exited,
    Failed,
    Stopped,
}
