mod config;
mod display;
mod process;
mod terminal;

use process::{Event, OutputLine};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use terminal::{RunState, ScriptView, StatusBar};

const MAX_LOG_LINES: usize = 100_000;

struct ScriptState {
    name: String,
    cmd: String,
    visible: bool,
    run_state: RunState,
    color: &'static str,
    stop: Arc<AtomicBool>,
    /// Script was removed from config; kept in list until it fully exits.
    stopping: bool,
}

struct LogEntry {
    script_name: String,
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
    let mut status_bar = StatusBar::new(name_width);

    let mut scripts: Vec<ScriptState> = config
        .scripts
        .iter()
        .enumerate()
        .map(|(i, cfg)| ScriptState {
            name: cfg.name.clone(),
            cmd: cfg.cmd.clone(),
            visible: true,
            run_state: RunState::Running,
            color: display::assign_color(i),
            stop: Arc::new(AtomicBool::new(false)),
            stopping: false,
        })
        .collect();

    let mut urls: Vec<config::UrlConfig> = config.urls.clone().unwrap_or_default();

    let mut log: Vec<LogEntry> = Vec::new();
    let mut shutting_down = false;
    let mut exited_count = 0usize;
    let mut url_dialog_open = false;

    // Spawn supervisor threads
    for script in &scripts {
        spawn_supervisor(script, &tx, &work_dir);
    }

    // Spawn stdin reader
    {
        let tx = tx.clone();
        std::thread::spawn(move || process::read_stdin(tx));
    }

    // Spawn config watcher
    {
        let tx = tx.clone();
        let path = config_path
            .canonicalize()
            .unwrap_or_else(|_| config_path.clone());
        std::thread::spawn(move || process::watch_config(path, tx));
    }

    // Keep tx alive for spawning new supervisors during config reload.
    // The main loop breaks explicitly on shutdown, so we don't need channel close.

    draw_status(&status_bar, &scripts);

    for event in &rx {
        // Detect Ctrl+C: shutdown was triggered externally
        if !shutting_down && shutdown.load(Ordering::Relaxed) {
            shutting_down = true;
        }

        match event {
            // --- URL dialog mode ---
            Event::Keypress(b @ b'1'..=b'9') if url_dialog_open && !shutting_down => {
                let idx = (b - b'1') as usize;
                if idx < urls.len() {
                    open_url(&urls[idx].url);
                }
                url_dialog_open = false;
                draw_status(&status_bar, &scripts);
            }
            Event::Keypress(_) if url_dialog_open => {
                // Any key (including Escape) dismisses the dialog
                url_dialog_open = false;
                draw_status(&status_bar, &scripts);
            }
            // --- Normal mode ---
            Event::Keypress(b'o') if !shutting_down && !urls.is_empty() => {
                url_dialog_open = true;
                status_bar.draw_url_dialog(&urls);
                draw_status(&status_bar, &scripts);
            }
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
                script_name: ref name,
                ref line,
            }) => {
                if let Some(script) = scripts.iter().find(|s| &s.name == name) {
                    let formatted = display::format_stdout_line(
                        &script.name,
                        script.color,
                        line,
                        status_bar.name_width(),
                    );
                    let visible = script.visible;
                    push_log(
                        &mut log,
                        LogEntry {
                            script_name: name.clone(),
                            formatted: formatted.clone(),
                            always_visible: false,
                        },
                    );
                    if visible {
                        status_bar.clear();
                        status_bar.print_line(&formatted);
                        draw_status(&status_bar, &scripts);
                    }
                }
            }
            Event::Output(OutputLine::Stderr {
                script_name: ref name,
                ref line,
            }) => {
                if let Some(script) = scripts.iter().find(|s| &s.name == name) {
                    let formatted = display::format_stderr_line(
                        &script.name,
                        script.color,
                        line,
                        status_bar.name_width(),
                    );
                    let visible = script.visible;
                    push_log(
                        &mut log,
                        LogEntry {
                            script_name: name.clone(),
                            formatted: formatted.clone(),
                            always_visible: false,
                        },
                    );
                    if visible {
                        status_bar.clear();
                        status_bar.print_line(&formatted);
                        draw_status(&status_bar, &scripts);
                    }
                }
            }
            Event::Output(OutputLine::Exited {
                script_name: ref name,
                code,
            }) => {
                let was_stopping = scripts.iter().find(|s| &s.name == name).is_some_and(|s| s.stopping);
                if let Some(script) = scripts.iter_mut().find(|s| &s.name == name) {
                    script.run_state = RunState::Exited(code);
                    let formatted = display::format_exit_line(
                        &script.name,
                        script.color,
                        code,
                        status_bar.name_width(),
                    );
                    push_log(
                        &mut log,
                        LogEntry {
                            script_name: name.clone(),
                            formatted: formatted.clone(),
                            always_visible: true,
                        },
                    );
                    status_bar.clear();
                    status_bar.print_line(&formatted);
                }
                // Remove scripts that were stopping (removed from config) now that they've exited
                if was_stopping {
                    scripts.retain(|s| !(&s.name == name && s.stopping));
                }
                draw_status(&status_bar, &scripts);
                if shutting_down {
                    exited_count += 1;
                    if exited_count >= scripts.len() {
                        status_bar.clear();
                        break;
                    }
                }
            }
            Event::Output(OutputLine::Restarting {
                script_name: ref name,
            }) => {
                if let Some(script) = scripts.iter_mut().find(|s| &s.name == name) {
                    script.run_state = RunState::Restarting;
                    draw_status(&status_bar, &scripts);
                }
            }
            Event::Output(OutputLine::Restarted {
                script_name: ref name,
            }) => {
                if let Some(script) = scripts.iter_mut().find(|s| &s.name == name) {
                    script.run_state = RunState::Running;
                    let formatted = display::format_restart_line(
                        &script.name,
                        script.color,
                        status_bar.name_width(),
                    );
                    push_log(
                        &mut log,
                        LogEntry {
                            script_name: name.clone(),
                            formatted: formatted.clone(),
                            always_visible: true,
                        },
                    );
                    status_bar.clear();
                    status_bar.print_line(&formatted);
                    draw_status(&status_bar, &scripts);
                }
            }
            Event::ConfigReloaded(new_config) => {
                if shutting_down {
                    continue;
                }
                urls = new_config.urls.clone().unwrap_or_default();
                apply_config_reload(
                    &new_config,
                    &mut scripts,
                    &mut status_bar,
                    &tx,
                    &work_dir,
                );
                draw_status(&status_bar, &scripts);
            }
            Event::ConfigError(ref msg) => {
                let err_msg = format!(
                    "{}{}config error: {}{}",
                    display::RED,
                    display::BOLD,
                    msg,
                    display::RESET,
                );
                status_bar.clear();
                status_bar.print_line(&err_msg);
                draw_status(&status_bar, &scripts);
            }
            _ => {}
        }
    }
}

/// Apply a config reload: stop removed/changed scripts, start new/changed ones.
/// Stopped scripts are kept in the list (marked `stopping`) until they fully exit.
fn apply_config_reload(
    new_config: &config::Config,
    scripts: &mut Vec<ScriptState>,
    status_bar: &mut StatusBar,
    tx: &mpsc::Sender<Event>,
    work_dir: &PathBuf,
) {
    let new_by_name: std::collections::HashMap<&str, &str> = new_config
        .scripts
        .iter()
        .map(|s| (s.name.as_str(), s.cmd.as_str()))
        .collect();

    // Mark removed and changed-cmd scripts as stopping
    for script in scripts.iter_mut() {
        if script.stopping {
            continue; // already draining from a previous reload
        }
        match new_by_name.get(script.name.as_str()) {
            None => {
                script.stop.store(true, Ordering::Relaxed);
                script.stopping = true;
            }
            Some(&new_cmd) if new_cmd != script.cmd => {
                script.stop.store(true, Ordering::Relaxed);
                script.stopping = true;
            }
            _ => {}
        }
    }

    // Add new scripts and respawn changed-cmd scripts
    let mut color_idx = scripts.iter().filter(|s| !s.stopping).count();
    for new_cfg in &new_config.scripts {
        let existing = scripts.iter().find(|s| s.name == new_cfg.name && !s.stopping);
        if existing.is_some() {
            continue; // unchanged, already running
        }

        // Preserve visibility from old entry (even if stopping)
        let old_visible = scripts.iter().find(|s| s.name == new_cfg.name).map(|s| s.visible);

        let stop = Arc::new(AtomicBool::new(false));
        let state = ScriptState {
            name: new_cfg.name.clone(),
            cmd: new_cfg.cmd.clone(),
            visible: old_visible.unwrap_or(true),
            run_state: RunState::Running,
            color: display::assign_color(color_idx),
            stop: stop.clone(),
            stopping: false,
        };
        spawn_supervisor(&state, tx, work_dir);
        scripts.push(state);
        color_idx += 1;
    }

    // Reorder: active scripts in config order, then stopping scripts at the end
    let config_order: Vec<String> = new_config.scripts.iter().map(|s| s.name.clone()).collect();
    scripts.sort_by(|a, b| {
        let a_pos = if a.stopping {
            usize::MAX
        } else {
            config_order.iter().position(|n| n == &a.name).unwrap_or(usize::MAX)
        };
        let b_pos = if b.stopping {
            usize::MAX
        } else {
            config_order.iter().position(|n| n == &b.name).unwrap_or(usize::MAX)
        };
        a_pos.cmp(&b_pos)
    });

    // Reassign colors based on new positions
    for (i, script) in scripts.iter_mut().enumerate() {
        script.color = display::assign_color(i);
    }

    // Update name_width for all scripts (including stopping ones, they still produce output)
    let new_name_width = scripts.iter().map(|s| s.name.len()).max().unwrap_or(1);
    status_bar.set_name_width(new_name_width);

    // Show reload confirmation
    let reload_msg = format!(
        "{}--- config reloaded ---{}",
        display::BOLD,
        display::RESET,
    );
    status_bar.clear();
    status_bar.print_line(&reload_msg);
}

fn spawn_supervisor(script: &ScriptState, tx: &mpsc::Sender<Event>, work_dir: &PathBuf) {
    let tx = tx.clone();
    let name = script.name.clone();
    let cmd = script.cmd.clone();
    let cwd = work_dir.clone();
    let stop = script.stop.clone();
    std::thread::spawn(move || process::supervise(name, cmd, tx, cwd, stop));
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
        .filter(|e| {
            e.always_visible
                || scripts
                    .iter()
                    .find(|s| s.name == e.script_name)
                    .is_some_and(|s| s.visible)
        })
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

/// Open a URL in the default browser. Uses `open` on macOS, `xdg-open` on Linux.
fn open_url(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // Spawn detached - we don't care about the result
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}
