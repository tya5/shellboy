//! Producer-owned closed snapshots. No generic event payload decoding.
use serde::Serialize;

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Fact {
    TurnStarted,
    RecoveryResumed,
}
impl Fact {
    pub(super) fn encoded(&self) -> String {
        serde_json::to_string(self).expect("closed notification fact")
    }
}
