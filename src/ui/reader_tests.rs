//! Regression of key geometry before drawing and across modal/resize transitions.
use super::*;
use ratatui::backend::TestBackend;
use unicode_width::UnicodeWidthStr;

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.bottom() {
        let mut x = area.x;
        while x < area.right() {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        text.push('\n');
    }
    text
}

fn state() -> UiState {
    UiState::new(&Config::default(), PathBuf::new())
}
fn key(ui: &mut UiState, code: KeyCode) {
    ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}
#[test]
fn help_page_uses_current_resize_before_any_draw() {
    let mut ui = state();
    ui.event(Event::Resize(120, 45));
    key(&mut ui, KeyCode::F(1));
    key(&mut ui, KeyCode::PageDown);
    assert_eq!(ui.help_scroll, 32);
    ui.event(Event::Resize(80, 20));
    key(&mut ui, KeyCode::PageDown);
    assert_eq!(
        ui.help_scroll, 39,
        "the smaller viewport advances seven rows without drawing and preserves both status rows"
    );
    key(&mut ui, KeyCode::PageUp);
    assert_eq!(ui.help_scroll, 32);
}
#[test]
fn reader_page_is_owned_by_current_popup_not_the_last_drawn_popup() {
    let mut ui = state();
    let mut terminal = Terminal::new(TestBackend::new(120, 45)).unwrap();
    key(&mut ui, KeyCode::F(1));
    terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
    key(&mut ui, KeyCode::Esc);
    ui.event(Event::Resize(80, 20));
    ui.management.open_text(ManagementView {
        title: "合成情報".into(),
        text: "資料\n".repeat(100),
        scroll: 0,
    });
    key(&mut ui, KeyCode::PageDown);
    assert_eq!(
        ui.management.as_ref().unwrap().scroll,
        6,
        "management has a two-line footer above the two status rows"
    );
    ui.confirmation = Some(Confirmation::ForceQuit);
    key(&mut ui, KeyCode::PageDown);
    let after_other_popup = ui.confirmation_scroll;
    let mut fresh = state();
    fresh.event(Event::Resize(80, 20));
    fresh.confirmation = Some(Confirmation::ForceQuit);
    key(&mut fresh, KeyCode::PageDown);
    assert_eq!(after_other_popup, fresh.confirmation_scroll);
    assert_ne!(after_other_popup, 6);
    assert_eq!(
        ui.management.as_ref().unwrap().scroll,
        6,
        "hidden reader stays unchanged"
    );
}
#[test]
fn confirmation_measures_the_actual_wide_popup_and_keeps_footer_fixed_when_small() {
    for (width, height) in [(120, 40), (200, 45), (35, 18), (8, 5)] {
        let mut ui = state();
        let source = format!("{}a", "日".repeat(53));
        ui.confirmation = Some(Confirmation::Operation {
            command: Command::Status(StatusArgs::default()),
            detail: source.clone(),
        });
        let area = Rect::new(0, 0, width, height);
        let (text, requested_height) = ui.confirmation.as_ref().unwrap().layout(area);
        assert_eq!(text, source);
        let body = popup::body_area(
            area,
            CONFIRMATION_CONTEXT,
            CONFIRMATION_FOOTER,
            Some(requested_height),
        );
        assert!(body.right() <= width && body.bottom() <= height);
        if width >= 120 {
            assert_eq!(body.width, 108);
            assert_eq!(
                requested_height, 8,
                "short content keeps a small context/footer frame"
            );
            assert_eq!(body.height, 2);
        }
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let screen = rendered_text(&terminal);
        if width >= 35 {
            assert_eq!(screen.matches('日').count(), 53);
            assert!(
                screen.contains("N / Esc 戻る") && screen.contains("Y 実行"),
                "{width}x{height}: {screen}"
            );
            key(&mut ui, KeyCode::PageDown);
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            let screen = rendered_text(&terminal);
            assert!(
                screen.contains("N / Esc 戻る"),
                "footer must not scroll with the body"
            );
        }
        key(&mut ui, KeyCode::Esc);
        assert!(ui.confirmation.is_none());
        assert_eq!(ui.confirmation_scroll, 0);
    }
}

#[test]
fn confirmation_line_measurement_includes_the_reserved_scrollbar_column() {
    let source = "a".repeat(321); // Fits exactly three 107-cell rows.
    let confirmation = Confirmation::Operation {
        command: Command::Status(StatusArgs::default()),
        detail: source,
    };
    let area = Rect::new(0, 0, 120, 40);
    let (_, height) = confirmation.layout(area);
    assert_eq!(
        height, 10,
        "three actual reader rows plus seven framing rows"
    );
    let body = popup::body_area(
        area,
        CONFIRMATION_CONTEXT,
        CONFIRMATION_FOOTER,
        Some(height),
    );
    assert_eq!(body.width, 108);
    assert_eq!(popup::reader_width(body), 107);
    assert_eq!(body.height, 3);
}
