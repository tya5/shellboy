//! UI phase anchors only; no scheduler, persistence or I/O.
use super::*;

#[derive(Clone, Debug, Default)]
pub struct SavedTiming {
    pub generation: Option<i64>,
    pub attempt_started_at: Option<String>,
    pub tool_started_at: Option<String>,
    pub wall_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl PartialEq for SavedTiming {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.attempt_started_at == other.attempt_started_at
            && self.tool_started_at == other.tool_started_at
    }
}
impl Eq for SavedTiming {}

#[derive(Clone, Debug)]
pub(super) struct PhaseClock {
    key: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        ActivityPhase,
    ),
    anchor: Option<String>,
    at: Instant,
    base: Option<Duration>,
    observed: bool,
}
impl PhaseClock {
    pub fn observe(view: &ActivityView, previous: Option<Self>, at: Instant) -> Option<Self> {
        Self::for_phase(view, view.phase(), previous, at)
    }
    pub fn request(view: &ActivityView, previous: Option<Self>, at: Instant) -> Option<Self> {
        let s = view.snapshot.as_ref()?;
        if s.phase != ActivityPhase::AwaitingModel {
            return None;
        }
        let phase = if s.attempt_id.is_some() && view.received_attempt == s.attempt_id {
            ActivityPhase::Receiving
        } else {
            ActivityPhase::AwaitingModel
        };
        Self::for_phase(view, phase, previous, at)
    }
    fn for_phase(
        view: &ActivityView,
        phase: ActivityPhase,
        previous: Option<Self>,
        at: Instant,
    ) -> Option<Self> {
        let snapshot = view.snapshot.as_ref()?;
        if !phase.is_active() {
            return None;
        }
        let attempt = (phase != ActivityPhase::Tools)
            .then(|| snapshot.attempt_id.clone())
            .flatten();
        let key = (
            snapshot.run_id.clone(),
            snapshot.turn_id.clone(),
            attempt,
            if phase == ActivityPhase::Tools {
                None
            } else {
                snapshot.timing.generation
            },
            phase,
        );
        let anchor = match phase {
            ActivityPhase::AwaitingModel => snapshot.timing.attempt_started_at.clone(),
            ActivityPhase::Tools => snapshot.timing.tool_started_at.clone(),
            _ => None,
        };
        if let Some(previous) = previous
            && previous.key == key
            && previous.anchor == anchor
        {
            return Some(previous);
        }
        let observed = anchor.is_none();
        let (at, base) = if phase == ActivityPhase::Receiving {
            let first = view.receive.as_ref().map_or(at, |r| r.first_at);
            (first, Some(Duration::ZERO))
        } else if let Some(saved) = &anchor {
            let base = chrono::DateTime::parse_from_rfc3339(saved)
                .ok()
                .zip(snapshot.timing.wall_at)
                .and_then(|(start, wall)| wall.signed_duration_since(start).to_std().ok());
            (at, base)
        } else {
            (at, Some(Duration::ZERO))
        };
        Some(Self {
            key,
            anchor,
            at,
            base,
            observed,
        })
    }
    pub fn note(&self, now: Instant) -> String {
        match self.base {
            Some(base) => format!(
                "{}{}s",
                if self.observed {
                    FooterText::Observed.text()
                } else {
                    ""
                },
                base.saturating_add(now.saturating_duration_since(self.at))
                    .as_secs()
            ),
            None => FooterText::UnknownAge.text().into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(id: &str, age: i64) -> ActivitySnapshot {
        let wall = chrono::DateTime::parse_from_rfc3339("2026-10-31T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        ActivitySnapshot {
            run_id: Some("r".into()),
            turn_id: Some("u".into()),
            attempt_id: Some(id.into()),
            phase: ActivityPhase::AwaitingModel,
            turn_in_progress: true,
            timing: SavedTiming {
                attempt_started_at: Some((wall - chrono::Duration::seconds(age)).to_rfc3339()),
                wall_at: Some(wall),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    #[test]
    fn saved_request_age_resets_with_attempt_not_turn_and_equal_facts_do_not_redraw() {
        let at = Instant::now();
        let mut view = ActivityView::default();
        let a = snapshot("a", 120);
        view.accept_at(1, a.clone(), at);
        assert_eq!(
            view.phase_time_note(at + Duration::from_secs(3)).as_deref(),
            Some("123s")
        );
        let mut later = a;
        later.timing.wall_at = Some(later.timing.wall_at.unwrap() + chrono::Duration::seconds(3));
        assert!(!view.accept_at(1, later, at + Duration::from_secs(3)));
        view.accept_at(2, snapshot("b", 7), at + Duration::from_secs(4));
        assert_eq!(
            view.phase_time_note(at + Duration::from_secs(5)).as_deref(),
            Some("8s")
        );
        assert!(!view.accept_at(1, snapshot("a", 999), at));
    }
    #[test]
    fn receive_before_snapshot_survives_old_phase_and_is_transport_not_generated_output() {
        let at = Instant::now();
        let mut view = ActivityView::default();
        view.receive_at("a", at);
        view.accept_at(
            1,
            ActivitySnapshot {
                phase: ActivityPhase::Completed,
                ..Default::default()
            },
            at,
        );
        view.accept_at(2, snapshot("a", 120), at + Duration::from_secs(2));
        assert_eq!(view.phase(), ActivityPhase::Receiving);
        assert_eq!(
            view.phase_time_note(at + Duration::from_secs(5)).as_deref(),
            Some("観測 5s")
        );
        assert!(!view.receive_at("b", at + Duration::from_secs(5)));
        view.accept_at(3, snapshot("b", 4), at + Duration::from_secs(6));
        assert_eq!(view.phase(), ActivityPhase::Receiving);
        assert!(!view.receive_at("a", at + Duration::from_secs(7)));
    }
    #[test]
    fn tools_and_attempt_have_separate_named_ages_and_terminal_facts_win() {
        let at = Instant::now();
        let mut view = ActivityView::default();
        let mut s = snapshot("a", 120);
        s.tool_running = true;
        s.timing.tool_started_at =
            Some((s.timing.wall_at.unwrap() - chrono::Duration::seconds(15)).to_rfc3339());
        view.accept_at(1, s, at);
        assert_eq!(view.phase(), ActivityPhase::Tools);
        assert_eq!(view.phase_time_note(at).as_deref(), Some("15s"));
        assert!(view.secondary_note(at).unwrap().contains("120s"));
        view.receive_at("a", at + Duration::from_secs(1));
        assert_eq!(view.phase(), ActivityPhase::Tools);
        assert!(
            view.secondary_note(at + Duration::from_secs(2))
                .unwrap()
                .contains("観測 1s")
        );
        let mut end = snapshot("a", 122);
        end.phase = ActivityPhase::Completed;
        end.tool_running = true;
        view.accept_at(2, end, at + Duration::from_secs(3));
        assert_eq!(view.phase(), ActivityPhase::Completed);
        assert!(view.phase_time_note(at).is_none());
    }
    #[test]
    fn bad_saved_timestamp_is_unknown_and_unavailable_does_not_keep_counting() {
        let at = Instant::now();
        let mut view = ActivityView::default();
        let mut s = snapshot("a", 1);
        s.timing.attempt_started_at = Some("not-a-date".into());
        view.accept_at(1, s, at);
        assert_eq!(
            view.phase_time_note(at).as_deref(),
            Some(FooterText::UnknownAge.text())
        );
        view.unavailable_at(2, at);
        assert!(
            view.phase_time_note(at + Duration::from_secs(100))
                .is_none()
        );
        assert_eq!(FooterText::Observed.key(), "ui.activity.age.observed");
    }
}
