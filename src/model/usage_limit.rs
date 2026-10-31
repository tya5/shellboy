//! Bounded provider reset facts; never infer quotas from prose or usage totals.
use super::{ModelError, Value};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    UsageLimitReached,
    RateLimitRetryAfter,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLimit {
    pub kind: LimitKind,
    pub resets_at: i64,
    pub limit_window_minutes: Option<u64>,
    pub locally_blocked: bool,
}
impl UsageLimit {
    pub(crate) fn parse(value: &Value, now: DateTime<Utc>) -> Option<Self> {
        let error = value
            .get("error")
            .filter(|v| v.is_object())
            .unwrap_or(value);
        Self::object(error, now).or_else(|| {
            let text = error.get("message")?.as_str()?;
            Self::text(text, now)
        })
    }
    pub(crate) fn text(text: &str, now: DateTime<Utc>) -> Option<Self> {
        if text.len() > 65_536 {
            return None;
        }
        let text = text.trim();
        let json = if text.starts_with('{') {
            text
        } else {
            let rest = text.strip_prefix("litellm.RateLimitError")?;
            if !rest.starts_with(':') && !rest.starts_with(char::is_whitespace) {
                return None;
            }
            &rest[rest.find('{')?..]
        };
        let value: Value = serde_json::from_str(json).ok()?;
        Self::object(value.get("error").unwrap_or(&value), now)
    }
    fn object(error: &Value, now: DateTime<Utc>) -> Option<Self> {
        if ![error.get("type"), error.get("code")]
            .into_iter()
            .flatten()
            .any(|v| v.as_str() == Some("usage_limit_reached"))
        {
            return None;
        }
        let resets_at = if let Some(at) = error.get("resets_at") {
            at.as_i64()?
        } else {
            let seconds = error.get("resets_in_seconds")?.as_u64()?;
            if seconds == 0 {
                return None;
            }
            now.timestamp()
                .checked_add(i64::from(now.timestamp_subsec_nanos() > 0))?
                .checked_add(i64::try_from(seconds).ok()?)?
        };
        let at = DateTime::from_timestamp(resets_at, 0)?;
        if at <= now || at.year() > 9999 {
            return None;
        }
        Some(Self {
            kind: LimitKind::UsageLimitReached,
            resets_at,
            limit_window_minutes: error.get("limit_window_minutes").and_then(Value::as_u64),
            locally_blocked: false,
        })
    }
    pub(crate) fn retry_after_header(text: &str, now: DateTime<Utc>) -> Option<Self> {
        let text = text.trim();
        let at = if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
            let seconds = text.parse::<i64>().ok()?;
            if seconds <= 600 {
                return None;
            }
            DateTime::from_timestamp(
                now.timestamp()
                    .checked_add(seconds)?
                    .checked_add(i64::from(now.timestamp_subsec_nanos() > 0))?,
                0,
            )?
        } else {
            let at = DateTime::parse_from_rfc2822(text).ok()?.with_timezone(&Utc);
            if at.signed_duration_since(now) <= chrono::Duration::seconds(600) {
                return None;
            }
            at
        };
        if at.year() > 9999 {
            return None;
        }
        Some(Self {
            kind: LimitKind::RateLimitRetryAfter,
            resets_at: at.timestamp(),
            limit_window_minutes: None,
            locally_blocked: false,
        })
    }
    pub fn reset_time(&self) -> String {
        DateTime::from_timestamp(self.resets_at, 0)
            .map(|at| at.to_rfc3339())
            .unwrap_or_else(|| "unknown reset".into())
    }
    pub(crate) fn details(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("fixed facts serialize");
        value["resets_at_utc"] = self.reset_time().into();
        value
    }
    pub(crate) fn error(self) -> ModelError {
        let mut error = ModelError::new(
            "usage_limit",
            format!(
                "{}; resets at {}{}; wait until reset or select another connection/model with /model",
                match self.kind {
                    LimitKind::UsageLimitReached => "usage_limit_reached",
                    LimitKind::RateLimitRetryAfter => "rate_limit_retry_after",
                },
                self.reset_time(),
                self.limit_window_minutes
                    .map(|n| format!(" (window {n} minutes)"))
                    .unwrap_or_default()
            ),
        );
        error.usage_limit = Some(Box::new(self));
        error
    }
}
use chrono::Datelike;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn facts_require_exact_code_and_valid_future_integer_time() {
        let now = DateTime::from_timestamp(1_793_404_800, 0).unwrap();
        let at = now.timestamp() + 3600;
        for value in [
            json!({"error":{"type":"usage_limit_reached","resets_at":at,"limit_window_minutes":10080}}),
            json!({"error":{"code":"usage_limit_reached","resets_at":at}}),
            json!({"error":{"message":format!("litellm.RateLimitError: provider - {}", json!({"error":{"type":"usage_limit_reached","resets_at":at}}))}}),
        ] {
            let fact = UsageLimit::parse(&value, now).unwrap();
            assert_eq!(fact.resets_at, at);
            assert_eq!(fact.kind, LimitKind::UsageLimitReached);
            assert!(!fact.locally_blocked);
        }
        let relative = UsageLimit::parse(
            &json!({"type":"usage_limit_reached","resets_in_seconds":3600}),
            now,
        )
        .unwrap();
        assert_eq!(relative.resets_at, at);
        for value in [
            json!({"error":{"type":"rate_limit","resets_at":at}}),
            json!({"error":{"type":"usage_limit_reached"}}),
            json!({"error":{"message":"usage_limit_reached, resets tomorrow"}}),
            json!({"type":"usage_limit_reached","resets_at":"1793408400"}),
            json!({"type":"usage_limit_reached","resets_at":at as f64 + 0.5}),
            json!({"type":"usage_limit_reached","resets_at":0}),
            json!({"type":"usage_limit_reached","resets_at":i64::MAX}),
            json!({"type":"usage_limit_reached","resets_in_seconds":-1}),
            json!({"type":"usage_limit_reached","resets_in_seconds":0}),
        ] {
            assert!(UsageLimit::parse(&value, now).is_none(), "{value}");
        }
        assert!(UsageLimit::text("provider says usage_limit_reached", now).is_none());
        assert_eq!(
            UsageLimit::retry_after_header("601", now)
                .unwrap()
                .resets_at,
            now.timestamp() + 601
        );
        assert!(UsageLimit::retry_after_header("600", now).is_none());
        assert!(UsageLimit::retry_after_header("999999999999999999999", now).is_none());
        let fractional = now + chrono::Duration::milliseconds(500);
        assert!(
            UsageLimit::parse(
                &json!({"type":"usage_limit_reached","resets_in_seconds":0}),
                fractional
            )
            .is_none()
        );
        assert_eq!(
            UsageLimit::parse(
                &json!({"type":"usage_limit_reached","resets_in_seconds":1}),
                fractional
            )
            .unwrap()
            .resets_at,
            now.timestamp() + 2
        );
        assert_eq!(
            UsageLimit::retry_after_header("601", fractional)
                .unwrap()
                .resets_at,
            now.timestamp() + 602
        );
        assert!(UsageLimit::text("litellm.RateLimitErrorExample {}", now).is_none());
        let date = (now + chrono::Duration::seconds(3600)).to_rfc2822();
        assert_eq!(
            UsageLimit::retry_after_header(&date, now)
                .unwrap()
                .resets_at,
            at
        );
    }
}
