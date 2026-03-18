use std::io::{self, Write};
use std::sync::OnceLock;

use crate::display;

static ORIGINAL_TERMIOS: OnceLock<libc::termios> = OnceLock::new();

pub struct RawModeGuard;

impl RawModeGuard {
    pub fn new() -> Self {
        let fd = stdin_fd();
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { libc::tcgetattr(fd, &mut original) };

        ORIGINAL_TERMIOS.get_or_init(|| original);

        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) };

        // Install panic hook that restores terminal
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            default_hook(info);
        }));

        // Install SIGINT handler so Ctrl+C restores terminal
        unsafe {
            libc::signal(libc::SIGINT, sigint_handler as libc::sighandler_t);
        }

        Self
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn stdin_fd() -> i32 {
    use std::os::unix::io::AsRawFd;
    io::stdin().as_raw_fd()
}

extern "C" fn sigint_handler(_: i32) {
    // Signal all children to exit but don't kill ourselves yet —
    // let the main loop drain remaining output before exiting.
    crate::process::trigger_shutdown();
}

fn restore_terminal() {
    if let Some(original) = ORIGINAL_TERMIOS.get() {
        unsafe { libc::tcsetattr(stdin_fd(), libc::TCSAFLUSH, original) };
    }
}

pub struct StatusBar {
    name_width: usize,
}

impl StatusBar {
    pub fn new(name_width: usize) -> Self {
        Self { name_width }
    }

    pub fn set_name_width(&mut self, name_width: usize) {
        self.name_width = name_width;
    }

    /// Write a line of output and redraw the status bar in a single flush.
    pub fn print_line_with_status(&self, line: &str, scripts: &[ScriptView]) {
        let mut out = io::stdout().lock();
        // Clear current line (status bar), print output line, then redraw status bar
        write!(out, "\r\x1b[2K{line}\n").ok();
        self.write_status(&mut out, scripts);
        out.flush().ok();
    }

    /// Draw only the status bar (no preceding output).
    pub fn draw(&self, scripts: &[ScriptView]) {
        let mut out = io::stdout().lock();
        self.write_status(&mut out, scripts);
        out.flush().ok();
    }

    /// Clear the status bar line.
    pub fn clear(&self) {
        let mut out = io::stdout().lock();
        write!(out, "\r\x1b[2K").ok();
        out.flush().ok();
    }

    /// Clear the entire screen and replay visible lines, then draw status bar.
    pub fn replay(&self, lines: &[&str], scripts: &[ScriptView]) {
        let mut out = io::stdout().lock();
        write!(out, "\x1b[2J\x1b[H").ok();
        for line in lines {
            write!(out, "{line}\n").ok();
        }
        self.write_status(&mut out, scripts);
        out.flush().ok();
    }

    /// Draw a picker dialog with a title, numbered entries, and help text.
    /// Each entry is a (label, detail) pair. Then draws the status bar.
    pub fn draw_dialog(
        &self,
        title: &str,
        entries: &[(String, String)],
        help_text: &str,
        scripts: &[ScriptView],
    ) {
        let mut out = io::stdout().lock();

        let max_detail = 50;
        let content_width = entries
            .iter()
            .map(|(label, detail)| {
                let detail_len = detail.len().min(max_detail + 3);
                5 + label.len() + 4 + detail_len
            })
            .max()
            .unwrap_or(20);

        let box_inner = content_width.max(help_text.len() + 2).max(20);

        write!(out, "\r\x1b[2K").ok();

        // Top border
        let title_str = format!(" {title} ");
        let remaining = box_inner.saturating_sub(title_str.len());
        write!(out, "┌─{title_str}{}┐\n", "─".repeat(remaining)).ok();

        // Entries
        for (i, (label, detail)) in entries.iter().enumerate() {
            if i >= 9 {
                break;
            }
            let detail_display = if detail.len() > max_detail {
                format!("{}...", &detail[..max_detail])
            } else {
                detail.clone()
            };
            let visible_len = 5 + label.len() + 4 + detail_display.len();
            let padding = box_inner.saturating_sub(visible_len);
            write!(
                out,
                "│  {bold}{num}){reset} {label}    {detail}{pad}│\n",
                bold = display::BOLD,
                num = i + 1,
                reset = display::RESET,
                detail = detail_display,
                pad = " ".repeat(padding),
            )
            .ok();
        }

        // Empty line + help text + bottom border
        write!(out, "│{}│\n", " ".repeat(box_inner)).ok();
        let help_padded = format!("  {help_text}");
        let help_padding = box_inner.saturating_sub(help_padded.len());
        write!(out, "│{help_padded}{}│\n", " ".repeat(help_padding)).ok();
        write!(out, "└{}┘\n", "─".repeat(box_inner)).ok();

        self.write_status(&mut out, scripts);
        out.flush().ok();
    }

    pub fn name_width(&self) -> usize {
        self.name_width
    }

    /// Write the status bar content to a writer (no flush).
    fn write_status(&self, out: &mut impl Write, scripts: &[ScriptView]) {
        write!(out, "\r\x1b[2K").ok();
        for (i, s) in scripts.iter().enumerate() {
            let color = display::assign_color(i);
            let status = if s.visible {
                format!("{}{}", display::BOLD, "ON")
            } else {
                format!("{}{}", display::DIM, "off")
            };
            let state = match &s.run_state {
                RunState::Running => String::new(),
                RunState::Exited(code) => {
                    let c = code.map_or("sig".into(), |c| c.to_string());
                    format!(" exit:{c}")
                }
                RunState::Restarting => " restarting".into(),
            };
            write!(
                out,
                " {color}[{}:{}{}]{}{} ",
                i + 1,
                s.name,
                state,
                status,
                display::RESET
            )
            .ok();
        }
    }
}

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
