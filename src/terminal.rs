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

    pub fn draw(&self, scripts: &[ScriptView]) {
        let mut out = io::stdout().lock();

        // Move to start of line, clear it
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
        out.flush().ok();
    }

    pub fn clear(&self) {
        let mut out = io::stdout().lock();
        write!(out, "\r\x1b[2K").ok();
        out.flush().ok();
    }

    pub fn print_line(&self, line: &str) {
        let mut out = io::stdout().lock();
        // Clear status bar, print line, then we'll redraw status bar after
        write!(out, "\r\x1b[2K{line}\n").ok();
        out.flush().ok();
    }

    /// Clear the entire screen and replay visible lines from the buffer.
    pub fn replay(&self, lines: &[&str]) {
        let mut out = io::stdout().lock();
        // Clear screen and move cursor to top-left
        write!(out, "\x1b[2J\x1b[H").ok();
        for line in lines {
            write!(out, "{line}\n").ok();
        }
        out.flush().ok();
    }

    pub fn name_width(&self) -> usize {
        self.name_width
    }

    /// Draw a URL picker dialog as normal output lines above the status bar.
    /// The dialog uses box-drawing characters and shows numbered URLs.
    pub fn draw_url_dialog(&self, urls: &[crate::config::UrlConfig]) {
        let mut out = io::stdout().lock();

        // Calculate box width: fit the widest entry
        let max_url_display = 50;
        let content_width = urls
            .iter()
            .map(|u| {
                let url_display = if u.url.len() > max_url_display {
                    max_url_display + 2 // for ".."
                } else {
                    u.url.len()
                };
                // "  N) Name    url  " format
                4 + u.name.len() + 4 + url_display
            })
            .max()
            .unwrap_or(20);

        // Minimum width for the help text line
        let help_text = "  Press 1-9 to open, Esc to cancel";
        let box_inner = content_width.max(help_text.len()).max(20);

        // Clear status bar first
        write!(out, "\r\x1b[2K").ok();

        // Top border
        let title = " Open URL ";
        let remaining = box_inner.saturating_sub(title.len());
        let right_border = "─".repeat(remaining);
        write!(
            out,
            "\x1b[2K\u{250c}\u{2500}{title}{right_border}\u{2510}\n"
        )
        .ok();

        // URL entries
        for (i, u) in urls.iter().enumerate() {
            if i >= 9 {
                break; // max 9 URLs (keys 1-9)
            }
            let url_display = if u.url.len() > max_url_display {
                format!("{}...", &u.url[..max_url_display])
            } else {
                u.url.clone()
            };
            let entry = format!(
                "  {bold}{num}){reset} {name}    {url}",
                bold = display::BOLD,
                num = i + 1,
                reset = display::RESET,
                name = u.name,
                url = url_display,
            );
            // Pad to box width (account for ANSI codes not taking visual space)
            let visible_len = 5 + u.name.len() + 4 + if u.url.len() > max_url_display {
                max_url_display + 3
            } else {
                u.url.len()
            };
            let padding = box_inner.saturating_sub(visible_len);
            write!(
                out,
                "\x1b[2K\u{2502}{entry}{pad}\u{2502}\n",
                pad = " ".repeat(padding),
            )
            .ok();
        }

        // Empty line
        write!(
            out,
            "\x1b[2K\u{2502}{}\u{2502}\n",
            " ".repeat(box_inner),
        )
        .ok();

        // Help line
        let help_padding = box_inner.saturating_sub(help_text.len());
        write!(
            out,
            "\x1b[2K\u{2502}{help_text}{}\u{2502}\n",
            " ".repeat(help_padding),
        )
        .ok();

        // Bottom border
        write!(
            out,
            "\x1b[2K\u{2514}{}\u{2518}\n",
            "─".repeat(box_inner),
        )
        .ok();

        out.flush().ok();
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
