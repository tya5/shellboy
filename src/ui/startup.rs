//! One-shot initialization screen. No file/DB operations or worker waits.
use super::{TerminalSession, i18n, theme};
use crate::config::UiLanguage;
use crate::{
    app::StartupStage,
    protocol::{ApiError, ErrorCode},
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
        format!(
            "{} {}",
            ["|", "||", "|||", "||"][tick % 4],
            stage_text(stage, language)
        )
    };
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), zones[1]);
    let footer = if screen.error.is_some() {
        i18n::local!(language, "Enter / Esc 閉じる", "Enter / Esc Close")
    } else if screen.stopping {
        i18n::local!(
            language,
            "開始済み処理を中断せず、安全な終了を待っています",
            "Waiting for safe shutdown; started work is not interrupted"
        )
    } else if screen.ignored_input {
        i18n::local!(
            language,
            "起動完了まで入力できません | Esc / Ctrl+C / Ctrl+Q 終了",
            "Input disabled during startup | Esc / Ctrl+C / Ctrl+Q Quit"
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
    };
}

/// Poll only bounded observations; completion and cleanup stay on async workers.
pub fn wait<T>(
    session: &mut TerminalSession,
    stages: &watch::Receiver<crate::app::StartupProgress>,
    result: &Receiver<Result<Option<T>, ApiError>>,
    cancel: &CancellationToken,
) -> io::Result<Result<Option<T>, ApiError>> {
    wait_inner(session, stages, result, cancel, None)
}
pub fn wait_with_termination<T>(
    session: &mut TerminalSession,
    stages: &watch::Receiver<crate::app::StartupProgress>,
    result: &Receiver<Result<Option<T>, ApiError>>,
    cancel: &CancellationToken,
    termination: &super::termination::Termination,
) -> io::Result<Result<Option<T>, ApiError>> {
    wait_inner(session, stages, result, cancel, Some(termination))
}
fn wait_inner<T>(
    session: &mut TerminalSession,
    stages: &watch::Receiver<crate::app::StartupProgress>,
    result: &Receiver<Result<Option<T>, ApiError>>,
    cancel: &CancellationToken,
    termination: Option<&super::termination::Termination>,
) -> io::Result<Result<Option<T>, ApiError>> {
    let mut detached = false;
    let mut screen = Screen {
        stopping: cancel.is_cancelled(),
        ..Default::default()
    };
    let start = Instant::now();
    let mut drawn = None;
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
        // Keep handling input while the bounded probe runs, but do not start
        // presentation workers with a width that could subsequently change.
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
            let stage = progress.stage;
            let tick = if screen.error.is_some() {
                0
            } else {
                (start.elapsed().as_millis() / 150) as usize
            };
            let signature = (
                stage,
                screen.language,
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
            if event::poll(Duration::from_millis(25))? {
                let event = event::read()?;
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

/// Human-facing TUI adapter failures; shared ApiError values remain untouched.
pub fn terminal_error(language: UiLanguage, error: impl std::fmt::Display) -> String {
    match language {
        UiLanguage::Ja => format!("端末エラー: {error}"),
        UiLanguage::En => format!("Terminal error: {error}"),
    }
}
pub fn shutdown_error(language: UiLanguage, error: impl std::fmt::Display) -> String {
    match language {
        UiLanguage::Ja => format!("終了処理エラー: {error}"),
        UiLanguage::En => format!("Shutdown error: {error}"),
    }
}
pub fn runtime_adapter_failed(language: UiLanguage) -> &'static str {
    i18n::local!(
        language,
        "実行環境との接続に失敗しました",
        "Runtime adapter failed"
    )
}
pub fn forced_exit_notice(language: UiLanguage) -> &'static str {
    i18n::local!(
        language,
        "強制終了: 会話の保存や所有プロセスの回収が未完了の可能性があります。再起動して中断した作業を確認してください。",
        "Forced exit: conversation saving and owned process cleanup may be incomplete. Restart to inspect interrupted work."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
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
