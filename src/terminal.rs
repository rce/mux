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
