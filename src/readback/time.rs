use super::*;
use chrono::{DateTime, Datelike, Duration, Utc};
#[derive(Clone, Default)]
pub(crate) struct Window {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}
impl Window {
    pub fn new(since: Option<&str>, until: Option<&str>) -> Result<Self, ApiError> {
        fn parse(s: &str) -> Result<DateTime<Utc>, ApiError> {
            if s.len() > 128 || s.as_bytes().get(4) != Some(&b'-') {
                return Err(invalid("TIME must be a four-digit-year RFC3339 timestamp"));
            }
            DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|_| invalid("TIME must be RFC3339 with an explicit offset"))
        }
        let w = Self {
            since: since.map(parse).transpose()?,
            until: until.map(parse).transpose()?,
        };
        if w.since.zip(w.until).is_some_and(|(a, b)| a > b) {
            return Err(invalid("since must not exceed until"));
        }
        Ok(w)
    }
    pub fn active(&self) -> bool {
        self.since.is_some() || self.until.is_some()
    }
    pub fn includes(&self, at: Option<&str>) -> bool {
        if !self.active() {
            return true;
        }
        at.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|t| self.since.is_none_or(|s| t >= s) && self.until.is_none_or(|u| t < u))
    }
    pub fn json(&self) -> Value {
        json!({"since":self.since.map(|t|t.to_rfc3339()),"until":self.until.map(|t|t.to_rfc3339()),"bounds":"[since,until)","durations":"whole selected lifetimes, not overlap/proration","unknown_starts":"excluded when filtered"})
    }
    pub fn sql(&self, column: &str) -> String {
        let mut terms = vec!["1".to_owned()];
        for (bound, op, days) in [(self.since, ">=", -2), (self.until, "<", 2)] {
            if let Some(t) = bound {
                if let Some(coarse) = t
                    .checked_add_signed(Duration::days(days))
                    .filter(|d| (0..=9999).contains(&d.year()))
                {
                    terms.push(format!("{column}{op}'{}'", coarse.format("%Y-%m-%d")));
                }
                let key = crate::usage_totals::start_key(Some(&t.to_rfc3339())).unwrap();
                terms.push(format!(
                    "usage_saved_start({column}){op}'{}'",
                    key.replace('\'', "''")
                ));
            }
        }
        terms.join(" AND ")
    }
    pub fn indexed_sql(&self, column: &str) -> String {
        let (ls, ln) = self.lower();
        let (us, un) = self.upper();
        let s = seconds(column);
        let n = nanos(column);
        format!(
            "({s})>={ls} AND ({s})<={us} AND ({s},{n})>=({ls},{ln}) AND ({s},{n})<({us},{un}) AND ({} OR ({s})>-9223372036854775808)",
            if self.active() { 0 } else { 1 }
        )
    }
    pub fn lower(&self) -> (i64, u32) {
        self.since
            .map(|t| (t.timestamp(), t.timestamp_subsec_nanos()))
            .unwrap_or((i64::MIN, 0))
    }
    pub fn upper(&self) -> (i64, u32) {
        self.until
            .map(|t| (t.timestamp(), t.timestamp_subsec_nanos()))
            .unwrap_or((i64::MAX, u32::MAX))
    }
}
pub(crate) fn seconds(column: &str) -> String {
    include_str!("time-seconds.sql")
        .trim()
        .replace("{{time}}", column)
}
pub(crate) fn nanos(column: &str) -> String {
    include_str!("time-nanos.sql")
        .trim()
        .replace("{{time}}", column)
}
