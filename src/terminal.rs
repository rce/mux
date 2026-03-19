use std::io::Stdout;

use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};

// ---------------------------------------------------------------------------
// Types carried over from the old module
// ---------------------------------------------------------------------------

pub struct ScriptView<'a> {
    pub name: &'a str,
    pub visible: bool,
    pub run_state: &'a RunState,
}

#[derive(Clone)]
pub enum RunState {
    Running,
    Exited(Option<i32>),
    Restarting,
}

// ---------------------------------------------------------------------------
// View-model structs consumed by render_ui
// ---------------------------------------------------------------------------

pub struct AppState<'a> {
    pub log_lines: &'a [Line<'static>],
    pub scripts: &'a [ScriptView<'a>],
    pub dialog: Option<DialogState<'a>>,
    pub scroll_offset: usize,
}

pub struct DialogState<'a> {
    pub title: &'a str,
    pub entries: &'a [(String, String)],
    pub help_text: &'a str,
}

// ---------------------------------------------------------------------------
// Terminal setup / teardown
// ---------------------------------------------------------------------------

pub fn setup_terminal() -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

// ---------------------------------------------------------------------------
// The single render function
// ---------------------------------------------------------------------------

pub fn render_ui(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());

    // -- Log area -----------------------------------------------------------
    render_log_area(frame, chunks[0], state);

    // -- Status bar ---------------------------------------------------------
    render_status_bar(frame, chunks[1], state.scripts);

    // -- Dialog overlay (if any) --------------------------------------------
    if let Some(ref dialog) = state.dialog {
        render_dialog(frame, frame.area(), dialog);
    }
}

// ---------------------------------------------------------------------------
// Log area
// ---------------------------------------------------------------------------

fn render_log_area(frame: &mut Frame, area: Rect, state: &AppState) {
    let total_lines = state.log_lines.len();
    let visible_height = area.height as usize;

    // Auto-scroll when scroll_offset would place us at or past the bottom
    let auto_scroll_offset = total_lines.saturating_sub(visible_height);
    let offset = if state.scroll_offset >= auto_scroll_offset {
        auto_scroll_offset
    } else {
        state.scroll_offset
    };

    let paragraph = Paragraph::new(state.log_lines.to_vec())
        .wrap(Wrap { trim: false })
        .scroll((offset.min(u16::MAX as usize) as u16, 0));
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn render_status_bar(frame: &mut Frame, area: Rect, scripts: &[ScriptView]) {
    let spans = build_status_spans(scripts);
    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

fn build_status_spans(scripts: &[ScriptView]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    for (i, s) in scripts.iter().enumerate() {
        let color = crate::display::ratatui_color(i);

        // Run-state suffix
        let state_str: String = match &s.run_state {
            RunState::Running => String::new(),
            RunState::Exited(code) => {
                let c = code.map_or("sig".into(), |c: i32| c.to_string());
                format!(" exit:{c}")
            }
            RunState::Restarting => " restarting".into(),
        };

        // Visibility label
        let (vis_label, vis_modifier) = if s.visible {
            ("ON", Modifier::BOLD)
        } else {
            ("off", Modifier::DIM)
        };

        // Build: " [N:name state STATUS] "
        let prefix = format!(" [{}:{}{} ", i + 1, s.name, state_str);
        let suffix = "] ";

        spans.push(Span::styled(prefix, Style::default().fg(color)));
        spans.push(Span::styled(
            vis_label.to_string(),
            Style::default().fg(color).add_modifier(vis_modifier),
        ));
        spans.push(Span::styled(
            suffix.to_string(),
            Style::default().fg(color),
        ));
    }

    spans
}

// ---------------------------------------------------------------------------
// Dialog overlay
// ---------------------------------------------------------------------------

fn render_dialog(frame: &mut Frame, area: Rect, dialog: &DialogState) {
    let max_detail = 50;

    let content_width = dialog
        .entries
        .iter()
        .map(|(label, detail)| {
            let detail_len = detail.len().min(max_detail + 3);
            5 + label.len() + 4 + detail_len
        })
        .max()
        .unwrap_or(20);

    let box_inner = content_width
        .max(dialog.help_text.len() + 2)
        .max(dialog.title.len() + 4)
        .max(20);

    // +2 for borders
    let width = (box_inner + 2).min(area.width as usize) as u16;
    // entries + blank line + help line + 2 borders
    let height = (dialog.entries.len() + 4).min(area.height as usize) as u16;

    let dialog_rect = centered_rect(width, height, area);

    // Clear the area behind the dialog
    frame.render_widget(Clear, dialog_rect);

    // Build dialog content lines
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (i, (label, detail)) in dialog.entries.iter().enumerate() {
        if i >= 9 {
            break;
        }
        let detail_display = if detail.len() > max_detail {
            format!("{}...", &detail[..max_detail])
        } else {
            detail.clone()
        };
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{})", i + 1),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {label}    {detail_display}")),
        ]);
        lines.push(line);
    }

    // Blank line
    lines.push(Line::raw(""));

    // Help text
    lines.push(Line::from(vec![
        Span::raw(format!("  {}", dialog.help_text)),
    ]));

    let block = Block::bordered().title(format!(" {} ", dialog.title));
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, dialog_rect);
}

/// Calculate a centered rectangle of the given size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

// ---------------------------------------------------------------------------
// Tests — the specification for the rendering layer
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal, buffer::Buffer, style::Color};

    // == Color palette ======================================================

    #[test]
    fn color_for_index_returns_cyan_for_first_cat() {
        assert_eq!(crate::display::ratatui_color(0), Color::Cyan);
    }

    #[test]
    fn color_for_index_wraps_around_palette() {
        let palette_len = crate::display::RATATUI_COLORS.len();
        assert_eq!(crate::display::ratatui_color(0), crate::display::ratatui_color(palette_len));
        assert_eq!(crate::display::ratatui_color(1), crate::display::ratatui_color(palette_len + 1));
    }

    #[test]
    fn color_palette_has_expected_entries() {
        assert_eq!(crate::display::RATATUI_COLORS.len(), 9);
    }

    // == Status bar spans ===================================================

    #[test]
    fn status_bar_shows_script_number_name_and_on_when_visible() {
        let run_state = RunState::Running;
        let scripts = vec![ScriptView {
            name: "whiskers",
            visible: true,
            run_state: &run_state,
        }];
        let spans = build_status_spans(&scripts);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("[1:whiskers "));
        assert!(text.contains("ON"));
    }

    #[test]
    fn status_bar_shows_off_when_script_hidden() {
        let run_state = RunState::Running;
        let scripts = vec![ScriptView {
            name: "paws",
            visible: false,
            run_state: &run_state,
        }];
        let spans = build_status_spans(&scripts);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("off"));
        assert!(!text.contains("ON"));
    }

    #[test]
    fn status_bar_shows_exit_code_for_exited_script() {
        let run_state = RunState::Exited(Some(42));
        let scripts = vec![ScriptView {
            name: "tabby",
            visible: true,
            run_state: &run_state,
        }];
        let spans = build_status_spans(&scripts);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("exit:42"));
    }

    #[test]
    fn status_bar_shows_exit_sig_when_no_exit_code() {
        let run_state = RunState::Exited(None);
        let scripts = vec![ScriptView {
            name: "nyan",
            visible: true,
            run_state: &run_state,
        }];
        let spans = build_status_spans(&scripts);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("exit:sig"));
    }

    #[test]
    fn status_bar_shows_restarting_state() {
        let run_state = RunState::Restarting;
        let scripts = vec![ScriptView {
            name: "mittens",
            visible: true,
            run_state: &run_state,
        }];
        let spans = build_status_spans(&scripts);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("restarting"));
    }

    #[test]
    fn status_bar_shows_multiple_scripts() {
        let r1 = RunState::Running;
        let r2 = RunState::Running;
        let scripts = vec![
            ScriptView { name: "meow", visible: true, run_state: &r1 },
            ScriptView { name: "purr", visible: false, run_state: &r2 },
        ];
        let spans = build_status_spans(&scripts);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("[1:meow"));
        assert!(text.contains("[2:purr"));
    }

    // == centered_rect ======================================================

    #[test]
    fn centered_rect_places_dialog_in_middle() {
        let area = Rect::new(0, 0, 80, 24);
        let result = centered_rect(40, 10, area);
        assert_eq!(result.x, 20);
        assert_eq!(result.y, 7);
        assert_eq!(result.width, 40);
        assert_eq!(result.height, 10);
    }

    #[test]
    fn centered_rect_clamps_to_area_when_too_large() {
        let area = Rect::new(0, 0, 20, 10);
        let result = centered_rect(40, 20, area);
        assert_eq!(result.width, 20);
        assert_eq!(result.height, 10);
    }

    // == render_ui integration via TestBackend ==============================

    #[test]
    fn render_ui_draws_log_lines_and_status_bar() {
        let backend = TestBackend::new(60, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        let log_lines = vec![
            Line::raw("hello from whiskers"),
            Line::raw("meow meow meow"),
        ];
        let run_state = RunState::Running;
        let scripts = vec![ScriptView {
            name: "whiskers",
            visible: true,
            run_state: &run_state,
        }];
        let state = AppState {
            log_lines: &log_lines,
            scripts: &scripts,
            dialog: None,
            scroll_offset: 0,
        };

        terminal
            .draw(|frame| render_ui(frame, &state))
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content = buffer_to_string(&buf);
        assert!(content.contains("hello from whiskers"));
        assert!(content.contains("meow meow meow"));
        // Status bar on the last row
        assert!(content.contains("[1:whiskers"));
        assert!(content.contains("ON"));
    }

    #[test]
    fn render_ui_auto_scrolls_when_many_log_lines() {
        let backend = TestBackend::new(40, 4); // 3 rows for log, 1 for status
        let mut terminal = Terminal::new(backend).unwrap();

        let log_lines: Vec<Line<'static>> = (0..20)
            .map(|i| Line::raw(format!("line {i}")))
            .collect();
        let run_state = RunState::Running;
        let scripts = vec![ScriptView {
            name: "cat",
            visible: true,
            run_state: &run_state,
        }];
        let state = AppState {
            log_lines: &log_lines,
            scripts: &scripts,
            dialog: None,
            scroll_offset: 999, // past the end -> auto-scroll
        };

        terminal
            .draw(|frame| render_ui(frame, &state))
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content = buffer_to_string(&buf);
        // Should show the last lines, not the first
        assert!(content.contains("line 19"));
        assert!(!content.contains("line 0"));
    }

    #[test]
    fn render_ui_draws_dialog_overlay() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        let entries = vec![
            ("kitty".to_string(), "http://meow.cat".to_string()),
            ("nyan".to_string(), "http://nyan.cat".to_string()),
        ];
        let run_state = RunState::Running;
        let scripts = vec![ScriptView {
            name: "paws",
            visible: true,
            run_state: &run_state,
        }];
        let state = AppState {
            log_lines: &[],
            scripts: &scripts,
            dialog: Some(DialogState {
                title: "Open URL",
                entries: &entries,
                help_text: "Press 1-9 to open",
            }),
            scroll_offset: 0,
        };

        terminal
            .draw(|frame| render_ui(frame, &state))
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content = buffer_to_string(&buf);
        assert!(content.contains("Open URL"));
        assert!(content.contains("kitty"));
        assert!(content.contains("nyan"));
        assert!(content.contains("Press 1-9"));
    }

    // -- Test helpers (at the bottom, as the spec prescribes) ---------------

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                s.push_str(cell.symbol());
            }
            s.push('\n');
        }
        s
    }
}
