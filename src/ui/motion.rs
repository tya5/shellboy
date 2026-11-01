//! The conversation's single motion slot (#90). Pure selection over the
//! already-projected view plus a one-cell overlay on visible cached rows: no
//! I/O, no layout, cache, source or selection change.
use super::activity::ActivityPhase;
use super::presentation::RowSource;
use super::view::ThreadView;
use crate::presentation::rows::{RowKind, SPINNER_FRAMES};
use ratatui::text::Line;

/// The one conversation row allowed to spin. Every other running row keeps
/// its static #83 mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MotionTarget {
    /// A foreground shell whose saved shell state is `running` and not
    /// released (`running_current`).
    Shell { execution_id: String },
    /// The provisional streaming answer of the current inflight attempt.
    Answer { node_id: String },
}

/// The selection rule, in one place: only while the selected Thread's Turn is
/// in progress, the newest foreground `running_current` shell (later node,
/// then later block); else the current attempt's provisional answer row while
/// the model is awaited or received; else nothing. Background or released
/// shells, stale saved rows and finished answers never spin. The winner is
/// chosen before viewport filtering: an off-screen winner shows no spinner
/// and never hands the slot to an older visible row.
pub(super) fn select(view: &ThreadView) -> Option<MotionTarget> {
    let snapshot = view.activity.snapshot.as_ref()?;
    if !snapshot.turn_in_progress {
        return None;
    }
    for node in view.nodes().iter().rev() {
        for block in node.presentation.blocks.iter().rev() {
            let Some(tool) = &block.tool else { continue };
            if tool.current_state.as_deref() == Some("running_current")
                && !tool.background
                && let Some(execution_id) = &tool.execution_id
            {
                return Some(MotionTarget::Shell {
                    execution_id: execution_id.clone(),
                });
            }
        }
    }
    if !matches!(
        view.activity.phase(),
        ActivityPhase::AwaitingModel | ActivityPhase::Receiving
    ) {
        return None;
    }
    let attempt = snapshot.attempt_id.as_deref()?;
    view.nodes()
        .iter()
        .rev()
        .find(|node| node.node_id == attempt)
        .filter(|node| node.role == "assistant" && node.presentation.provisional_stream)
        .map(|node| MotionTarget::Answer {
            node_id: node.node_id.clone(),
        })
}

pub(super) fn spinner_frame(index: usize) -> &'static str {
    SPINNER_FRAMES[index % SPINNER_FRAMES.len()]
}

/// Replace the static mark of the first visible row of `target` with one
/// one-column spinner frame. Returns whether a row was painted.
pub(super) fn paint_spinner(
    lines: &mut [Line<'_>],
    sources: &[RowSource],
    view: &ThreadView,
    target: &MotionTarget,
    frame: &'static str,
) -> bool {
    let mark = match target {
        MotionTarget::Shell { .. } => RowKind::Running.mark(),
        MotionTarget::Answer { .. } => RowKind::Assistant.mark(),
    };
    let matches = |source: &RowSource| match target {
        MotionTarget::Shell { execution_id } => source
            .item
            .as_ref()
            .is_some_and(|item| item.execution_id.as_deref() == Some(execution_id)),
        MotionTarget::Answer { node_id } => view
            .nodes()
            .get(source.node)
            .is_some_and(|node| &node.node_id == node_id),
    };
    for (line, source) in lines.iter_mut().zip(sources) {
        if !matches(source) {
            continue;
        }
        let Some(span) = line.spans.first_mut() else {
            continue;
        };
        let Some(rest) = span.content.strip_prefix(mark) else {
            continue;
        };
        span.content = format!("{frame}{rest}").into();
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;
    use crate::presentation::{
        BlockKind, NodePresentation, PresentationBlock, PresentationKind, ToolPresentation,
    };
    use unicode_width::UnicodeWidthStr;

    fn shell(id: &str, state: &str, background: bool) -> UiNode {
        UiNode {
            node_id: format!("node-{id}"),
            role: "assistant".into(),
            presentation: NodePresentation {
                kind: PresentationKind::Assistant,
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(ToolPresentation {
                        group_id: Some(format!("group-{id}")),
                        call_id: format!("call-{id}"),
                        call_node_id: Some(format!("node-{id}")),
                        command: format!("printf {id}"),
                        status: "running".into(),
                        current_state: Some(state.into()),
                        execution_id: Some(format!("exec-{id}")),
                        background,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn answer(attempt: &str, provisional: bool) -> UiNode {
        UiNode {
            node_id: attempt.into(),
            role: "assistant".into(),
            text: "途中の回答".into(),
            total_chars: 5,
            presentation: NodePresentation {
                kind: PresentationKind::Assistant,
                provisional_stream: provisional,
                blocks: vec![PresentationBlock {
                    kind: BlockKind::Prose,
                    text: "途中の回答".into(),
                    source_end: 5,
                    source_exact: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn activity(phase: ActivityPhase, in_progress: bool) -> activity::ActivitySnapshot {
        activity::ActivitySnapshot {
            run_id: Some("run".into()),
            turn_id: Some("turn".into()),
            attempt_id: Some("attempt".into()),
            phase,
            turn_in_progress: in_progress,
            ..Default::default()
        }
    }
    fn view(nodes: Vec<UiNode>, snapshot: activity::ActivitySnapshot) -> ThreadView {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        *view.nodes_mut_for_test() = nodes;
        view.activity.accept(1, snapshot);
        view
    }
    fn shell_target(id: &str) -> Option<MotionTarget> {
        Some(MotionTarget::Shell {
            execution_id: format!("exec-{id}"),
        })
    }

    #[test]
    fn one_rule_picks_newest_foreground_shell_then_current_answer_else_nothing() {
        let tools = activity(ActivityPhase::Tools, true);
        let mixed = view(
            vec![
                shell("old", "running_current", false),
                shell("new", "running_current", false),
                shell("bg", "background_current", true),
                shell("done", "exited", false),
            ],
            tools.clone(),
        );
        assert_eq!(select(&mixed), shell_target("new"));
        let older_only = view(
            vec![
                shell("old", "running_current", false),
                shell("new", "running_current", true),
            ],
            tools.clone(),
        );
        assert_eq!(
            select(&older_only),
            shell_target("old"),
            "background never wins"
        );
        let background_only = view(
            vec![shell("bg", "background_current", true)],
            activity(ActivityPhase::Background, false),
        );
        assert_eq!(select(&background_only), None);
        let finished_turn = view(
            vec![shell("new", "running_current", false)],
            activity(ActivityPhase::Completed, false),
        );
        assert_eq!(select(&finished_turn), None, "only the current Turn moves");

        let awaiting = activity(ActivityPhase::AwaitingModel, true);
        let streaming = view(vec![answer("attempt", true)], awaiting.clone());
        assert_eq!(
            select(&streaming),
            Some(MotionTarget::Answer {
                node_id: "attempt".into()
            })
        );
        let shell_wins = view(
            vec![
                shell("new", "running_current", false),
                answer("attempt", true),
            ],
            awaiting.clone(),
        );
        assert_eq!(select(&shell_wins), shell_target("new"));
        for nodes in [
            vec![answer("attempt", false)],
            vec![answer("older-attempt", true)],
            vec![],
        ] {
            assert_eq!(
                select(&view(nodes, awaiting.clone())),
                None,
                "saved, stale or not yet visible answers never spin"
            );
        }
    }

    #[test]
    fn spinner_swaps_one_mark_cell_on_one_row_without_moving_geometry() {
        let view = view(
            vec![
                shell("old", "running_current", false),
                shell("new", "running_current", false),
            ],
            activity(ActivityPhase::Tools, true),
        );
        let (cached, _, sources) = super::super::presentation::history(&view, 80);
        let target = select(&view).unwrap();
        let running = |lines: &[Line<'_>]| {
            lines
                .iter()
                .filter(|line| {
                    line.spans
                        .first()
                        .is_some_and(|s| s.content.starts_with(RowKind::Running.mark()))
                })
                .count()
        };
        assert_eq!(running(&cached), 2);
        for (index, frame) in SPINNER_FRAMES.iter().enumerate() {
            assert_eq!(spinner_frame(index), *frame);
            let mut painted = super::super::presentation::borrowed(&cached);
            assert!(paint_spinner(&mut painted, &sources, &view, &target, frame));
            assert_eq!(running(&painted), 1, "other running rows keep ⏵");
            let mut changed = 0;
            for (before, after) in cached.iter().zip(&painted) {
                assert_eq!(before.width(), after.width());
                assert_eq!(before.spans.len(), after.spans.len());
                let text: String = after.spans.iter().map(|s| s.content.as_ref()).collect();
                let original: String = before.spans.iter().map(|s| s.content.as_ref()).collect();
                if text != original {
                    changed += 1;
                    assert!(text.starts_with(frame), "{text}");
                    assert!(text.contains("printf new"), "{text}");
                    assert_eq!(frame.width_cjk(), 1);
                }
            }
            assert_eq!(changed, 1);
        }
        // An off-screen winner leaves visible rows untouched.
        let mut painted = super::super::presentation::borrowed(&cached[..0]);
        assert!(!paint_spinner(
            &mut painted,
            &sources[..0],
            &view,
            &target,
            "⠋"
        ));
    }

    #[test]
    fn answer_spinner_marks_only_its_first_body_row() {
        let view = view(
            vec![answer("attempt", true)],
            activity(ActivityPhase::Receiving, true),
        );
        let (cached, _, sources) = super::super::presentation::history(&view, 80);
        let target = select(&view).unwrap();
        let mut painted = super::super::presentation::borrowed(&cached);
        assert!(paint_spinner(&mut painted, &sources, &view, &target, "⠙"));
        let text = super::super::presentation::plain(&painted);
        assert!(text.contains("⠙ 途中の回答"), "{text}");
        assert!(!text.contains(RowKind::Assistant.mark()), "{text}");
        assert_eq!(
            super::super::presentation::plain(&cached).replace(RowKind::Assistant.mark(), "⠙"),
            text
        );
    }

    fn running_ui(config: &Config, background: bool) -> UiState {
        let mut ui = UiState::new(config, std::env::temp_dir());
        ui.select_thread("t".into());
        *ui.view_mut().nodes_mut_for_test() = vec![shell("new", "running_current", background)];
        let generation = ui.view().generation;
        ui.apply(UiUpdate::Activity {
            thread_id: "t".into(),
            generation,
            epoch: 1,
            snapshot: activity(ActivityPhase::Tools, true),
            observed_at: Instant::now(),
        });
        ui
    }

    #[test]
    fn spinner_shares_the_footer_clock_and_adds_no_frames() {
        let config = Config::default();
        let now = Instant::now();
        let mut counts = Vec::new();
        for background in [false, true] {
            let mut ui = running_ui(&config, background);
            assert_eq!(select(ui.view()).is_some(), !background);
            let layout = ui.history_layout(80, 20);
            let revision = ui.view().history_revision();
            let mut frames = 0;
            let mut glyphs = std::collections::BTreeSet::new();
            for tick in 0..=400u64 {
                ui.dirty = false;
                let at = now + Duration::from_millis(tick * 25);
                ui.animation_tick(at);
                if ui.dirty {
                    frames += 1;
                    glyphs.extend(ui.animation.spinner(at));
                }
                assert!(ui.take_deferred_actions().is_empty());
            }
            assert_eq!(ui.view().history_revision(), revision);
            assert!(Arc::ptr_eq(&layout, &ui.history_layout(80, 20)));
            assert_eq!(glyphs.is_empty(), background);
            assert!(glyphs.len() > 1 || background);
            counts.push(frames);
        }
        assert_eq!(counts[0], counts[1], "spinner rides the footer frames");
        assert!(counts[0] <= 101, "<=10fps over 10s: {}", counts[0]);
    }

    #[test]
    fn motion_off_freezes_every_region_on_static_marks() {
        let mut config = Config::default();
        config.ui.motion = crate::config::MotionMode::Off;
        let mut ui = running_ui(&config, false);
        let now = Instant::now();
        for tick in 0..=100u64 {
            ui.dirty = false;
            let at = now + Duration::from_millis(tick * 25);
            ui.animation_tick(at);
            assert!(!ui.dirty, "motion off never schedules a frame");
            assert_eq!(ui.animation.spinner(at), None);
            assert!(!ui.animation.blink_dim(at));
        }
        let spans = ui.animation.spans(
            ActivityPhase::Tools,
            Style::default(),
            now + Duration::from_millis(600),
        );
        assert_eq!(spans.len(), 1, "footer shows its static mark and label");
        assert!(crate::config::Config::parse("[ui]\nmotion='off'").is_ok());
        assert!(crate::config::Config::parse("[ui]\nmotion='slow'").is_err());
        assert_eq!(Config::default().ui.motion, crate::config::MotionMode::On);
    }
}
