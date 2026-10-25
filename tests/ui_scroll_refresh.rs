//! Transient raw-preview errors must not move a paused reader to older history.
use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use shellboy::{
    config::Config,
    presentation::{
        BlockKind, NodePresentation, OutputStream, PresentationBlock, PresentationField,
        PresentationKind,
    },
    ui::{UiNode, UiState, UiUpdate, ViewAnchor, draw},
};

fn tool_node(raw: bool) -> UiNode {
    let canonical = "saved combined preview";
    let mut node=UiNode{node_id:"tool".into(),role:"tool".into(),text:canonical.into(),base_char_offset:0,total_chars:canonical.chars().count(),reasoning_ranges:vec![],presentation:NodePresentation{
        kind:PresentationKind::ToolResult,title:"shell · exited".into(),blocks:vec![PresentationBlock{kind:BlockKind::ToolResult,stream:Some(if raw{OutputStream::Stdout}else{OutputStream::Combined}),text:if raw{(0..80).map(|i|format!("R{i:03}\n")).collect()}else{canonical.into()},source_start:0,source_end:canonical.chars().count(),source_exact:false,..Default::default()}],fields:vec![PresentationField{value_omitted:false,display:None,key:"/ui_raw_stream_preview".into(),label:"出力の表示範囲".into(),value:if raw{"保持中rawの表示用先頭抜粋（API保存本文と別）。stdout/stderr末尾まで取得。"}else{"rawのstream識別を取得できません（IoFailed）。保存済みstream情報を維持し、未識別部分は統合抜粋を表示。"}.into()}],details_omitted:!raw,..Default::default()}};
    if !raw {
        node.presentation.fields.push(PresentationField {
            value_omitted: false,
            display: None,
            key: "/ui_raw_stream_preview_error".into(),
            label: "出力の表示取得エラー".into(),
            value: "IoFailed".into(),
        });
    }
    node
}
fn top_source(ui: &mut UiState, terminal: &mut Terminal<TestBackend>) -> usize {
    terminal.draw(|frame| draw(frame, ui)).unwrap();
    let buffer = terminal.backend().buffer();
    for row in 2..buffer.area.height - 6 {
        let line = (1..buffer.area.width - 1)
            .map(|column| buffer[(column, row)].symbol())
            .collect::<String>();
        for (offset, ch) in line.char_indices() {
            if matches!(ch, 'P' | 'R')
                && line
                    .as_bytes()
                    .get(offset + 1..offset + 4)
                    .is_some_and(|b| b.iter().all(u8::is_ascii_digit))
            {
                let n = line[offset + 1..offset + 4].parse::<usize>().unwrap();
                return if ch == 'R' { 80 + n } else { n };
            }
        }
    }
    panic!("no numbered source row visible")
}
fn down(ui: &mut UiState) {
    ui.event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
}

#[test]
fn transient_raw_preview_failure_does_not_jump_up_after_a_downward_wheel() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("fixture".into());
    let prior = (0..80).map(|i| format!("P{i:03}\n")).collect::<String>();
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation: ui.view().generation,
        nodes: vec![
            UiNode {
                node_id: "prior".into(),
                role: "assistant".into(),
                text: prior.clone(),
                base_char_offset: 0,
                total_chars: prior.chars().count(),
                reasoning_ranges: vec![],
                presentation: Default::default(),
            },
            tool_node(true),
        ],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 2,
    });
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("prior".into()),
        char_offset: 65 * 5,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    let before = top_source(&mut ui, &mut terminal);
    assert_eq!(before, 65);
    down(&mut ui);
    let after_wheel = top_source(&mut ui, &mut terminal);
    assert!(after_wheel > before);
    let original = ui.view().nodes()[1].text.clone();
    // Same node, canonical payload and navigation scope. This is the fallback
    // presentation produced by streams::enrich after a temporary read deadline.
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "fixture".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        node: tool_node(false),
    });
    assert_eq!(ui.view().nodes()[1].text, original);
    assert_eq!(ui.view().anchor.node_id.as_deref(), Some("prior"));
    let after_reply = top_source(&mut ui, &mut terminal);
    assert!(
        after_reply >= after_wheel,
        "transient display read failure moved the visible source backwards: before={before}, downward={after_wheel}, after_reply={after_reply}"
    );
}

#[test]
fn transient_neighbor_preview_failure_cannot_clamp_a_paused_history_refresh_upward() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("fixture".into());
    let prior = (0..80).map(|i| format!("P{i:03}\n")).collect::<String>();
    let preceding = UiNode {
        node_id: "prior".into(),
        role: "assistant".into(),
        text: prior.clone(),
        base_char_offset: 0,
        total_chars: prior.chars().count(),
        reasoning_ranges: vec![],
        presentation: Default::default(),
    };
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation: ui.view().generation,
        nodes: vec![preceding.clone(), tool_node(true)],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 2,
    });
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("prior".into()),
        char_offset: 65 * 5,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    let before = top_source(&mut ui, &mut terminal);
    assert_eq!(before, 65);
    down(&mut ui);
    let after_wheel = top_source(&mut ui, &mut terminal);
    assert!(after_wheel > before && after_wheel < 80);
    // Background history_page refresh performs raw enrichment again. The
    // canonical bodies are unchanged; only the neighbor's raw read timed out.
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation: ui.view().generation,
        nodes: vec![preceding, tool_node(false)],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 2,
    });
    assert_eq!(ui.view().anchor.node_id.as_deref(), Some("prior"));
    let after_reply = top_source(&mut ui, &mut terminal);
    assert!(
        after_reply >= after_wheel,
        "temporary neighbor preview loss clamped the reader backward: before={before}, downward={after_wheel}, after_refresh={after_reply}"
    );
}

fn paused_tool() -> UiState {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("fixture".into());
    ui.apply(UiUpdate::History {
        thread_id: "fixture".into(),
        generation: ui.view().generation,
        nodes: vec![tool_node(true)],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
    });
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("tool".into()),
        char_offset: 0,
    };
    ui
}
fn body_reply(ui: &mut UiState, node: UiNode) {
    ui.apply(UiUpdate::NodeWindow {
        thread_id: "fixture".into(),
        generation: ui.view().generation,
        view_revision: ui.view().view_revision,
        node,
    });
}
fn has_raw(ui: &UiState) -> bool {
    ui.view().nodes()[0]
        .presentation
        .blocks
        .iter()
        .any(|b| b.stream == Some(OutputStream::Stdout) && b.text.contains("R000"))
}
#[test]
fn retained_raw_copy_keeps_the_current_read_error_visible_in_details() {
    let mut ui = paused_tool();
    body_reply(&mut ui, tool_node(false));
    assert!(has_raw(&ui));
    let fields = &ui.view().nodes()[0].presentation.fields;
    assert!(
        fields
            .iter()
            .any(|f| f.key == "/ui_raw_stream_preview_error" && f.value == "IoFailed")
    );
    assert!(
        fields
            .iter()
            .any(|f| f.key == "/ui_raw_stream_preview" && f.value.contains("保持済み")),
        "the detail must disclose that this is a cached copy, not a successful current read"
    );
}
#[test]
fn confirmed_expiry_and_missing_raw_are_not_hidden_by_the_cached_copy() {
    for code in ["OutputExpired", "TargetUnavailable"] {
        let mut ui = paused_tool();
        let mut incoming = tool_node(false);
        incoming
            .presentation
            .fields
            .iter_mut()
            .find(|f| f.key == "/ui_raw_stream_preview_error")
            .unwrap()
            .value = code.into();
        body_reply(&mut ui, incoming);
        assert!(!has_raw(&ui), "confirmed {code} must show its fallback");
        assert_eq!(
            ui.view().nodes()[0].presentation.blocks[0].stream,
            Some(OutputStream::Combined)
        );
    }
}
#[test]
fn successful_changed_rendering_and_different_source_ranges_are_accepted() {
    let mut ui = paused_tool();
    let mut wide = Terminal::new(TestBackend::new(80, 28)).unwrap();
    wide.draw(|frame| draw(frame, &mut ui)).unwrap();
    let mut success = tool_node(true);
    success.presentation.blocks[0].text = "NEW SUCCESSFUL PREVIEW AT ANOTHER WIDTH".into();
    body_reply(&mut ui, success);
    let mut narrow = Terminal::new(TestBackend::new(40, 28)).unwrap();
    narrow.draw(|frame| draw(frame, &mut ui)).unwrap();
    assert_eq!(
        ui.view().nodes()[0].presentation.blocks[0].text,
        "NEW SUCCESSFUL PREVIEW AT ANOTHER WIDTH"
    );
    for variant in 0..3 {
        let mut ui = paused_tool();
        let mut incoming = tool_node(false);
        match variant {
            0 => {
                incoming.text = "different canonical payload".into();
                incoming.total_chars = incoming.text.chars().count();
            }
            1 => incoming.base_char_offset = 1,
            _ => incoming.total_chars += 1,
        }
        body_reply(&mut ui, incoming);
        assert!(
            !has_raw(&ui),
            "changed source witness {variant} must not inherit previous raw content"
        );
    }
}
#[test]
fn following_the_latest_output_does_not_freeze_a_previous_preview() {
    let mut ui = paused_tool();
    ui.view_mut().follow = true;
    body_reply(&mut ui, tool_node(false));
    assert!(!has_raw(&ui));
}
