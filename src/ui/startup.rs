//! One-shot initialization screen. No file/DB operations or worker waits.
use super::{TerminalSession, i18n, theme};
use crate::config::UiLanguage;
use crate::{
    app::StartupStage,
    protocol::{ApiError, ErrorCode},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use std::{
    io,
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Screen {
    language: UiLanguage,
    motion: crate::config::MotionMode,
    elapsed_seconds: u64,
    migration: Option<(u32, u32)>,
    blink_dim: bool,
    stopping: bool,
    ignored_input: bool,
    error: Option<ApiError>,
}
impl Screen {
    fn event(&mut self, event: Event, cancel: &CancellationToken) -> bool {
        if matches!(event, Event::Paste(_)) {
            self.ignored_input = true;
            return false;
        }
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            return false;
        };
        if kind == KeyEventKind::Release {
            return false;
        }
        let quit = code == KeyCode::Esc
            || (modifiers.contains(KeyModifiers::CONTROL)
                && matches!(code, KeyCode::Char('c' | 'q')));
        if self.error.is_some() {
            return quit || code == KeyCode::Enter;
        }
        if quit {
            self.stopping = true;
            cancel.cancel();
        } else {
            self.ignored_input = true;
        }
        false
    }
}
fn stage_text(stage: StartupStage, language: UiLanguage) -> &'static str {
    match stage {
        StartupStage::Configuration => i18n::local!(
            language,
            "設定・保存先を確認しています",
            "Checking configuration and storage"
        ),
        StartupStage::OwnerLock => i18n::local!(
            language,
            "保存先の所有権を確認しています",
            "Checking storage ownership"
        ),
        StartupStage::Database => i18n::local!(
            language,
            "DBの確認・更新を進めています",
            "Checking and updating the database"
        ),
        StartupStage::DatabasePragmas => i18n::local!(
            language,
            "DBの設定を確認しています",
            "Checking database settings"
        ),
        StartupStage::DatabaseMigration => i18n::local!(
            language,
            "DBの確認・更新を進めています",
            "Checking and updating the database"
        ),
        StartupStage::DatabaseValidate => i18n::local!(
            language,
            "DBの構造を確認しています",
            "Validating database schema"
        ),
        StartupStage::DatabaseForeignKeys => i18n::local!(
            language,
            "DBの参照を確認しています",
            "Checking database references"
        ),
        StartupStage::DatabaseQuickCheck => i18n::local!(
            language,
            "DBの整合性を確認しています",
            "Checking database integrity"
        ),
        StartupStage::InterruptedWork => i18n::local!(
            language,
            "前回中断した処理を整理しています",
            "Recovering interrupted work"
        ),
        StartupStage::Control => i18n::local!(
            language,
            "実行環境を準備しています",
            "Preparing the runtime"
        ),
        StartupStage::Endpoint => {
            i18n::local!(language, "接続口を準備しています", "Preparing the endpoint")
        }
        StartupStage::Closing => i18n::local!(
            language,
            "保存処理と所有者の終了を待っています",
            "Waiting for saving and owner shutdown"
        ),
    }
}
fn draw(frame: &mut Frame<'_>, screen: &Screen, stage: StartupStage, tick: usize) {
    let language = screen.language;
    let area = frame.area();
    frame.render_widget(Paragraph::new(""), area);
    let zones = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .margin(2)
        .split(area);
    let heading = if screen.error.is_some() {
        i18n::local!(language, "起動できませんでした", "Startup failed")
    } else if screen.stopping {
        i18n::local!(language, "終了待ち", "Waiting for shutdown")
    } else {
        i18n::local!(language, "起動中", "Starting")
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            heading,
            Style::default().fg(theme::EMPHASIS),
        ))),
        zones[0],
    );
    let text = if let Some(error) = &screen.error {
        crate::presentation::sanitize_plain(
            &format!("{}\n{}", error, error.hint.as_deref().unwrap_or("")),
            0,
        )
        .0
    } else {
        let tick = if screen.motion == crate::config::MotionMode::Off {
            0
        } else {
            tick
        };
        let ascii = crate::presentation::rows::SPINNER_CANDIDATES[2];
        let spinner = super::text_width::glyphs::select(
            super::motion::spinner_frame(tick),
            ascii[tick % ascii.len()],
        );
        let elapsed = screen.elapsed_seconds;
        let time = match language {
            UiLanguage::Ja => format!("経過 {elapsed}秒"),
            UiLanguage::En => format!("Elapsed {elapsed}s"),
        };
        format!("{spinner} {}  {time}", stage_text(stage, language))
    };
    let mut lines = vec![Line::from(text)];
    if screen.error.is_none()
        && let Some((current, total)) = screen.migration
        && current > 0
        && current <= total
        && total <= crate::storage::SCHEMA_VERSION as u32
    {
        use super::text_width::UnicodeWidthStr;
        let label = match language {
            UiLanguage::Ja => format!("構造の更新 {current} / {total} 段"),
            UiLanguage::En => format!("Schema update {current} / {total}"),
        };
        let narrow = super::text_width::glyphs::narrow();
        let needed = if narrow {
            (total as usize).saturating_mul(2).saturating_sub(1)
        } else {
            total as usize + 2
        };
        let mut line = Vec::new();
        if needed + 2 + label.as_str().width() <= zones[1].width as usize {
            if narrow {
                let palette = theme::Palette::detect(crate::config::PaletteMode::Dark);
                for step in 1..=total {
                    if step > 1 {
                        line.push(Span::raw(" "));
                    }
                    let symbol = if step < current {
                        "●"
                    } else if step == current {
                        "◐"
                    } else {
                        "○"
                    };
                    let style = if step == current && screen.blink_dim {
                        palette.live_work_dim().unwrap_or_default()
                    } else {
                        palette.live_work()
                    };
                    line.push(Span::styled(symbol, style));
                }
            } else {
                let mut marks = String::from("[");
                for step in 1..=total {
                    marks.push(if step < current {
                        '#'
                    } else if step == current {
                        '>'
                    } else {
                        '-'
                    });
                }
                marks.push(']');
                line.push(Span::raw(marks));
            }
            line.push(Span::raw("  "));
        }
        line.push(Span::raw(label));
        lines.push(Line::from(line));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), zones[1]);
    let footer = if screen.error.is_some() {
        i18n::local!(language, "Enter / Esc 閉じる", "Enter / Esc Close")
    } else if screen.stopping {
        i18n::local!(
            language,
            "開始済み処理を中断せず、安全な終了を待っています",
            "Waiting for safe shutdown; started work is not interrupted"
        )
    } else if screen.ignored_input {
        crate::ui::text_width::glyphs::select(
            i18n::local!(
                language,
                "起動完了まで入力できません · Esc / Ctrl+C / Ctrl+Q 終了",
                "Input disabled during startup · Esc / Ctrl+C / Ctrl+Q Quit"
            ),
            i18n::local!(
                language,
                "起動完了まで入力できません | Esc / Ctrl+C / Ctrl+Q 終了",
                "Input disabled during startup | Esc / Ctrl+C / Ctrl+Q Quit"
            ),
        )
    } else {
        i18n::local!(
            language,
            "Esc / Ctrl+C / Ctrl+Q 終了",
            "Esc / Ctrl+C / Ctrl+Q Quit"
        )
    };
    frame.render_widget(Paragraph::new(footer).wrap(Wrap { trim: false }), zones[2]);
}
fn render(
    frame: &mut Frame<'_>,
    screen: &Screen,
    stage: StartupStage,
    tick: usize,
    snapshot: &crate::terminal_links::SharedLinkSnapshot,
) {
    draw(frame, screen, stage, tick);
    *snapshot.borrow_mut() = crate::terminal_links::LinkSnapshot {
        buffer: frame.buffer_mut().clone(),
        regions: vec![],
        repaint_rows: None.into(),
    };
}

/// Poll only bounded observations; completion and cleanup stay on async workers.
pub fn wait<T>(
    session: &mut TerminalSession,
    stages: &watch::Receiver<crate::app::StartupProgress>,
    result: &Receiver<Result<Option<T>, ApiError>>,
    cancel: &CancellationToken,
) -> io::Result<Result<Option<T>, ApiError>> {
    wait_inner(session, stages, result, cancel, None, Instant::now())
}
pub fn wait_with_termination<T>(
    session: &mut TerminalSession,
    stages: &watch::Receiver<crate::app::StartupProgress>,
    result: &Receiver<Result<Option<T>, ApiError>>,
    cancel: &CancellationToken,
    termination: &super::termination::Termination,
    start: Instant,
) -> io::Result<Result<Option<T>, ApiError>> {
    wait_inner(session, stages, result, cancel, Some(termination), start)
}
fn wait_inner<T>(
    session: &mut TerminalSession,
    stages: &watch::Receiver<crate::app::StartupProgress>,
    result: &Receiver<Result<Option<T>, ApiError>>,
    cancel: &CancellationToken,
    termination: Option<&super::termination::Termination>,
    start: Instant,
) -> io::Result<Result<Option<T>, ApiError>> {
    let mut detached = false;
    let mut screen = Screen {
        stopping: cancel.is_cancelled(),
        ..Default::default()
    };
    let mut drawn = None;
    let mut animation = super::activity::Animation::new(start);
    let mut ready = None;
    loop {
        if let Some(termination) = termination {
            if termination.forced() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "forced OS termination",
                ));
            }
            if termination.requested() {
                cancel.cancel();
                screen.stopping = true;
            }
        }
        if screen.error.is_none() && ready.is_none() {
            match result.try_recv() {
                Ok(Ok(value)) => ready = Some(value),
                Ok(Err(error)) => screen.error = Some(error),
                Err(TryRecvError::Disconnected) => {
                    screen.error = Some(ApiError::new(
                        ErrorCode::IoFailed,
                        "Initialization adapter stopped without a result",
                    ))
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        // Owner readiness is independent of capability-response arrival.
        if ready.is_some() {
            return Ok(Ok(ready.take().expect("ready result")));
        }
        if termination.is_some_and(super::termination::Termination::requested)
            && screen.error.is_some()
        {
            return Ok(Err(screen.error.take().unwrap()));
        }
        if detached {
            if screen.error.is_some() {
                return Ok(Err(screen.error.take().unwrap()));
            }
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        let rendered = (|| -> io::Result<bool> {
            super::termination::check_terminal()?;
            let progress = *stages.borrow();
            screen.language = progress.language;
            screen.motion = progress.motion;
            screen.elapsed_seconds = start.elapsed().as_secs();
            screen.migration = progress.migration;
            let palette = theme::Palette::detect(crate::config::PaletteMode::Dark);
            let now = Instant::now();
            let demand = super::activity::MotionDemand {
                footer: super::activity::ActivityPhase::Idle,
                spinner: true,
                blink: progress
                    .migration
                    .and_then(|_| palette.live_work_dim().map(|_| start)),
            };
            animation.tick_demand(
                now,
                demand,
                true,
                10,
                screen.motion == crate::config::MotionMode::On,
            );
            screen.blink_dim = animation.blink_dim(now);
            let stage = progress.stage;
            let tick = if screen.error.is_some() || screen.motion == crate::config::MotionMode::Off
            {
                0
            } else {
                (start.elapsed().as_millis() / 100) as usize
            };
            let signature = (
                stage,
                screen.language,
                screen.elapsed_seconds,
                screen.migration,
                screen.blink_dim,
                tick,
                screen.stopping,
                screen.ignored_input,
                screen.error.is_some(),
            );
            if drawn != Some(signature) {
                let snapshot = &session.link_snapshot;
                session
                    .terminal
                    .draw(|frame| render(frame, &screen, stage, tick, snapshot))?;
                drawn = Some(signature);
            }
            let event = session.capabilities.poll(
                &session._guard,
                session.diagnostics.as_ref(),
                Duration::from_millis(25),
            )?;
            if session.capabilities.take_changed() {
                drawn = None;
            }
            if let Some(event) = event {
                if matches!(event, Event::Resize(..)) {
                    drawn = None;
                }
                if screen.event(event, cancel) {
                    return Ok(true);
                }
            }
            Ok(false)
        })();
        match rendered {
            Ok(true) => return Ok(Err(screen.error.take().expect("failure acknowledgement"))),
            Ok(false) => {}
            Err(error) => {
                if let Some(termination) = termination {
                    termination.terminal_lost();
                    cancel.cancel();
                    screen.stopping = true;
                    detached = true;
                } else {
                    return Err(error);
                }
            }
        }
    }
}

/// Post-TUI stderr diagnostics stay English regardless of the configured UI locale.
pub fn terminal_error(error: impl std::fmt::Display) -> String {
    format!("Terminal error: {error}")
}
pub fn shutdown_error(error: impl std::fmt::Display) -> String {
    format!("Shutdown error: {error}")
}
pub fn runtime_adapter_failed() -> &'static str {
    "Runtime adapter failed"
}
pub fn forced_exit_notice() -> &'static str {
    "Forced exit: conversation saving and owned process cleanup may be incomplete. Restart to inspect interrupted work."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_spinner_and_footer_follow_measured_width() {
        use crate::ui::text_width::{self, UnicodeWidthStr};
        use ratatui::{Terminal, backend::TestBackend};
        for (policy, dots, separator) in [
            (Some(1), "⠹", " · "),
            (Some(2), "-", " | "),
            (None, "-", " | "),
        ] {
            text_width::set_terminal_width(policy);
            for width in [80, 120] {
                let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
                let screen = Screen {
                    ignored_input: true,
                    ..Default::default()
                };
                terminal
                    .draw(|frame| draw(frame, &screen, StartupStage::Database, 2))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let mut rows = Vec::new();
                for y in 0..20 {
                    let mut row = String::new();
                    let mut x = 0;
                    while x < width {
                        let symbol = buffer[(x, y)].symbol();
                        row.push_str(symbol);
                        x += symbol.width().max(1) as u16;
                    }
                    assert!(
                        row.width() <= usize::from(width),
                        "{policy:?} {width}: {row}"
                    );
                    rows.push(row);
                }
                let shown = rows.join("\n");
                assert!(shown.contains(dots), "{policy:?}: {shown}");
                assert!(shown.contains(separator), "{policy:?}: {shown}");
                assert!(shown.contains(stage_text(StartupStage::Database, screen.language)));
            }
        }
        text_width::clear_terminal_width();
    }
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn spinner_frames_keep_the_stage_at_the_same_column() {
        use crate::ui::text_width;
        for policy in [Some(1), Some(2), None] {
            text_width::set_terminal_width(policy);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let screen = Screen {
                    language,
                    ..Default::default()
                };
                let first = stage_text(StartupStage::Database, language)
                    .chars()
                    .next()
                    .unwrap()
                    .to_string();
                let mut positions = Vec::new();
                for tick in 0..20 {
                    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
                    terminal
                        .draw(|frame| draw(frame, &screen, StartupStage::Database, tick))
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let pos = (0..buffer.area.height)
                        .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
                        .find(|&(x, y)| buffer[(x, y)].symbol() == first)
                        .unwrap();
                    positions.push(pos);
                }
                assert!(
                    positions.iter().all(|position| *position == positions[0]),
                    "{policy:?} {language:?}: {positions:?}"
                );
            }
        }
        text_width::clear_terminal_width();
    }
    #[test]
    fn motion_off_freezes_the_same_spinner_for_all_frames() {
        for policy in [Some(1), None] {
            crate::ui::text_width::set_terminal_width(policy);
            let screen = Screen {
                motion: crate::config::MotionMode::Off,
                ..Default::default()
            };
            let mut snapshots = Vec::new();
            for tick in 0..20 {
                let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
                terminal
                    .draw(|frame| draw(frame, &screen, StartupStage::Database, tick))
                    .unwrap();
                snapshots.push(terminal.backend().buffer().clone());
            }
            assert!(snapshots.iter().all(|buffer| *buffer == snapshots[0]));
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn migration_markers_and_elapsed_use_real_counts_and_fit_width() {
        use crate::ui::text_width::{self, UnicodeWidthStr};
        fn visible(buffer: &ratatui::buffer::Buffer) -> String {
            let mut text = String::new();
            for y in 0..buffer.area.height {
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    text.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                text.push('\n');
            }
            text
        }
        for (policy, expected) in [(Some(1), "● ● ◐ ○ ○"), (None, "[##>--]")] {
            text_width::set_terminal_width(policy);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let screen = Screen {
                    language,
                    migration: Some((3, 5)),
                    elapsed_seconds: 12,
                    ..Default::default()
                };
                let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
                terminal
                    .draw(|frame| draw(frame, &screen, StartupStage::Database, 2))
                    .unwrap();
                let shown = visible(terminal.backend().buffer());
                assert!(shown.contains(expected), "{shown}");
                assert!(
                    shown.contains(if language == UiLanguage::Ja {
                        "経過 12秒"
                    } else {
                        "Elapsed 12s"
                    }),
                    "{shown}"
                );
                let mut small = Terminal::new(TestBackend::new(27, 20)).unwrap();
                small
                    .draw(|frame| draw(frame, &screen, StartupStage::Database, 2))
                    .unwrap();
                let shown = small
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(!shown.contains(expected), "{shown}");
            }
        }
        text_width::clear_terminal_width();
    }
    #[test]
    fn localized_startup_screens_preserve_controls_raw_errors_and_width() {
        for policy in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(policy);
            for width in [80, 120] {
                for language in [UiLanguage::En, UiLanguage::Ja] {
                    for stage in [
                        StartupStage::Configuration,
                        StartupStage::OwnerLock,
                        StartupStage::Database,
                        StartupStage::InterruptedWork,
                        StartupStage::Control,
                        StartupStage::Endpoint,
                        StartupStage::Closing,
                    ] {
                        for mode in 0..4 {
                            let screen = Screen {
                                language,
                                motion: crate::config::MotionMode::On,
                                elapsed_seconds: 0,
                                migration: None,
                                blink_dim: false,
                                stopping: mode == 2,
                                ignored_input: mode == 1,
                                error: (mode == 3).then(|| {
                                    ApiError::new(ErrorCode::IoFailed, "RAW_PROVIDER_ID_42")
                                        .hint("RAW_HINT_42")
                                }),
                            };
                            let before = screen
                                .error
                                .as_ref()
                                .map(|error| serde_json::to_value(error).unwrap());
                            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
                            terminal
                                .draw(|frame| draw(frame, &screen, stage, 0))
                                .unwrap();
                            let buffer = terminal.backend().buffer();
                            i18n::assert_invariant_ui_symbols(buffer, policy);
                            use crate::ui::text_width::UnicodeWidthStr;
                            let mut text = String::new();
                            for y in 0..buffer.area.height {
                                let mut x = 0;
                                while x < width {
                                    let symbol = buffer[(x, y)].symbol();
                                    text.push_str(symbol);
                                    x += symbol.width().max(1) as u16;
                                }
                                text.push('\n');
                            }
                            let heading = if mode == 3 {
                                i18n::local!(language, "起動できませんでした", "Startup failed")
                            } else if mode == 2 {
                                i18n::local!(language, "終了待ち", "Waiting for shutdown")
                            } else {
                                i18n::local!(language, "起動中", "Starting")
                            };
                            assert!(
                                text.contains(heading),
                                "{language:?} {width} {stage:?} {mode}"
                            );
                            if mode == 3 {
                                assert!(text.contains("RAW_PROVIDER_ID_42"));
                                assert!(text.contains("RAW_HINT_42"));
                                assert!(text.contains("Enter / Esc"));
                            } else if mode == 2 {
                                assert!(text.contains(i18n::local!(
                                    language,
                                    "安全な終了を待っています",
                                    "Waiting for safe shutdown"
                                )));
                            } else {
                                assert!(text.contains(stage_text(stage, language)));
                                assert!(text.contains("Ctrl+Q"));
                            }
                            assert_eq!(
                                before,
                                screen
                                    .error
                                    .as_ref()
                                    .map(|error| serde_json::to_value(error).unwrap())
                            );
                        }
                    }
                }
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }
    #[test]
    fn input_is_not_admitted_and_quit_only_requests_safe_finalization() {
        let token = CancellationToken::new();
        let mut screen = Screen::default();
        assert!(!screen.event(key(KeyCode::Char('x'), KeyModifiers::NONE), &token));
        assert!(!screen.event(Event::Paste("/send secret fixture".into()), &token));
        assert!(screen.ignored_input);
        assert!(!token.is_cancelled());
        assert!(!screen.event(key(KeyCode::Char('q'), KeyModifiers::CONTROL), &token));
        assert!(token.is_cancelled());
        assert!(screen.stopping);
        assert!(
            !screen.event(key(KeyCode::Esc, KeyModifiers::NONE), &token),
            "quit is not a completed cleanup"
        );
        screen.error = Some(ApiError::new(
            ErrorCode::ConfigInvalid,
            "fixture invalid configuration",
        ));
        assert!(screen.event(key(KeyCode::Enter, KeyModifiers::NONE), &token));
    }
    #[test]
    fn startup_frames_publish_current_wide_cells_and_clear_previous_links() {
        let snapshot = std::rc::Rc::new(std::cell::RefCell::new(
            crate::terminal_links::LinkSnapshot {
                regions: vec![crate::terminal_links::LinkRegion {
                    x: 2,
                    y: 2,
                    width: 4,
                    url: "https://example.invalid/stale".into(),
                }],
                ..Default::default()
            },
        ));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut screen = Screen {
            language: UiLanguage::Ja,
            ..Default::default()
        };
        terminal
            .draw(|frame| render(frame, &screen, StartupStage::Database, 0, &snapshot))
            .unwrap();
        assert_eq!(snapshot.borrow().buffer[(6, 2)].symbol(), "中");
        assert!(snapshot.borrow().regions.is_empty());
        screen.error = Some(ApiError::new(ErrorCode::ConfigInvalid, "fixture"));
        terminal
            .draw(|frame| render(frame, &screen, StartupStage::Configuration, 0, &snapshot))
            .unwrap();
        assert_eq!(snapshot.borrow().buffer[(6, 2)].symbol(), "で");
        assert_eq!(
            snapshot.borrow().buffer.area,
            terminal.backend().buffer().area
        );
    }

    #[test]
    fn startup_wait_failure_and_cleanup_fit_small_and_large_test_terminals() {
        for (width, height) in [(35, 12), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut screen = Screen {
                language: UiLanguage::Ja,
                ..Default::default()
            };
            terminal
                .draw(|frame| draw(frame, &screen, StartupStage::Database, 0))
                .unwrap();
            let cells = |terminal: &Terminal<TestBackend>| {
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
                    .replace(' ', "")
            };
            assert!(cells(&terminal).contains("起動中"));
            assert!(cells(&terminal).contains("DBの確認"));
            screen.stopping = true;
            terminal
                .draw(|frame| draw(frame, &screen, StartupStage::Closing, 0))
                .unwrap();
            assert!(cells(&terminal).contains("終了待ち"));
            screen.error = Some(ApiError::new(ErrorCode::ConfigInvalid, "SYNTHETIC_FAILURE"));
            terminal
                .draw(|frame| draw(frame, &screen, StartupStage::Configuration, 0))
                .unwrap();
            assert!(cells(&terminal).contains("起動できませんでした"));
            assert!(cells(&terminal).contains("SYNTHETIC_FAILURE"));
        }
    }
}

#[cfg(test)]
mod stderr_review_tests {
    use super::*;
    #[test]
    fn post_tui_stderr_diagnostics_are_english_and_keep_raw_errors() {
        let raw = "RAW provider error 原文";
        assert_eq!(terminal_error(raw), format!("Terminal error: {raw}"));
        assert_eq!(shutdown_error(raw), format!("Shutdown error: {raw}"));
        assert_eq!(runtime_adapter_failed(), "Runtime adapter failed");
        assert_eq!(
            forced_exit_notice(),
            "Forced exit: conversation saving and owned process cleanup may be incomplete. Restart to inspect interrupted work."
        );
        assert!(forced_exit_notice().is_ascii());
    }
}
