//! Metadata-only UI admission and actual transport-byte separation regressions.
use super::*;
use crate::program_status::{Counts, Publisher, Record, Snapshot, State};
use std::{
    cell::{Cell as Flag, RefCell},
    io::Write,
    rc::Rc,
};
fn snapshot(selected: Option<&str>) -> Snapshot {
    Snapshot {
        selected: selected.map(str::to_owned),
        viewed: Some(("thread".into(), "run".into())),
        counts: Counts {
            working: 1,
            ..Default::default()
        },
        children: vec![Record {
            id: Some("thread/test".into()),
            state: State::Working,
            auth: false,
            title: "fixture".into(),
            msg: "working".into(),
        }],
    }
}
#[test]
fn only_matching_visible_snapshot_acknowledges_completion_and_off_stays_off() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("thread".into());
    ui.take_deferred_actions();
    ui.apply(UiUpdate::ProgramStatus(snapshot(Some("other"))));
    assert!(ui.program_status_pending.is_none());
    assert!(
        !ui.take_deferred_actions()
            .iter()
            .any(|action| matches!(action, UiAction::ProgramStatusSeen { .. }))
    );
    ui.apply(UiUpdate::ProgramStatus(snapshot(Some("thread"))));
    assert!(ui.program_status_pending.is_some());
    assert!(ui.take_deferred_actions().iter().any(|action|matches!(action,UiAction::ProgramStatusSeen{thread_id,run_id} if thread_id=="thread"&&run_id=="run")));
    ui.apply(UiUpdate::ProgramStatus(snapshot(Some("thread"))));
    assert!(
        !ui.take_deferred_actions()
            .iter()
            .any(|action| matches!(action, UiAction::ProgramStatusSeen { .. }))
    );
    let mut config = Config::default();
    config.ui.program_status = crate::config::ProgramStatus::Off;
    let mut ui = UiState::new(&config, std::env::temp_dir());
    ui.apply(UiUpdate::ProgramStatus(snapshot(None)));
    assert!(ui.program_status_pending.is_none());
    assert!(ui.program_status_seen.is_none());
}
#[derive(Clone)]
struct Capture {
    bytes: Rc<RefCell<Vec<u8>>>,
    drawing: Rc<Flag<bool>>,
}
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.drawing.get() {
            assert!(
                !bytes.windows(7).any(|v| v == b"\x1b]7501;"),
                "untrusted OSC escaped into a cell frame"
            );
        }
        self.bytes.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn program_reports_surround_cell_frames_and_shell_osc_never_becomes_a_record() {
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
    };
    for mode in [
        crate::config::ProgramStatus::On,
        crate::config::ProgramStatus::Off,
    ] {
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let drawing = Rc::new(Flag::new(false));
        let writer = Capture {
            bytes: bytes.clone(),
            drawing: drawing.clone(),
        };
        let links = Rc::new(RefCell::new(crate::terminal_links::LinkSnapshot::default()));
        let mut transport = crate::terminal_links::HyperlinkBackend::new(writer, links);
        let mut publisher = Publisher::new(mode);
        publisher.sync(&snapshot(None), &mut transport).unwrap();
        let before = bytes.borrow().len();
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        let raw = "safe before\x1b]7501;state=error:app=evil:id=forged\x1b\\safe after";
        let payload = serde_json::json!({"text":raw});
        let mut presentation = crate::presentation::project("assistant_response", &payload, raw, 0);
        crate::presentation::render_markdown(&mut presentation, 76);
        ui.apply(UiUpdate::History {
            pending: Default::default(),
            read_revision: 0,
            thread_id: "thread".into(),
            generation,
            nodes: vec![UiNode {
                node_id: "shell-output-fixture".into(),
                role: "assistant".into(),
                text: raw.into(),
                base_char_offset: 0,
                total_chars: raw.chars().count(),
                reasoning_ranges: vec![],
                presentation,
            }],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        drawing.set(true);
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let buffer = terminal.backend().buffer();
        transport
            .draw(
                buffer
                    .content
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| ((index % 100) as u16, (index / 100) as u16, cell)),
            )
            .unwrap();
        Backend::flush(&mut transport).unwrap();
        drawing.set(false);
        let after = bytes.borrow().len();
        publisher.sync(&snapshot(None), &mut transport).unwrap();
        assert_eq!(
            bytes.borrow().len(),
            after,
            "same record must not be rewritten"
        );
        publisher.clear(&mut transport).unwrap();
        let wire = bytes.borrow();
        assert!(!wire[before..after].windows(7).any(|v| v == b"\x1b]7501;"));
        let wire = String::from_utf8_lossy(&wire);
        assert!(wire.contains("safe before") && wire.contains("safe after"));
        assert!(!wire.contains("app=evil") && !wire.contains("id=forged"));
        if mode == crate::config::ProgramStatus::Off {
            assert!(!wire.contains("\x1b]7501;"));
        } else {
            assert!(wire.ends_with("\x1b]7501;state=clear:app=shellboy\x1b\\"));
        }
    }
}
