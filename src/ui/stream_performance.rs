//! Opt-in synthetic baseline, shared unchanged across the #29 optimization.
use super::*;

fn report(name: &str, mut samples: Vec<Duration>, extra: serde_json::Value) {
    samples.sort();
    let percentile = |p: usize| samples[(samples.len() * p).div_ceil(100).saturating_sub(1)];
    println!(
        "PERF {}",
        serde_json::json!({"name":name,"samples":samples.len(),
        "p50_us":percentile(50).as_secs_f64()*1e6,"p95_us":percentile(95).as_secs_f64()*1e6,"extra":extra})
    );
}
fn source(id: String, text: String, width: u16) -> UiNode {
    let mut node = UiNode {
        node_id: id,
        role: "assistant".into(),
        total_chars: text.chars().count(),
        presentation: crate::presentation::project_stream_window(&text, 0, &[], width),
        text,
        ..Default::default()
    };
    node.presentation.provisional_stream = false;
    crate::frontend::stream_metrics::seal(&mut node);
    node
}
#[test]
#[ignore = "synthetic 192KiB history and 120 streaming layout frames; run serially with --nocapture"]
fn performance_long_history_stream_layout_and_resize() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("layout-benchmark".into());
    let generation = ui.view().generation;
    let nodes = (0..127)
        .map(|n| {
            source(
                format!("history-{n}"),
                format!(
                    "## Source {n}\n\n{}\n```rust\nlet value = {n};\n```\n",
                    "Synthetic retained context 日本語 text. ".repeat(32)
                ),
                120,
            )
        })
        .collect::<Vec<_>>();
    let source_bytes = nodes.iter().map(|node| node.text.len()).sum::<usize>();
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "layout-benchmark".into(),
        generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 127,
    });
    let mut layout_times = vec![];
    let mut update_times = vec![];
    let mut text = "Long streamed answer 日本語.\n\n".repeat(256);
    let retention_before = ui.retention_passes;
    for n in 0..120 {
        text.push_str(&format!(
            "\nFrame {n}: **incremental output** 日本語 {}\n",
            "tail ".repeat(24)
        ));
        let node = source("attempt".into(), text.clone(), 120);
        let started = Instant::now();
        ui.apply(UiUpdate::Stream {
            thread_id: "layout-benchmark".into(),
            generation,
            node_id: node.node_id,
            role: node.role,
            text: node.text,
            base_char_offset: 0,
            total_chars: node.total_chars,
            reasoning_ranges: vec![],
            presentation: node.presentation,
            ack: None,
        });
        update_times.push(started.elapsed());
        let started = Instant::now();
        let layout = ui.history_layout(120, 30);
        layout_times.push(started.elapsed());
        ui.active_view_mut().viewport_row(&layout, 120, 30);
    }
    let layout = ui.history_layout(120, 30);
    let (lines, targets, sources) = presentation::history(ui.view(), 120);
    assert_eq!(
        layout.styled, lines,
        "incremental and uncached rendered rows must match"
    );
    assert_eq!(layout.targets, targets);
    assert_eq!(
        layout
            .sources
            .iter()
            .map(|s| (s.node, s.covered_end, s.offset))
            .collect::<Vec<_>>(),
        sources
            .iter()
            .map(|s| (s.node, s.covered_end, s.offset))
            .collect::<Vec<_>>()
    );
    let builds = ui.layout_cache.builds;
    let _ = ui.history_layout(120, 45);
    let height_builds = ui.layout_cache.builds - builds;
    report(
        "long_history_stream_layout",
        layout_times,
        serde_json::json!({"history_nodes":127,"retained_source_bytes":source_bytes,"stream_bytes":text.len(),"height_only_rebuilds":height_builds,"layout_builds":builds,"unit_builds":ui.layout_cache.unit_builds,"unit_hits":ui.layout_cache.unit_hits}),
    );
    report(
        "long_history_stream_update",
        update_times,
        serde_json::json!({"retention_passes":ui.retention_passes-retention_before}),
    );
    ui.take_deferred_actions();
    let started = Instant::now();
    let mut reloads = 0;
    for _ in 0..200 {
        ui.prepare_render_width(80);
        let requests = ui
            .take_deferred_actions()
            .into_iter()
            .filter_map(|action| match action {
                UiAction::LoadNodeWindow { node_id, .. } => Some(node_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        if requests.is_empty() {
            break;
        }
        for id in requests {
            reloads += 1;
            let mut node = ui
                .view()
                .nodes()
                .iter()
                .find(|node| node.node_id == id)
                .unwrap()
                .clone();
            node.presentation = crate::presentation::project_stream_window(
                &node.text,
                node.base_char_offset,
                &node.reasoning_ranges,
                80,
            );
            crate::frontend::stream_metrics::seal(&mut node);
            let view_revision = ui.view().view_revision;
            ui.apply(UiUpdate::NodeWindow {
                thread_id: "layout-benchmark".into(),
                generation,
                view_revision,
                node,
            });
        }
        let _ = ui.history_layout(80, 30);
    }
    report(
        "long_history_resize",
        vec![started.elapsed()],
        serde_json::json!({"from_width":120,"to_width":80,"reload_requests":reloads,"nodes":ui.view().nodes().len()}),
    );
}

fn assert_uncached_equivalent(ui: &mut UiState, width: u16, height: u16) {
    let layout = ui.history_layout(width, height);
    let (lines, targets, sources) = presentation::history(ui.view(), width);
    assert_eq!(layout.styled, lines);
    assert_eq!(
        layout.lines,
        Paragraph::new(presentation::borrowed(&lines))
            .wrap(Wrap { trim: false })
            .line_count(width)
    );
    assert_eq!(layout.targets, targets);
    assert_eq!(
        layout
            .sources
            .iter()
            .map(|s| (s.node, s.covered_end, s.offset))
            .collect::<Vec<_>>(),
        sources
            .iter()
            .map(|s| (s.node, s.covered_end, s.offset))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        ui.layout_cache.bytes,
        ui.layout_cache
            .entries
            .values()
            .chain(ui.layout_cache.units.values())
            .map(|entry| entry.bytes)
            .sum::<usize>()
    );
    assert!(ui.layout_cache.bytes <= ui.config.cache.layout_bytes as usize);
}
#[test]
fn render_units_reuse_grouped_history_and_keep_links_sources_and_budget_identical() {
    use crate::presentation::{
        BlockKind, EventClass, PresentationBlock, PresentationKind, ToolPresentation,
    };
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("layout-benchmark".into());
    let generation = ui.view().generation;
    let mut nodes = vec![source(
        "a".into(),
        "[first](https://example.com/a) 日本語\n```rust\nlet a=1;\n```".into(),
        60,
    )];
    for n in 0..2 {
        let mut event = source(format!("event{n}"), String::new(), 60);
        event.presentation.kind = PresentationKind::Event;
        event.presentation.event_class = EventClass::Change;
        event.presentation.title = format!("changed {n}");
        crate::frontend::stream_metrics::seal(&mut event);
        nodes.push(event);
    }
    for (kind, id) in [
        (BlockKind::ToolCall, "call"),
        (BlockKind::ToolResult, "result"),
    ] {
        let mut node = source(id.into(), String::new(), 60);
        node.presentation.blocks = vec![PresentationBlock {
            kind,
            tool: Some(ToolPresentation {
                call_id: "same-call".into(),
                command: "printf output".into(),
                status: "exited".into(),
                ..Default::default()
            }),
            ..Default::default()
        }];
        crate::frontend::stream_metrics::seal(&mut node);
        nodes.push(node);
    }
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "layout-benchmark".into(),
        generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 5,
    });
    assert_uncached_equivalent(&mut ui, 60, 10);
    let first = ui.history_layout(60, 10);
    assert!(Arc::ptr_eq(&first, &ui.history_layout(60, 30)));
    for n in 0..4 {
        let node = source(
            "attempt".into(),
            format!("stream {n}: [last](https://example.com/{n}) **日本語**"),
            60,
        );
        ui.apply(UiUpdate::Stream {
            thread_id: "layout-benchmark".into(),
            generation,
            node_id: node.node_id,
            role: node.role,
            text: node.text,
            base_char_offset: 0,
            total_chars: node.total_chars,
            reasoning_ranges: vec![],
            presentation: node.presentation,
            ack: None,
        });
        assert_uncached_equivalent(&mut ui, 60, 10);
    }
    assert!(
        ui.layout_cache.unit_hits >= 6,
        "unchanged render units must survive stream revisions"
    );
    assert_uncached_equivalent(&mut ui, 30, 10);
    // Zero budget retains no unit cache and produces the same renderer output.
    ui.config.cache.layout_bytes = 0;
    assert_uncached_equivalent(&mut ui, 60, 10);
    assert!(ui.layout_cache.units.is_empty());
}

#[test]
fn resize_reprojects_every_visible_short_node_and_defers_offscreen_nodes() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("layout-benchmark".into());
    let generation = ui.view().generation;
    let nodes = (0..30)
        .map(|n| source(format!("short-{n}"), format!("Short source {n}"), 120))
        .collect();
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "layout-benchmark".into(),
        generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 30,
    });
    let layout = ui.history_layout(122, 55);
    let row = ui.viewport_row(&layout, 122, 55);
    let visible = visible_render_nodes(&layout, row, 55, ui.view().nodes().len());
    assert!(
        visible.len() > 20,
        "short-message viewport must contain substantially more than three nodes"
    );
    let mut expected = working_node_ids(ui.view())
        .into_iter()
        .collect::<BTreeSet<_>>();
    expected.extend(
        visible
            .iter()
            .map(|index| ui.view().nodes()[*index].node_id.clone()),
    );
    assert!(
        expected.len() < 30,
        "fixture must also retain offscreen nodes"
    );
    ui.take_deferred_actions();
    let mut requested = BTreeSet::new();
    for _ in 0..30 {
        let preflight = RenderPreflight::new(ui.view());
        let projected = ui.history_layout(82, 55);
        let row = ui.viewport_row(&projected, 82, 55);
        let visible = visible_render_nodes(&projected, row, 55, ui.view().nodes().len());
        ui.prepare_visible_render_width(80, &visible, preflight);
        let requests = ui
            .take_deferred_actions()
            .into_iter()
            .filter_map(|action| match action {
                UiAction::LoadNodeWindow { node_id, .. } => Some(node_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        if requests.is_empty() {
            break;
        }
        for id in requests {
            assert!(
                requested.insert(id.clone()),
                "same-width source must not be requested twice"
            );
            let old = ui
                .view()
                .nodes()
                .iter()
                .find(|node| node.node_id == id)
                .unwrap();
            let node = source(id.clone(), old.text.clone(), 80);
            let view_revision = ui.view().view_revision;
            ui.apply(UiUpdate::NodeWindow {
                thread_id: "layout-benchmark".into(),
                generation,
                view_revision,
                node,
            });
        }
    }
    assert_eq!(
        requested, expected,
        "reload all visible nodes plus only the existing three-node lookahead"
    );
    let before = ui.history_layout(82, 55);
    let builds = ui.layout_cache.builds;
    let taller = ui.history_layout(82, 65);
    assert!(Arc::ptr_eq(&before, &taller));
    assert_eq!(ui.layout_cache.builds, builds);
    let row = ui.viewport_row(&taller, 82, 65);
    let taller_visible = visible_render_nodes(&taller, row, 65, ui.view().nodes().len());
    assert!(
        taller_visible.len() > visible.len(),
        "new height must expose additional source nodes without rebuilding layout"
    );
}
