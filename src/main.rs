mod config;
mod display;
mod process;
mod terminal;

use process::{Event, OutputLine};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use terminal::{RunState, ScriptView, StatusBar};

const MAX_LOG_LINES: usize = 100_000;

struct ScriptState {
    name: String,
    visible: bool,
    run_state: RunState,
    color: &'static str,
}

struct LogEntry {
    script_idx: usize,
    formatted: String,
    /// Always shown regardless of visibility (exit/restart messages)
    always_visible: bool,
}

fn main() {
    let config_path = PathBuf::from(
        std::env::args().nth(1).unwrap_or_else(|| "mux.toml".into()),
    );

    let config = match config::load(config_path.to_str().unwrap_or("mux.toml")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mux: {e}");
            std::process::exit(1);
        }
    };

    // Run commands relative to the config file's directory
    let work_dir = config_path
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|e| {
            eprintln!("mux: failed to resolve config directory: {e}");
            std::process::exit(1);
        });

    let name_width = config.scripts.iter().map(|s| s.name.len()).max().unwrap();

    let (tx, rx) = mpsc::channel::<Event>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let child_pids = Arc::new(Mutex::new(Vec::new()));

    // Register globals so signal handler can access them
    process::init_globals(shutdown.clone(), child_pids.clone());

    let _guard = terminal::RawModeGuard::new();
    let status_bar = StatusBar::new(name_width);

    let mut scripts: Vec<ScriptState> = config
        .scripts
        .iter()
        .enumerate()
        .map(|(i, cfg)| ScriptState {
            name: cfg.name.clone(),
            visible: true,
            run_state: RunState::Running,
            color: display::assign_color(i),
        })
        .collect();

    let mut log: Vec<LogEntry> = Vec::new();
    let mut shutting_down = false;
    let mut exited_count = 0usize;

    // Spawn supervisor threads
    for (i, cfg) in config.scripts.iter().enumerate() {
        let tx = tx.clone();
        let cmd = cfg.cmd.clone();
        let cwd = work_dir.clone();
        std::thread::spawn(move || process::supervise(i, cmd, tx, cwd));
    }

    // Spawn stdin reader
    {
        let tx = tx.clone();
        std::thread::spawn(move || process::read_stdin(tx));
    }

    drop(tx);

    draw_status(&status_bar, &scripts);

    for event in &rx {
        // Detect Ctrl+C: shutdown was triggered externally
        if !shutting_down && shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            shutting_down = true;
        }

        match event {
            Event::Keypress(b'q') if !shutting_down => {
                shutting_down = true;
                process::trigger_shutdown();
            }
            Event::Keypress(b @ b'1'..=b'9') if !shutting_down => {
                let idx = (b - b'1') as usize;
                if idx < scripts.len() {
                    scripts[idx].visible = !scripts[idx].visible;
                    replay_visible(&status_bar, &log, &scripts);
                    draw_status(&status_bar, &scripts);
                }
            }
            Event::Output(OutputLine::Stdout {
                script_idx: idx,
                line,
            }) => {
                let formatted = display::format_stdout_line(
                    &scripts[idx].name,
                    scripts[idx].color,
                    &line,
                    status_bar.name_width(),
                );
                push_log(&mut log, LogEntry { script_idx: idx, formatted: formatted.clone(), always_visible: false });
                if scripts[idx].visible {
                    status_bar.clear();
                    status_bar.print_line(&formatted);
                    draw_status(&status_bar, &scripts);
                }
            }
            Event::Output(OutputLine::Stderr {
                script_idx: idx,
                line,
            }) => {
                let formatted = display::format_stderr_line(
                    &scripts[idx].name,
                    scripts[idx].color,
                    &line,
                    status_bar.name_width(),
                );
                push_log(&mut log, LogEntry { script_idx: idx, formatted: formatted.clone(), always_visible: false });
                if scripts[idx].visible {
                    status_bar.clear();
                    status_bar.print_line(&formatted);
                    draw_status(&status_bar, &scripts);
                }
            }
            Event::Output(OutputLine::Exited {
                script_idx: idx,
                code,
            }) => {
                scripts[idx].run_state = RunState::Exited(code);
                let formatted = display::format_exit_line(
                    &scripts[idx].name,
                    scripts[idx].color,
                    code,
                    status_bar.name_width(),
                );
                push_log(&mut log, LogEntry { script_idx: idx, formatted: formatted.clone(), always_visible: true });
                status_bar.clear();
                status_bar.print_line(&formatted);
                draw_status(&status_bar, &scripts);
                if shutting_down {
                    exited_count += 1;
                    if exited_count >= scripts.len() {
                        status_bar.clear();
                        break;
                    }
                }
            }
            Event::Output(OutputLine::Restarting { script_idx: idx }) => {
                scripts[idx].run_state = RunState::Restarting;
                draw_status(&status_bar, &scripts);
            }
            Event::Output(OutputLine::Restarted { script_idx: idx }) => {
                scripts[idx].run_state = RunState::Running;
                let formatted = display::format_restart_line(
                    &scripts[idx].name,
                    scripts[idx].color,
                    status_bar.name_width(),
                );
                push_log(&mut log, LogEntry { script_idx: idx, formatted: formatted.clone(), always_visible: true });
                status_bar.clear();
                status_bar.print_line(&formatted);
                draw_status(&status_bar, &scripts);
            }
            _ => {}
        }
    }
}

fn push_log(log: &mut Vec<LogEntry>, entry: LogEntry) {
    log.push(entry);
    if log.len() > MAX_LOG_LINES {
        let drain = log.len() - MAX_LOG_LINES;
        log.drain(..drain);
    }
}

fn replay_visible(bar: &StatusBar, log: &[LogEntry], scripts: &[ScriptState]) {
    let visible_lines: Vec<&str> = log
        .iter()
        .filter(|e| e.always_visible || scripts[e.script_idx].visible)
        .map(|e| e.formatted.as_str())
        .collect();
    bar.replay(&visible_lines);
}

fn draw_status(bar: &StatusBar, scripts: &[ScriptState]) {
    let views: Vec<ScriptView> = scripts
        .iter()
        .map(|s| ScriptView {
            name: &s.name,
            visible: s.visible,
            run_state: &s.run_state,
        })
        .collect();
    bar.draw(&views);
}
