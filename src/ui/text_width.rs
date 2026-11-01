//! Nonblocking observation of terminal ambiguous-character width.

/// A terminal observation, not a user preference or a guessed locale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum AmbiguousWidth {
    One,
    Two,
    #[default]
    Unknown,
}

/// Nonblocking state machine. The caller owns terminal input and routes only
/// cursor-position reports here; other events must keep their original order.
pub(super) struct Probe {
    deadline: std::time::Instant,
    first: Option<AmbiguousWidth>,
    result: Option<AmbiguousWidth>,
}

impl Probe {
    pub(super) fn new(now: std::time::Instant, timeout: std::time::Duration) -> Self {
        Self {
            deadline: now + timeout,
            first: None,
            result: None,
        }
    }

    /// Reports use zero-based coordinates. Queries are emitted after the first
    /// and second copies of the ambiguous character on terminal row 11.
    pub(super) fn observe(&mut self, column: u16, row: u16, now: std::time::Instant) {
        if self.finish_if_expired(now).is_some() {
            return;
        }
        let candidate = match (self.first, column, row) {
            (None, 1, 10) | (Some(AmbiguousWidth::One), 2, 10) => AmbiguousWidth::One,
            (None, 2, 10) | (Some(AmbiguousWidth::Two), 4, 10) => AmbiguousWidth::Two,
            _ => {
                self.result = Some(AmbiguousWidth::Unknown);
                return;
            }
        };
        if self.first.is_none() {
            self.first = Some(candidate);
        } else {
            self.result = Some(candidate);
        }
    }

    pub(super) fn fail(&mut self) {
        self.result = Some(AmbiguousWidth::Unknown);
    }

    pub(super) fn finish_if_expired(&mut self, now: std::time::Instant) -> Option<AmbiguousWidth> {
        if self.result.is_none() && now >= self.deadline {
            self.result = Some(AmbiguousWidth::Unknown);
        }
        self.result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn unknown_is_safe_and_east_asian_text_retains_fixed_width() {
        unicode_width::set_terminal_width(Some(1));
        assert_eq!("A\u{00b7}日".width(), 4);
        unicode_width::set_terminal_width(Some(2));
        assert_eq!("A\u{00b7}日".width(), 5);
        unicode_width::set_terminal_width(None);
        assert_eq!("A\u{00b7}日".width(), 5);
        unicode_width::clear_terminal_width();
    }

    #[test]
    fn probe_uses_only_matching_responses_before_the_deadline() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let mut narrow = Probe::new(now, Duration::from_millis(200));
        narrow.observe(1, 10, now);
        assert_eq!(narrow.finish_if_expired(now), None);
        narrow.observe(2, 10, now);
        assert_eq!(narrow.finish_if_expired(now), Some(AmbiguousWidth::One));
        let mut wide = Probe::new(now, Duration::from_millis(200));
        wide.observe(2, 10, now);
        wide.observe(4, 10, now);
        assert_eq!(wide.finish_if_expired(now), Some(AmbiguousWidth::Two));
        let mut inconsistent = Probe::new(now, Duration::from_millis(200));
        inconsistent.observe(1, 10, now);
        inconsistent.observe(4, 10, now);
        assert_eq!(
            inconsistent.finish_if_expired(now),
            Some(AmbiguousWidth::Unknown)
        );
        let mut timeout = Probe::new(now, Duration::from_millis(200));
        timeout.observe(2, 10, now + Duration::from_secs(1));
        assert_eq!(
            timeout.finish_if_expired(now),
            Some(AmbiguousWidth::Unknown)
        );
    }

    #[test]
    fn emoji_sequence_reserves_the_sum_of_components() {
        unicode_width::set_terminal_width(None);
        assert_eq!("👩\u{200d}💻".width(), 4);
        assert_eq!("❤\u{fe0f}".width(), 2);
        assert_eq!("🇯🇵".width(), 2);
        assert_eq!("👍🏽".width(), 4);
        assert_eq!("1\u{20e3}".width(), 2);
        unicode_width::clear_terminal_width();
    }
}
