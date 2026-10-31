//! Passive bounded labels and request witnesses. No reads, dispatch or clocks.
//! The G coordinator declares this module pub(crate); #109 does not edit ui.rs.
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::mem::size_of;

pub(crate) const MAX_TARGETS: usize = 200;
pub(crate) const MAX_IDENTITY_BYTES: usize = 512;
pub(crate) const MAX_METADATA_BYTES: usize = 256 * 1024;

/// Parse the existing G observed_at once; never substitute a draw-time clock.
pub(crate) fn captured_utc(observed_at: &str) -> Option<DateTime<Utc>> {
    if observed_at.len() > 64 {
        return None;
    }
    DateTime::parse_from_rfc3339(observed_at)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestWitness {
    pub owner: String,
    pub target: String,
    pub request: u64,
    pub generation: u64,
    /// Existing committed-change/read fence, not a new independent revision clock.
    pub revision: u64,
}
impl RequestWitness {
    pub fn metadata_bytes(&self) -> Option<usize> {
        [self.owner.as_str(), self.target.as_str()]
            .into_iter()
            .all(valid_identity)
            .then(|| size_of::<Self>() + self.owner.capacity() + self.target.capacity())
            .filter(|_| self.owner.capacity() <= MAX_IDENTITY_BYTES && self.target.capacity() <= MAX_IDENTITY_BYTES)
    }
}

fn valid_identity(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_IDENTITY_BYTES && !id.chars().any(char::is_control)
}

/// Caller supplies exact, namespaced saved identities, never rendered names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetWitnesses {
    Known(Vec<String>),
    Unavailable,
}
impl TargetWitnesses {
    pub fn metadata_bytes(&self) -> Option<usize> {
        match self {
            Self::Unavailable => Some(size_of::<Self>()),
            Self::Known(ids) => {
                if ids.len() > MAX_TARGETS || ids.capacity() > MAX_TARGETS {
                    return None;
                }
                let mut bytes = size_of::<Self>() + ids.capacity() * size_of::<String>();
                for id in ids {
                    if !valid_identity(id) || id.capacity() > MAX_IDENTITY_BYTES {
                        return None;
                    }
                    bytes = bytes.checked_add(id.capacity())?;
                }
                Some(bytes)
            }
        }
    }
    pub fn relevant(&self, committed: Option<&[String]>) -> bool {
        match (self, committed) {
            (Self::Known(ids), Some(changed)) if !ids.is_empty() && changed.len() <= MAX_TARGETS => {
                ids.iter().any(|id| changed.contains(id))
            }
            // Lost/unknown scope is explicit and conservative, never permission.
            _ => true,
        }
    }
    pub fn available(&self) -> bool {
        matches!(self, Self::Known(ids) if !ids.is_empty())
    }
}

/// Conservative retained heap charge; reject large/deep JSON before copying it.
/// The caller keeps the existing result Arc; these helpers never serialize bodies.
pub(crate) fn result_metadata_bytes(value: &Value, allowance: usize) -> Option<usize> {
    let mut bytes = 0usize;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        bytes = bytes.checked_add(size_of::<Value>())?;
        let children = match value {
            Value::String(text) => {
                bytes = bytes.checked_add(text.capacity())?;
                0
            }
            Value::Array(values) => {
                bytes = bytes.checked_add(values.capacity().checked_mul(size_of::<Value>())?)?;
                values.len()
            }
            Value::Object(values) => {
                for key in values.keys() {
                    bytes = bytes.checked_add(key.capacity() + size_of::<[usize; 16]>())?;
                    if bytes > allowance {
                        return None;
                    }
                }
                values.len()
            }
            _ => 0,
        };
        if bytes > allowance || pending.len().saturating_add(children) > 4096 {
            return None;
        }
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    Some(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationKind {
    Deletion,
    Cost,
    Index,
    Models,
    NativeMemory,
    StandaloneClient,
}
impl ObservationKind {
    pub fn confirmation(self) -> bool {
        matches!(self, Self::Deletion | Self::Cost | Self::Index)
    }
    fn label(self) -> &'static str {
        match self {
            Self::Deletion => "削除対象の確認",
            Self::Cost => "費用の確認",
            Self::Index => "索引対象の確認",
            Self::Models => "取得済みモデル一覧",
            Self::NativeMemory => "取得済みメモリ観測",
            Self::StandaloneClient => "完了した明示要求",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Freshness {
    Captured,
    Stale,
    FailedRefresh,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NextAction {
    RenewPreview,
    RefreshModels,
    RepeatInspection,
    None,
}
impl NextAction {
    fn label(self) -> &'static str {
        match self {
            Self::RenewPreview => "対象を明示的に再確認",
            Self::RefreshModels => "Ctrl+R 再取得",
            Self::RepeatInspection => "明示的に再照会",
            Self::None => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservationNotice {
    pub kind: ObservationKind,
    pub captured_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub freshness: Freshness,
    pub next_action: NextAction,
    /// UI enablement only. Existing transaction guards still decide execution.
    pub confirmation_enabled: bool,
}
impl ObservationNotice {
    pub fn text(&self) -> String {
        let captured = self.captured_at.map_or_else(
            || "取得時刻なし".into(),
            |time| format!("取得 UTC {}", time.to_rfc3339_opts(SecondsFormat::Millis, true)),
        );
        let state = match self.freshness {
            Freshness::Captured => "取得時点の事実",
            Freshness::Stale => "対象変更後・再確認が必要",
            Freshness::FailedRefresh => "再取得に失敗・以前の取得結果",
            Freshness::Unavailable => "確認範囲を取得できません",
        };
        let attempt = self.last_attempt_at.map_or_else(String::new, |time| {
            format!(" · 最終要求 UTC {}", time.to_rfc3339_opts(SecondsFormat::Millis, true))
        });
        format!("{} · {captured} · {state}{attempt} · {}", self.kind.label(), self.next_action.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_and_target_metadata_reject_truncation_and_excess_capacity() {
        let witness = RequestWitness { owner: "owner".into(), target: "thread:one".into(), request: 1, generation: 2, revision: 3 };
        assert!(witness.metadata_bytes().is_some());
        assert!(RequestWitness { target: "x".repeat(MAX_IDENTITY_BYTES + 1), ..witness }.metadata_bytes().is_none());
        assert!(TargetWitnesses::Known(vec!["id".into(); MAX_TARGETS + 1]).metadata_bytes().is_none());
        let mut large = String::with_capacity(1024);
        large.push('x');
        assert!(TargetWitnesses::Known(vec![large]).metadata_bytes().is_none());
        assert!(TargetWitnesses::Unavailable.relevant(Some(&[])));
        let empty = TargetWitnesses::Known(vec![]);
        assert!(!empty.available());
        assert!(empty.relevant(Some(&["thread:new".into()])));
    }

    #[test]
    fn existing_g_observed_at_is_parsed_once_without_inventing_missing_time() {
        let utc = captured_utc("2026-10-31T12:34:56+09:00").unwrap();
        assert_eq!(utc.to_rfc3339(), "2026-10-31T03:34:56+00:00");
        assert!(captured_utc("").is_none());
        assert!(captured_utc("2026-10-31T12:34:56").is_none());
        assert!(captured_utc(&"x".repeat(65)).is_none());
    }

    #[test]
    fn retained_result_and_notice_are_bounded_without_clock_or_io() {
        assert!(result_metadata_bytes(&json!({"cost": 42}), MAX_METADATA_BYTES).is_some());
        assert!(result_metadata_bytes(&json!("x".repeat(MAX_METADATA_BYTES)), MAX_METADATA_BYTES).is_none());
        let notice = ObservationNotice { kind: ObservationKind::Models,
            captured_at: Some(DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
            last_attempt_at: None, freshness: Freshness::Captured,
            next_action: NextAction::RefreshModels, confirmation_enabled: false };
        assert_eq!(notice.text(), notice.text());
        assert!(notice.text().contains("UTC"));
        assert!(notice.text().contains("Ctrl+R"));
        assert!(notice.text().len() < 512);
    }
}
