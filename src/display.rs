use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const RATATUI_COLORS: &[Color] = &[
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Magenta,
    Color::Blue,
    Color::White,
    Color::LightCyan,
    Color::LightYellow,
    Color::LightGreen,
];

pub fn ratatui_color(index: usize) -> Color {
    RATATUI_COLORS[index % RATATUI_COLORS.len()]
}

fn styled_log_line(name: &str, color: Color, name_width: usize, content: Span<'static>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:>name_width$}"), Style::new().fg(color)),
        Span::styled(" | ", Style::new().add_modifier(Modifier::DIM)),
        content,
    ])
}

pub fn styled_stdout_line(name: &str, color: Color, line: &str, name_width: usize) -> Line<'static> {
    styled_log_line(name, color, name_width, Span::raw(line.to_string()))
}

pub fn styled_stderr_line(name: &str, color: Color, line: &str, name_width: usize) -> Line<'static> {
    styled_log_line(name, color, name_width, Span::styled(line.to_string(), Style::new().fg(Color::Red)))
}

pub fn styled_exit_line(name: &str, color: Color, code: Option<i32>, name_width: usize) -> Line<'static> {
    let msg = match code {
        Some(c) => format!("exited with code {c}"),
        None => "exited with signal".into(),
    };
    styled_log_line(name, color, name_width, Span::styled(msg, Style::new().add_modifier(Modifier::BOLD)))
}

pub fn styled_restart_line(name: &str, color: Color, name_width: usize) -> Line<'static> {
    styled_log_line(name, color, name_width, Span::styled("restarting...", Style::new().add_modifier(Modifier::BOLD)))
}

pub fn styled_config_reload_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("--- config reloaded ---", Style::new().add_modifier(Modifier::BOLD)),
    ])
}

pub fn styled_config_error_line(msg: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("config error: {msg}"),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_stdout_line_has_colored_name_and_content() {
        let line = styled_stdout_line("meow", Color::Cyan, "hello kitty", 6);
        assert_eq!(line.spans.len(), 3);
        assert!(line.spans[0].content.contains("meow"));
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(line.spans[2].content, "hello kitty");
    }

    #[test]
    fn styled_stderr_line_has_red_content() {
        let line = styled_stderr_line("purr", Color::Yellow, "cat error", 6);
        assert_eq!(line.spans[2].style.fg, Some(Color::Red));
        assert_eq!(line.spans[2].content, "cat error");
    }

    #[test]
    fn styled_exit_line_shows_code() {
        let line = styled_exit_line("nyan", Color::Green, Some(0), 6);
        assert_eq!(line.spans[2].content, "exited with code 0");
    }

    #[test]
    fn styled_exit_line_shows_signal() {
        let line = styled_exit_line("nyan", Color::Green, None, 6);
        assert_eq!(line.spans[2].content, "exited with signal");
    }

    #[test]
    fn ratatui_color_wraps_around() {
        assert_eq!(ratatui_color(0), ratatui_color(RATATUI_COLORS.len()));
    }
}
