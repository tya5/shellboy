//! Bounded observations of work owned by the selected saved Run, from the
//! owner's `shell.list` running rows and their waits.
use crate::{
    protocol::{ApiError, Command, ErrorCode, ShellWait, WaitCondition},
    ui::live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget, WaitInfo, WaitTarget},
};
use std::time::{Duration, Instant};

const MAX_SHELLS: u32 = 128;

/// One running shell row of the owner's `shell.list`; only these fields are read.
#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct RunningShell {
    pub execution_id: String,
    pub run_id: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub command_excerpt: Option<String>,
    pub state: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub wait: Option<ShellWait>,
}

/// One `shell.list` page of the selected Thread's running shells; `complete`
/// is false when more running shells exist than the page returned.
pub(super) struct RunningPage {
    pub shells: Vec<RunningShell>,
    pub complete: bool,
}

pub(super) struct RawLiveWork {
    run_id: String,
    rows: Vec<RawRow>,
    shells: u64,

    waits: Option<u64>,
    known_waits: u64,
    duplicate_shells: u64,
    observed_at: Instant,
    wall_at: chrono::DateTime<chrono::Utc>,
}
struct RawRow {
    kind: &'static str,
    id: String,
    related: Option<String>,
    execution: String,
    label: Option<String>,
    description: Option<String>,
    state: String,
    started_at: Option<String>,
    output_at: Option<String>,
    wait: Option<ShellWait>,
    wait_target: Option<WaitTarget>,
}

fn started(at: Option<&str>) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(at?).ok()
}

/// Order the Run's running shells and waits oldest first (unknown starts last)
/// and keep `limit` rows. A waiting shell is shown as its wait. Counts are the
/// owner's: `owned_shells` includes starting and stopping shells.
pub(super) fn assemble(
    run_id: &str,
    owned_shells: u64,
    page: &RunningPage,
    limit: usize,
) -> Result<RawLiveWork, ApiError> {
    if !(1..=10).contains(&limit) {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Live work row limit must be in 1..=10",
        ));
    }
    let shells: Vec<_> = page.shells.iter().filter(|s| s.run_id == run_id).collect();
    let known = page.complete;
    let mut rows: Vec<RawRow> = Vec::new();
    let mut waiting = std::collections::BTreeSet::new();
    for shell in &shells {
        match (&shell.wait, known) {
            (Some(wait), true) => {
                waiting.insert(shell.execution_id.clone());
                rows.push(RawRow {
                    kind: "wait",
                    id: wait.operation_id.clone(),
                    related: None,
                    execution: shell.execution_id.clone(),
                    label: None,
                    description: None,
                    state: "waiting".into(),
                    started_at: Some(wait.accepted_at.clone()),
                    output_at: None,
                    wait: Some(wait.clone()),
                    wait_target: None,
                });
            }
            _ => rows.push(RawRow {
                kind: "shell",
                id: shell.execution_id.clone(),
                related: shell.turn_id.clone(),
                execution: shell.execution_id.clone(),
                label: shell.command_excerpt.clone(),
                description: shell.description.clone(),
                state: shell.state.clone(),
                started_at: shell.started_at.clone(),
                output_at: None,
                wait: None,
                wait_target: None,
            }),
        }
    }
    rows.sort_by(|a, b| {
        let (left, right) = (
            started(a.started_at.as_deref()),
            started(b.started_at.as_deref()),
        );
        (left.is_none(), left, &a.id, a.kind).cmp(&(right.is_none(), right, &b.id, b.kind))
    });
    rows.truncate(limit);
    let known_waits = waiting.len() as u64;
    Ok(RawLiveWork {
        run_id: run_id.into(),
        rows,
        shells: owned_shells,
        waits: known.then_some(known_waits),
        known_waits,
        duplicate_shells: known_waits,
        observed_at: Instant::now(),
        wall_at: chrono::Utc::now(),
    })
}

/// A missing target is an unknown label, not a failed observation.
fn absent<T>(result: Result<T, ApiError>) -> Result<Option<T>, ApiError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if matches!(
                error.code,
                ErrorCode::TargetUnavailable | ErrorCode::ThreadNotFound | ErrorCode::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// The selected Run's live work: one `shell.list` page of the Thread's running
/// shells, then `shell.get` for each kept shell row's last output and each
/// kept wait's target label (at most `limit` rows, so at most 10 reads).
pub(super) async fn read(
    owner: &super::native_history::Owner<'_>,
    thread: &str,
    run_id: &str,
    owned_shells: u64,
    limit: usize,
    language: crate::config::UiLanguage,
) -> Result<RawLiveWork, ApiError> {
    #[derive(serde::Deserialize)]
    struct Page {
        items: Vec<RunningShell>,
        next_cursor: Option<String>,
    }
    let page: Page = owner
        .call(Command::ShellList(crate::protocol::ShellListArgs {
            thread_id: Some(thread.to_owned()),
            since: None,
            until: None,
            command_contains: None,
            state: Some("running".into()),
            exit_code: None,
            limit: MAX_SHELLS,
            cursor: None,
        }))
        .await?;
    let page = RunningPage {
        complete: page.next_cursor.is_none(),
        shells: page.items,
    };
    let mut raw = assemble(run_id, owned_shells, &page, limit)?;
    let shell =
        |id: String| Command::ShellGet(crate::protocol::ExecutionTarget { execution_id: id });
    for row in &mut raw.rows {
        if row.kind == "shell" {
            let details: crate::protocol::ShellDetails = owner.call(shell(row.id.clone())).await?;
            row.output_at = details.last_output_at;
            continue;
        }
        let Some(wait) = &row.wait else { continue };
        match &wait.first_condition {
            WaitCondition::ShellEnded { execution_id }
            | WaitCondition::ShellOutput { execution_id, .. } => {
                row.wait_target = Some(WaitTarget::Shell(execution_id.clone()));
                let target: Option<crate::protocol::ShellDetails> =
                    absent(owner.call(shell(execution_id.clone())).await)?;
                row.label = target.and_then(|target| target.command);
            }
            WaitCondition::RunEnded { run_id } => {
                if let Some(target) = &wait.target_thread_id {
                    row.label = thread_title(owner, target, language).await?;
                    row.wait_target = row.label.is_some().then(|| WaitTarget::Run(run_id.clone()));
                }
            }
            WaitCondition::TurnCompleted { .. } | WaitCondition::ThreadSettled { .. } => {
                if let Some(target) = &wait.target_thread_id {
                    row.label = thread_title(owner, target, language).await?;
                    row.wait_target = row
                        .label
                        .is_some()
                        .then(|| WaitTarget::Thread(target.clone()));
                }
            }
            WaitCondition::MessageDelivered { .. } | WaitCondition::ReplyReceived { .. } => {}
        }
    }
    raw.observed_at = Instant::now();
    raw.wall_at = chrono::Utc::now();
    Ok(raw)
}

/// The target Thread's display title from its `thread.get` header.
async fn thread_title(
    owner: &super::native_history::Owner<'_>,
    thread: &str,
    language: crate::config::UiLanguage,
) -> Result<Option<String>, ApiError> {
    Ok(
        absent(super::native_header(owner.data_dir, owner.instance, thread).await)?
            .map(|header| header.prepare_for_ui(language).title),
    )
}

pub(super) fn age(saved: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let saved = chrono::DateTime::parse_from_rfc3339(saved?).ok()?;
    now.signed_duration_since(saved).to_std().ok()
}
pub(super) fn label(value: Option<&str>, fallback: &str) -> String {
    let (safe, _) = crate::presentation::sanitize_plain(value.unwrap_or_default(), 0);
    let safe: String = safe.chars().take(512).collect();
    if safe.trim().is_empty() {
        fallback.into()
    } else {
        safe
    }
}

#[cfg(test)]
pub(super) fn prepare(raw: RawLiveWork) -> LiveWorkSnapshot {
    prepare_in(raw, crate::config::UiLanguage::Ja)
}

pub(super) fn prepare_in(
    raw: RawLiveWork,
    language: crate::config::UiLanguage,
) -> LiveWorkSnapshot {
    let observed_at = raw.observed_at;
    let now = raw.wall_at;
    let overflow = raw
        .shells
        .saturating_add(raw.known_waits)
        .saturating_sub(raw.duplicate_shells)
        .saturating_sub(raw.rows.len() as u64);
    let rows = raw
        .rows
        .into_iter()
        .map(|mut row| {
            let wait_info = (row.kind == "wait").then(|| WaitInfo {
                target: row.wait_target.take(),
                timeout: row
                    .wait
                    .as_ref()
                    .map(|wait| Duration::from_millis(wait.timeout_ms)),
            });
            let (target, display) = match row.kind {
                "shell" => (
                    LiveWorkTarget::Shell {
                        execution_id: row.id,
                        turn_id: row.related,
                    },
                    label(
                        row.label.as_deref(),
                        crate::ui::i18n::local!(
                            language,
                            "コマンド（内容未記録）",
                            "Command (not recorded)"
                        ),
                    ),
                ),
                _ => (
                    LiveWorkTarget::Wait {
                        operation_id: row.id,
                        execution_id: row.execution,
                    },
                    {
                        let condition = row.wait.as_ref().map(|wait| &wait.first_condition);
                        let kind = match condition {
                            Some(WaitCondition::ShellEnded { .. }) => {
                                crate::ui::i18n::local!(language, "shell の終了", "Shell ended")
                            }
                            Some(WaitCondition::ShellOutput { .. }) => {
                                crate::ui::i18n::local!(language, "shell の出力", "Shell output")
                            }
                            Some(WaitCondition::TurnCompleted { .. }) => {
                                crate::ui::i18n::local!(language, "子の回答", "Child response")
                            }
                            Some(WaitCondition::RunEnded { .. }) => {
                                crate::ui::i18n::local!(language, "Run の終了", "Run ended")
                            }
                            Some(WaitCondition::ThreadSettled { .. }) => crate::ui::i18n::local!(
                                language,
                                "会話の状態",
                                "Conversation state"
                            ),
                            _ => crate::ui::i18n::local!(
                                language,
                                "待機対象不明",
                                "Unknown wait target"
                            ),
                        };
                        let name = label(
                            row.label.as_deref(),
                            crate::ui::i18n::local!(language, "対象不明", "Unknown target"),
                        );
                        let others = row
                            .wait
                            .as_ref()
                            .map_or(0, |wait| wait.condition_count)
                            .saturating_sub(1);
                        let other_threads = match condition {
                            Some(WaitCondition::ThreadSettled { thread_ids, .. }) => {
                                thread_ids.len().saturating_sub(1)
                            }
                            _ => 0,
                        };
                        let thread_summary = if other_threads > 0 {
                            match language {
                                crate::config::UiLanguage::Ja => {
                                    crate::ui::text_width::glyphs::formatted!(
                                        " · 他{other_threads}会話",
                                        " | 他{other_threads}会話"
                                    )
                                }
                                crate::config::UiLanguage::En => {
                                    crate::ui::text_width::glyphs::formatted!(
                                        " · {other_threads} other threads",
                                        " | {other_threads} other threads"
                                    )
                                }
                            }
                        } else {
                            String::new()
                        };
                        crate::ui::text_width::glyphs::formatted!(
                            "{kind} · {name}{thread_summary}{}",
                            "{kind} | {name}{thread_summary}{}",
                            if others > 0 {
                                match language {
                                    crate::config::UiLanguage::Ja => {
                                        crate::ui::text_width::glyphs::formatted!(
                                            " · 他{others}件",
                                            " | 他{others}件"
                                        )
                                    }
                                    crate::config::UiLanguage::En => {
                                        crate::ui::text_width::glyphs::formatted!(
                                            " · {others} more",
                                            " | {others} more"
                                        )
                                    }
                                }
                            } else {
                                String::new()
                            }
                        )
                    },
                ),
            };
            LiveWorkRow {
                target,
                label: display,
                description: crate::tool_description::label(row.description.as_deref()),
                state: row.state,
                elapsed_base: age(row.started_at.as_deref(), now),
                output_age_base: age(row.output_at.as_deref(), now),
                started_at: row.started_at,
                last_output_at: row.output_at,
                wait_info,
            }
        })
        .collect();
    LiveWorkSnapshot {
        run_id: raw.run_id,
        rows,
        shells: raw.shells,

        waits: raw.waits,
        overflow,
        counts_capped: raw.shells == 128,
        observed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shell(id: &str, run: &str, started_at: &str) -> RunningShell {
        RunningShell {
            execution_id: id.into(),
            run_id: run.into(),
            turn_id: Some("u".into()),
            command_excerpt: Some("printf saved\u{1b}[31m command".into()),
            state: "running".into(),
            started_at: Some(started_at.into()),
            description: None,
            wait: None,
        }
    }
    /// The Run `r` with four running shells and one shell of another Run.
    fn page() -> RunningPage {
        RunningPage {
            shells: vec![
                shell("s-old", "r", "2020-01-01T00:00:00Z"),
                shell("s-new", "r", "2020-01-01T00:00:03Z"),
                shell("s-third", "r", "2020-01-01T00:00:04Z"),
                shell("s-fourth", "r", "2020-01-01T00:00:05Z"),
                shell("foreign", "lineage-run", "2010-01-01T00:00:00Z"),
            ],
            complete: true,
        }
    }
    fn wait(operation: &str, conditions: &[serde_json::Value], timeout_ms: u64) -> ShellWait {
        ShellWait {
            operation_id: operation.into(),
            accepted_at: "2020-01-01T00:00:02Z".into(),
            timeout_ms,
            condition_count: conditions.len() as u32,
            first_condition: serde_json::from_value(conditions[0].clone()).unwrap(),
            target_thread_id: None,
        }
    }
    fn waiting(page: &mut RunningPage, execution: &str, wait: ShellWait) {
        page.shells
            .iter_mut()
            .find(|shell| shell.execution_id == execution)
            .unwrap()
            .wait = Some(wait);
    }
    /// The kept wait row with the label and target the owner reads resolve.
    fn resolved(raw: &mut RawLiveWork, label: &str, target: WaitTarget) {
        let row = raw.rows.iter_mut().find(|row| row.kind == "wait").unwrap();
        row.label = Some(label.into());
        row.wait_target = Some(target);
    }

    #[test]
    fn row_limit_bounds_the_merge_without_changing_counts() {
        for (limit, rows, overflow) in [(1, 1, 3), (3, 3, 1), (10, 4, 0)] {
            let snapshot = prepare(assemble("r", 4, &page(), limit).unwrap());
            assert_eq!(snapshot.rows.len(), rows);
            assert_eq!(snapshot.overflow, overflow);
            assert_eq!(snapshot.shells, 4);
        }
        for invalid in [0, 11] {
            assert!(assemble("r", 4, &page(), invalid).is_err());
        }
    }

    #[test]
    fn owned_count_at_the_bound_is_a_lower_bound() {
        let snapshot = prepare(assemble("r", 128, &page(), 3).unwrap());
        assert_eq!(snapshot.shells, 128);
        assert!(snapshot.counts_capped);
        assert_eq!(snapshot.rows.len(), 3);
        assert_eq!(snapshot.overflow, 125);
    }

    #[test]
    fn selected_wait_describes_first_target_and_replaces_its_shell() {
        let mut page = page();
        waiting(
            &mut page,
            "s-old",
            wait(
                "w",
                &[
                    json!({"type":"shell_ended","execution_id":"s-new"}),
                    json!({"type":"run_ended","run_id":"child-run"}),
                ],
                300000,
            ),
        );
        let raw = || {
            let mut raw = assemble("r", 4, &page, 10).unwrap();
            resolved(&mut raw, "printf saved", WaitTarget::Shell("s-new".into()));
            raw
        };
        let snapshot = prepare(raw());
        assert_eq!(
            (snapshot.shells, snapshot.waits, snapshot.overflow),
            (4, Some(1), 0)
        );
        assert!(!snapshot.rows.iter().any(|r| matches!(&r.target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == "s-old")));
        let wait = snapshot
            .rows
            .iter()
            .find(|r| matches!(r.target, LiveWorkTarget::Wait { .. }))
            .unwrap();
        assert!(
            wait.label.contains("shell の終了")
                && wait.label.contains("printf saved")
                && wait.label.contains("他1件"),
            "{}",
            wait.label
        );
        assert_eq!(
            wait.wait_info,
            Some(WaitInfo {
                target: Some(WaitTarget::Shell("s-new".into())),
                timeout: Some(Duration::from_secs(300))
            })
        );
        let english = prepare_in(raw(), crate::config::UiLanguage::En);
        let english_wait = english
            .rows
            .iter()
            .find(|row| matches!(row.target, LiveWorkTarget::Wait { .. }))
            .unwrap();
        for expected in ["Shell ended", "1 more", "printf saved"] {
            assert!(
                english_wait.label.contains(expected),
                "{}",
                english_wait.label
            );
        }
        assert!(!english_wait.label.contains("shell の終了"));
    }

    #[test]
    fn multiple_thread_targets_are_summarized_and_missing_target_stays_unknown() {
        for (condition, label, expected) in [
            (
                json!({"type":"thread_settled", "thread_ids":["child","lineage"], "states":["completed"]}),
                Some("First child"),
                "First child | 他1会話",
            ),
            (
                json!({"type":"run_ended", "run_id":"missing-run"}),
                None,
                "対象不明",
            ),
        ] {
            let mut page = page();
            waiting(&mut page, "s-old", wait("w", &[condition], 30000));
            let mut raw = assemble("r", 4, &page, 10).unwrap();
            if let Some(label) = label {
                resolved(&mut raw, label, WaitTarget::Thread("child".into()));
            }
            let snapshot = prepare(raw);
            let wait = snapshot
                .rows
                .iter()
                .find(|r| matches!(r.target, LiveWorkTarget::Wait { .. }))
                .unwrap();
            assert!(wait.label.contains(expected), "{}", wait.label);
        }
    }

    #[test]
    fn common_description_preserves_real_command_label() {
        let mut page = page();
        page.shells[0].description = Some("Inspect files".into());
        let snapshot = prepare(assemble("r", 4, &page, 3).unwrap());
        let first = &snapshot.rows[0];
        assert_eq!(first.description.as_deref(), Some("Inspect files"));
        assert!(first.label.contains("printf saved"));
        assert!(
            matches!(&first.target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == "s-old")
        );
    }

    #[test]
    fn waits_count_only_for_the_selected_run_and_an_incomplete_page_is_unknown() {
        let mut page = page();
        let message = || {
            wait(
                "w",
                &[json!({"type":"message_delivered","message_id":"m"})],
                30000,
            )
        };
        waiting(&mut page, "s-old", message());
        waiting(
            &mut page,
            "foreign",
            wait(
                "other",
                &[json!({"type":"message_delivered","message_id":"m"})],
                30000,
            ),
        );
        let snapshot = prepare(assemble("r", 4, &page, 3).unwrap());
        assert_eq!(
            (snapshot.shells, snapshot.waits, snapshot.overflow),
            (4, Some(1), 1)
        );
        assert!(
            matches!(&snapshot.rows[0].target, LiveWorkTarget::Wait { operation_id, .. } if operation_id == "w")
        );
        assert!(
            matches!(&snapshot.rows[1].target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == "s-new")
        );
        page.complete = false;
        let unknown = prepare(assemble("r", 4, &page, 10).unwrap());
        assert_eq!(unknown.waits, None);
        assert!(
            unknown
                .rows
                .iter()
                .all(|row| !matches!(row.target, LiveWorkTarget::Wait { .. }))
        );
    }

    #[test]
    fn invalid_or_future_saved_times_stay_unknown_and_labels_are_bounded() {
        assert!(age(Some("invalid"), chrono::Utc::now()).is_none());
        assert!(age(Some("2999-01-01T00:00:00Z"), chrono::Utc::now()).is_none());
        assert!(age(None, chrono::Utc::now()).is_none());
        assert_eq!(
            label(Some(&"x".repeat(1000)), "fallback").chars().count(),
            512
        );
    }

    #[test]
    fn chronological_display_handles_fractional_and_offset_times_with_unknown_last() {
        let mut page = page();
        for (id, at) in [
            ("s-old", "2020-01-01T00:00:00Z"),
            ("s-new", "2020-01-01T00:00:00.1Z"),
            ("s-third", "invalid"),
            ("s-fourth", "2020-01-01T01:00:00.2+01:00"),
        ] {
            page.shells
                .iter_mut()
                .find(|shell| shell.execution_id == id)
                .unwrap()
                .started_at = Some(at.into());
        }
        let snapshot = prepare(assemble("r", 4, &page, 10).unwrap());
        let order: Vec<_> = snapshot
            .rows
            .iter()
            .map(|row| match &row.target {
                LiveWorkTarget::Shell { execution_id, .. } => execution_id.as_str(),
                LiveWorkTarget::Wait { operation_id, .. } => operation_id.as_str(),
            })
            .collect();
        assert_eq!(order, ["s-old", "s-new", "s-fourth", "s-third"]);
    }

    #[test]
    fn delayed_preparation_keeps_snapshot_clock_before_new_output_markers() {
        let raw = assemble("r", 4, &page(), 3).unwrap();
        let observed_at = raw.observed_at;
        let expected_elapsed = age(Some("2020-01-01T00:00:00Z"), raw.wall_at);
        // A real commit marker can arrive while the finite presentation worker
        // is occupied. Its time must stay newer than this older observation.
        std::thread::sleep(Duration::from_millis(5));
        let newer_marker = Instant::now();
        std::thread::sleep(Duration::from_millis(5));
        let prepared = prepare(raw);
        assert_eq!(prepared.observed_at, observed_at);
        assert!(
            prepared.observed_at < newer_marker,
            "UI supersession must preserve a marker newer than the observation, even if preparation happens later"
        );
        assert_eq!(prepared.rows[0].elapsed_base, expected_elapsed);
    }
}
