pub const COLOR_PALETTE: &[&str] = &[
    "\x1b[36m", // cyan
    "\x1b[33m", // yellow
    "\x1b[32m", // green
    "\x1b[35m", // magenta
    "\x1b[34m", // blue
    "\x1b[37m", // white
    "\x1b[96m", // bright cyan
    "\x1b[93m", // bright yellow
    "\x1b[92m", // bright green
];
pub const RESET: &str = "\x1b[0m";
pub const RED: &str = "\x1b[31m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";

pub fn format_stdout_line(name: &str, color: &str, line: &str, name_width: usize) -> String {
    format!("{color}{name:>name_width$}{RESET} {DIM}|{RESET} {line}")
}

pub fn format_stderr_line(name: &str, color: &str, line: &str, name_width: usize) -> String {
    format!("{color}{name:>name_width$}{RESET} {DIM}|{RESET} {RED}{line}{RESET}")
}

pub fn format_exit_line(name: &str, color: &str, code: Option<i32>, name_width: usize) -> String {
    let msg = match code {
        Some(c) => format!("exited with code {c}"),
        None => "exited with signal".into(),
    };
    format!("{color}{name:>name_width$}{RESET} {DIM}|{RESET} {BOLD}{msg}{RESET}")
}

pub fn format_restart_line(name: &str, color: &str, name_width: usize) -> String {
    format!("{color}{name:>name_width$}{RESET} {DIM}|{RESET} {BOLD}restarting...{RESET}")
}

pub fn assign_color(index: usize) -> &'static str {
    COLOR_PALETTE[index % COLOR_PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_stdout_line_includes_colored_name_and_content() {
        let result = format_stdout_line("meow", "\x1b[36m", "hello kitty", 6);
        assert!(result.contains("\x1b[36m"));
        assert!(result.contains("meow"));
        assert!(result.contains("hello kitty"));
        assert!(result.ends_with("hello kitty"));
    }

    #[test]
    fn format_stderr_line_shows_content_in_red() {
        let result = format_stderr_line("purr", "\x1b[33m", "cat error", 6);
        assert!(result.contains(RED));
        assert!(result.contains("cat error"));
        assert!(result.ends_with(RESET));
    }

    #[test]
    fn format_exit_line_shows_exit_code() {
        let result = format_exit_line("nyan", "\x1b[32m", Some(0), 6);
        assert!(result.contains("exited with code 0"));
    }

    #[test]
    fn format_exit_line_shows_signal_when_no_code() {
        let result = format_exit_line("nyan", "\x1b[32m", None, 6);
        assert!(result.contains("exited with signal"));
    }

    #[test]
    fn format_restart_line_shows_restarting() {
        let result = format_restart_line("nyan", "\x1b[32m", 6);
        assert!(result.contains("restarting..."));
    }

    #[test]
    fn assign_color_wraps_around_palette() {
        let first = assign_color(0);
        let wrapped = assign_color(COLOR_PALETTE.len());
        assert_eq!(first, wrapped);
        assert_eq!(assign_color(1), assign_color(COLOR_PALETTE.len() + 1));
    }
}
