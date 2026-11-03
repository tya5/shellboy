//! Durable, bounded evidence for suppressing unproductive automatic attempts.
use super::*;
use serde::{Deserialize, Serialize};

const VERSION: u8 = 1;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_MARKS: usize = 8192;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Reason {
    NoTarget,
    NoNewTarget,
    RejectedInvalidState,
    RejectedOther,
    CoverageLimited,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Guard {
    version: u8,
    turn: String,
    witness: super::super::replay::PathWitness,
    tools: String,
    // None is an explicitly incomplete bounded forecast, never an empty set.
    marks: Option<Vec<String>>,
    mutable_protection: bool,
    #[serde(default)]
    refresh_revision: Option<i64>,
    #[serde(default)]
    summary_frozen_current_turn: bool,
    pub reason: Reason,
    invalid_state: u32,
    other_rejections: u32,
}

impl Guard {
    fn valid(&self) -> bool {
        self.version == VERSION
            && !self.turn.is_empty()
            && self.turn.len() <= 1024
            && hex_digest(&self.witness.sha256)
            && hex_digest(&self.tools)
            && self.marks.as_ref().is_none_or(|marks| {
                marks.len() <= MAX_MARKS
                    && marks.iter().all(|mark| parse_mark(mark).is_some())
                    && marks
                        .windows(2)
                        .all(|pair| mark_key(&pair[0]) < mark_key(&pair[1]))
            })
    }

    pub(super) fn mark_refresh(&mut self, revision: i64) {
        self.refresh_revision = Some(revision);
    }

    /// A partial spill rewrites its own path. Rebase the no-progress witness
    /// only when no later selection change occurred after that adoption.
    pub(super) fn refresh_after_adoption(&mut self, snapshot: &Snapshot) -> bool {
        if self.refresh_revision != Some(snapshot.selection) || self.turn != snapshot.turn {
            return false;
        }
        let Some(input) = snapshot.input.as_ref() else {
            return false;
        };
        self.witness = input.witness().clone();
        self.tools = input.old_tool_frontier();
        self.refresh_revision = None;
        true
    }

    pub(super) fn freeze_summary_for_current_turn(&mut self, frozen: bool) {
        self.summary_frozen_current_turn = frozen;
    }

    /// When the latest tool anchor is already in the protected current Turn,
    /// appending another tool cannot add older summary sources. Path rewrites
    /// and new Turns remain eligible for a fresh forecast.
    pub(super) fn blocks_unknown_summary(&self, snapshot: &Snapshot) -> bool {
        self.summary_frozen_current_turn
            && self.turn == snapshot.turn
            && snapshot
                .input
                .as_ref()
                .is_some_and(|input| input.extends(&self.witness))
    }

    pub(super) fn same_witness(&self, snapshot: &Snapshot) -> bool {
        let Some(input) = snapshot.input.as_ref() else {
            return false;
        };
        self.turn == snapshot.turn
            && input.extends(&self.witness)
            && input.old_tool_frontier() == self.tools
    }

    pub(super) fn unchanged(&self, snapshot: &Snapshot) -> bool {
        !self.mutable_protection && self.same_witness(snapshot)
    }

    pub(super) fn has_new_kind(&self, marks: Option<&[String]>, kind: &str) -> bool {
        match (&self.marks, marks) {
            (Some(previous), Some(current)) => current
                .iter()
                .filter(|mark| mark.starts_with(kind))
                .any(|mark| {
                    let Some((key, chars)) = parse_mark(mark) else {
                        return true;
                    };
                    match previous.binary_search_by(|previous| mark_key(previous).cmp(key)) {
                        Ok(index) => {
                            parse_mark(&previous[index]).is_none_or(|(_, old)| chars > old)
                        }
                        Err(_) => true,
                    }
                }),
            // Coverage-limited evidence cannot prove that a changed path has
            // no new eligible target; identical paths still take the fast path.
            (_, None) => true,
            (None, Some(current)) => current.iter().any(|mark| mark.starts_with(kind)),
        }
    }

    #[cfg(test)]
    fn has_new_targets(&self, marks: Option<&[String]>) -> bool {
        self.has_new_kind(marks, "spill:")
            || self.has_new_kind(marks, "summary:")
            || self.has_new_kind(marks, "middle:")
    }

    pub(super) fn set_reason(&mut self, reason: Reason) {
        self.reason = reason;
    }

    pub(super) fn replace_candidates(
        &mut self,
        marks: Option<Vec<String>>,
        mutable_protection: bool,
    ) {
        self.marks = match (self.marks.take(), marks) {
            (Some(previous), Some(current)) => merge_marks(Some(previous), Some(current)),
            (None, Some(current)) => Some(current),
            (_, None) => None,
        };
        self.mutable_protection = mutable_protection;
    }

    pub(super) fn counts(&self) -> (u32, u32) {
        (self.invalid_state, self.other_rejections)
    }

    pub(super) fn record_reason(&mut self, reason: Reason) {
        self.reason = reason;
        match reason {
            Reason::RejectedInvalidState => {
                self.invalid_state = self.invalid_state.saturating_add(1)
            }
            Reason::RejectedOther => {
                self.other_rejections = self.other_rejections.saturating_add(1)
            }
            _ => {}
        }
    }

    pub(super) fn new(
        snapshot: &Snapshot,
        marks: Option<Vec<String>>,
        mutable_protection: bool,
        reason: Reason,
        previous: Option<&Self>,
    ) -> Option<Self> {
        let input = snapshot.input.as_ref()?;
        let mut guard = Self {
            version: VERSION,
            turn: snapshot.turn.clone(),
            witness: input.witness().clone(),
            tools: input.old_tool_frontier(),
            marks: match (previous.and_then(|previous| previous.marks.clone()), marks) {
                (Some(previous), Some(current)) => merge_marks(Some(previous), Some(current)),
                (None, Some(current)) => Some(current),
                (_, None) => None,
            },
            mutable_protection,
            refresh_revision: None,
            summary_frozen_current_turn: previous.is_some_and(|guard| {
                guard.summary_frozen_current_turn
                    && guard.turn == snapshot.turn
                    && snapshot
                        .input
                        .as_ref()
                        .is_some_and(|input| input.extends(&guard.witness))
            }),
            reason,
            invalid_state: previous.map_or(0, |guard| guard.invalid_state),
            other_rejections: previous.map_or(0, |guard| guard.other_rejections),
        };
        match reason {
            Reason::RejectedInvalidState => {
                guard.invalid_state = guard.invalid_state.saturating_add(1)
            }
            Reason::RejectedOther => {
                guard.other_rejections = guard.other_rejections.saturating_add(1)
            }
            _ => {}
        }
        guard.valid().then_some(guard)
    }
}

pub(super) fn has_new_marks(previous: &[String], current: &[String], kind: &str) -> bool {
    current.iter().filter(|mark| mark.starts_with(kind)).any(|mark| {
        let Some((key, chars)) = parse_mark(mark) else { return false; };
        match previous.binary_search_by(|old| mark_key(old).cmp(key)) {
            Ok(index) => parse_mark(&previous[index]).is_some_and(|(_, old)| chars > old),
            Err(_) => true,
        }
    })
}

fn hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_mark(mark: &str) -> Option<(&str, u64)> {
    let (key, chars) = mark.rsplit_once(':')?;
    let (kind, id) = key.split_once(':')?;
    if !matches!(kind, "spill" | "summary" | "middle") || !hex_digest(id) || chars.len() != 16 {
        return None;
    }
    Some((key, u64::from_str_radix(chars, 16).ok()?))
}
fn mark_key(mark: &str) -> &str {
    mark.rsplit_once(':').map_or("", |(key, _)| key)
}

fn merge_marks(previous: Option<Vec<String>>, current: Option<Vec<String>>) -> Option<Vec<String>> {
    let (Some(previous), Some(current)) = (previous, current) else {
        return None;
    };
    let mut merged = std::collections::BTreeMap::<String, u64>::new();
    for mark in previous.iter().chain(&current) {
        let (key, chars) = parse_mark(mark)?;
        merged
            .entry(key.to_owned())
            .and_modify(|old| *old = (*old).max(chars))
            .or_insert(chars);
    }
    if merged.len() > MAX_MARKS {
        return None;
    }
    Some(
        merged
            .into_iter()
            .map(|(key, chars)| format!("{key}:{chars:016x}"))
            .collect(),
    )
}

pub(super) async fn load(
    control: &Arc<Control>,
    thread: &str,
) -> Result<(Option<Guard>, bool), ApiError> {
    let key = crate::compaction::CompactionKeys::new(thread).automatic_guard();
    let status_key = crate::compaction::CompactionKeys::new(thread).automatic_status();
    let (body, has_status) = control.store.database.call(move |db| {
        let body = db.query_row("SELECT CASE WHEN octet_length(value)<=?2 THEN value END FROM app_meta WHERE key=?1", params![key, MAX_BYTES as i64], |row| row.get::<_,Option<String>>(0)).optional()?.flatten();
        let has_status = db.query_row("SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)", [status_key], |row| row.get(0))?;
        Ok((body, has_status))
    }).await.map_err(storage_error)?;
    Ok((
        body.and_then(|body| serde_json::from_str::<Guard>(&body).ok())
            .filter(Guard::valid),
        has_status,
    ))
}

pub(super) async fn save(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    revision: u64,
    guard: &Guard,
    reason: Reason,
) -> Result<bool, ApiError> {
    let key = crate::compaction::CompactionKeys::new(&snapshot.thread).automatic_guard();
    let status_key = crate::compaction::CompactionKeys::new(&snapshot.thread).automatic_status();
    let body = serde_json::to_string(guard)
        .map_err(|_| invalid("Automatic compaction evidence cannot be encoded"))?;
    if body.len() > MAX_BYTES {
        return Ok(false);
    }
    let status = json!({"run_id":snapshot.run,"turn_id":snapshot.turn,"reason":reason}).to_string();
    let thread = snapshot.thread.clone();
    let saved = control.store.database.call(move |db| {
        let tx = db.transaction()?;
        let current: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL AND selection_revision=?2)", params![thread, revision as i64], |row| row.get(0))?;
        if current {
            tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, body])?;
            tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![status_key, status])?;
        }
        tx.commit()?;
        Ok(current)
    }).await.map_err(storage_error)?;
    if saved {
        control.changed(&snapshot.thread);
    }
    Ok(saved)
}

pub(super) async fn show_status(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    reason: Reason,
) -> Result<bool, ApiError> {
    let status_key = crate::compaction::CompactionKeys::new(&snapshot.thread).automatic_status();
    let body = json!({"run_id":snapshot.run,"turn_id":snapshot.turn,"reason":reason}).to_string();
    let thread = snapshot.thread.clone();
    let revision = snapshot.selection;
    let saved = control.store.database.call(move |db| {
        Ok(db.execute("INSERT INTO app_meta(key,value) SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM thread WHERE id=?3 AND deleted_at IS NULL AND selection_revision=?4) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![status_key,body,thread,revision])? != 0)
    }).await.map_err(storage_error)?;
    if saved {
        control.changed(&snapshot.thread);
    }
    Ok(saved)
}

pub(super) async fn anchor_is_current_turn(
    control: &Arc<Control>,
    anchor: Option<String>,
    turn: &str,
) -> Result<bool, ApiError> {
    let Some(anchor) = anchor else {
        return Ok(true);
    };
    let turn = turn.to_owned();
    control
        .store
        .database
        .call(move |db| {
            let actual = db
                .query_row("SELECT turn_id FROM node WHERE id=?1", [anchor], |row| {
                    row.get::<_, Option<String>>(0)
                })
                .optional()?
                .flatten();
            Ok(actual.as_deref() == Some(turn.as_str()))
        })
        .await
        .map_err(storage_error)
}

pub(super) async fn current_revision(
    control: &Arc<Control>,
    thread: &str,
) -> Result<i64, ApiError> {
    let thread = thread.to_owned();
    control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT selection_revision FROM thread WHERE id=?1 AND deleted_at IS NULL",
                [thread],
                |row| row.get(0),
            )?)
        })
        .await
        .map_err(storage_error)
}

pub(super) async fn hide_status(control: &Arc<Control>, thread: &str) -> Result<(), ApiError> {
    let key = crate::compaction::CompactionKeys::new(thread).automatic_status();
    let changed = control
        .store
        .database
        .call(move |db| Ok(db.execute("DELETE FROM app_meta WHERE key=?1", [key])? != 0))
        .await
        .map_err(storage_error)?;
    if changed {
        control.changed(thread);
    }
    Ok(())
}

pub(super) async fn clear(control: &Arc<Control>, thread: &str) -> Result<(), ApiError> {
    let guard = crate::compaction::CompactionKeys::new(thread).automatic_guard();
    let status = crate::compaction::CompactionKeys::new(thread).automatic_status();
    control
        .store
        .database
        .call(move |db| {
            db.execute(
                "DELETE FROM app_meta WHERE key IN (?1,?2)",
                params![guard, status],
            )?;
            Ok(())
        })
        .await
        .map_err(storage_error)
}

/// Bounded preview runs on a read-only connection, not the single DB writer.
pub(super) async fn candidates(
    control: &Arc<Control>,
    thread: &str,
    input: Option<super::super::replay::ReplayLease>,
) -> Result<Option<crate::compaction::preview::AutomaticCandidates>, ApiError> {
    let path = control.store.database.path().to_path_buf();
    let thread = thread.to_owned();
    crate::presentation::work(move || {
        let mut db =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|error| storage_error(error.into()))?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| storage_error(error.into()))?;
        db.pragma_update(None, "query_only", true)
            .map_err(|error| storage_error(error.into()))?;
        let tx = db
            .transaction()
            .map_err(|error| storage_error(error.into()))?;
        let result = crate::compaction::preview::automatic_candidates_tx(&tx, &thread);
        match result {
            Ok(candidates) => {
                tx.commit().map_err(|error| storage_error(error.into()))?;
                Ok(Some(candidates))
            }
            Err(crate::storage::StorageError::Operation(error))
                if error.code == ErrorCode::InputLimit =>
            {
                if !input.as_ref().is_some_and(|input| {
                    input.proves_no_reducing_older_tools() && input.proves_no_active_middle()
                }) {
                    return Ok(None);
                }
                // The progress handler bounds metadata-only proof independently
                // of the read-only connection's wall-clock time.
                use std::sync::atomic::{AtomicBool, Ordering};
                let interrupted = Arc::new(AtomicBool::new(false));
                let flag = interrupted.clone();
                let mut ticks = 0u32;
                tx.progress_handler(
                    1000,
                    Some(move || {
                        ticks += 1;
                        if ticks >= 131 {
                            flag.store(true, Ordering::Relaxed);
                            true
                        } else {
                            false
                        }
                    }),
                )
                .map_err(|error| storage_error(error.into()))?;
                let proof = crate::compaction::preview::automatic_structural_no_target_tx(
                    &tx, &thread, true,
                );
                tx.progress_handler(0, None::<fn() -> bool>)
                    .map_err(|error| storage_error(error.into()))?;
                if interrupted.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                match proof {
                    Ok(Some(revision)) => {
                        tx.commit().map_err(|error| storage_error(error.into()))?;
                        Ok(Some(crate::compaction::preview::AutomaticCandidates {
                            revision,
                            marks: Vec::new(),
                            mutable_protection: false,
                        }))
                    }
                    Ok(None) => Ok(None),
                    Err(error) => Err(storage_error(error)),
                }
            }
            Err(error) => Err(storage_error(error)),
        }
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(kind: &str, id: u64, chars: u64) -> String {
        format!("{kind}:{id:064x}:{chars:016x}")
    }
    fn guard(marks: Option<Vec<String>>) -> Guard {
        Guard {
            version: VERSION,
            turn: "synthetic-turn".into(),
            witness: super::super::super::replay::PathWitness {
                nodes: 0,
                sha256: "0".repeat(64),
            },
            tools: "1".repeat(64),
            marks,
            mutable_protection: false,
            refresh_revision: None,
            summary_frozen_current_turn: false,
            reason: Reason::NoNewTarget,
            invalid_state: 0,
            other_rejections: 0,
        }
    }

    #[test]
    fn only_new_or_larger_eligible_sources_are_progress() {
        let old = vec![mark("spill", 1, 50), mark("summary", 3, 100)];
        let saved = guard(Some(old.clone()));
        assert!(saved.valid());
        assert!(!saved.has_new_targets(Some(&old)));
        assert!(!saved.has_new_targets(Some(&[mark("spill", 1, 49)])));
        assert!(saved.has_new_targets(Some(&[mark("spill", 1, 51)])));
        assert!(saved.has_new_targets(Some(&[mark("spill", 2, 1)])));
        assert!(
            saved.has_new_targets(Some(&[mark("summary", 1, 50)])),
            "spill and summary are separate opportunities"
        );
        assert!(
            saved.has_new_targets(None),
            "incomplete evidence must not be mistaken for no targets"
        );
    }

    #[test]
    fn active_shortfall_requires_a_new_or_larger_middle() {
        let before = vec![mark("middle", 1, 100)];
        assert!(!has_new_marks(&before, &before, "middle:"));
        assert!(!has_new_marks(&before, &[mark("middle", 1, 99)], "middle:"));
        assert!(has_new_marks(&before, &[mark("middle", 1, 101)], "middle:"));
        assert!(has_new_marks(&before, &[mark("middle", 2, 1)], "middle:"));
        assert!(!has_new_marks(&before, &[mark("spill", 2, 1000)], "middle:"));
    }

    #[test]
    fn removed_and_restored_sources_do_not_retrigger_and_growth_is_monotonic() {
        let old = vec![mark("spill", 1, 50), mark("summary", 3, 100)];
        let merged = merge_marks(
            Some(old.clone()),
            Some(vec![mark("spill", 1, 20), mark("summary", 4, 20)]),
        )
        .unwrap();
        let saved = guard(Some(merged));
        assert!(saved.valid());
        assert!(
            !saved.has_new_targets(Some(&old)),
            "restoring previously attempted sources is not new progress"
        );
        assert!(saved.has_new_targets(Some(&[mark("spill", 1, 51)])));
    }

    #[test]
    fn malformed_and_oversized_metadata_never_becomes_false_evidence() {
        assert!(!guard(Some(vec!["not-a-mark".into()])).valid());
        let mut reversed = vec![mark("summary", 1, 2), mark("spill", 1, 3)];
        assert!(!guard(Some(reversed.clone())).valid());
        reversed.sort();
        assert!(guard(Some(reversed)).valid());
        let many = (0..=MAX_MARKS)
            .map(|id| mark("spill", id as u64, 1))
            .collect::<Vec<_>>();
        assert!(
            merge_marks(Some(vec![]), Some(many)).is_none(),
            "overflow remains explicitly unknown"
        );
    }

    #[test]
    fn rejection_counters_saturate_without_recording_payloads() {
        let mut saved = guard(Some(vec![mark("summary", 1, 10)]));
        saved.invalid_state = u32::MAX;
        saved.record_reason(Reason::RejectedInvalidState);
        saved.record_reason(Reason::RejectedOther);
        assert_eq!(saved.invalid_state, u32::MAX);
        assert_eq!(saved.other_rejections, 1);
        let body = serde_json::to_string(&saved).unwrap();
        assert!(!body.contains("private synthetic prompt"));
        assert!(body.len() < MAX_BYTES);
        let restored: Guard = serde_json::from_str(&body).unwrap();
        assert!(restored.valid());
        assert_eq!(restored.counts(), (u32::MAX, 1));
        assert!(
            !restored.has_new_targets(saved.marks.as_deref()),
            "a fresh driver must suppress the same persisted candidates"
        );
    }
}
