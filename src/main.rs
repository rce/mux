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
    stopping: bool,
}

struct LogEntry {
    script_name: String,
    formatted: String,
    always_visible: bool,
}

struct Mux {
    scripts: Vec<ScriptState>,
    urls: Vec<config::UrlConfig>,
    log: Vec<LogEntry>,
    status_bar: StatusBar,
    shutting_down: bool,
    exited_count: usize,
    url_dialog_open: bool,
    buffered_events: Vec<Event>,
    tx: mpsc::Sender<Event>,
    work_dir: PathBuf,
    shutdown: Arc<AtomicBool>,
}

enum LoopAction {
    Continue,
    Break,
}

impl Mux {
    /// Handle a single event. Returns Break if the event loop should exit.
    fn handle_event(&mut self, event: Event) -> LoopAction {
        // Detect Ctrl+C
        if !self.shutting_down && self.shutdown.load(Ordering::Relaxed) {
            self.shutting_down = true;
        }

        // While URL dialog is open, buffer non-keypress events
        if self.url_dialog_open {
            match event {
                Event::Keypress(b @ b'1'..=b'9') if !self.shutting_down => {
                    let idx = (b - b'1') as usize;
                    if idx < self.urls.len() {
                        open_url(&self.urls[idx].url);
                    }
                    self.dismiss_dialog();
                }
                Event::Keypress(_) => {
                    self.dismiss_dialog();
                }
                other => {
                    self.buffered_events.push(other);
                }
            }
            return LoopAction::Continue;
        }

        match event {
            Event::Keypress(b'o') if !self.shutting_down && !self.urls.is_empty() => {
                self.url_dialog_open = true;
                self.status_bar
                    .draw_url_dialog(&self.urls, &script_views(&self.scripts));
            }
            Event::Keypress(b'q') if !self.shutting_down => {
                self.shutting_down = true;
                process::trigger_shutdown();
            }
            Event::Keypress(b @ b'1'..=b'9') if !self.shutting_down => {
                let idx = (b - b'1') as usize;
                if idx < self.scripts.len() {
                    self.scripts[idx].visible = !self.scripts[idx].visible;
                    replay_visible(&self.status_bar, &self.log, &self.scripts);
                }
            }
            Event::Output(OutputLine::Stdout {
                script_name: name,
                line,
            }) => {
                if let Some(script) = self.scripts.iter().find(|s| s.name == name) {
                    let formatted = display::format_stdout_line(
                        &script.name,
                        script.color,
                        &line,
                        self.status_bar.name_width(),
                    );
                    let visible = script.visible;
                    push_log(
                        &mut self.log,
                        LogEntry {
                            script_name: name,
                            formatted: formatted.clone(),
                            always_visible: false,
                        },
                    );
                    if visible {
                        self.status_bar.print_line_with_status(
                            &formatted,
                            &script_views(&self.scripts),
                        );
                    }
                }
            }
            Event::Output(OutputLine::Stderr {
                script_name: name,
                line,
            }) => {
                if let Some(script) = self.scripts.iter().find(|s| s.name == name) {
                    let formatted = display::format_stderr_line(
                        &script.name,
                        script.color,
                        &line,
                        self.status_bar.name_width(),
                    );
                    let visible = script.visible;
                    push_log(
                        &mut self.log,
                        LogEntry {
                            script_name: name,
                            formatted: formatted.clone(),
                            always_visible: false,
                        },
                    );
                    if visible {
                        self.status_bar.print_line_with_status(
                            &formatted,
                            &script_views(&self.scripts),
                        );
                    }
                }
            }
            Event::Output(OutputLine::Exited {
                script_name: name,
                code,
            }) => {
                let was_stopping = self
                    .scripts
                    .iter()
                    .find(|s| s.name == name)
                    .is_some_and(|s| s.stopping);
                let mut formatted = String::new();
                if let Some(script) = self.scripts.iter_mut().find(|s| s.name == name) {
                    script.run_state = RunState::Exited(code);
                    formatted = display::format_exit_line(
                        &script.name,
                        script.color,
                        code,
                        self.status_bar.name_width(),
                    );
                    push_log(
                        &mut self.log,
                        LogEntry {
                            script_name: name.clone(),
                            formatted: formatted.clone(),
                            always_visible: true,
                        },
                    );
                }
                if was_stopping {
                    self.scripts.retain(|s| !(s.name == name && s.stopping));
                }
                if !formatted.is_empty() {
                    self.status_bar
                        .print_line_with_status(&formatted, &script_views(&self.scripts));
                } else {
                    self.status_bar.draw(&script_views(&self.scripts));
                }
                if self.shutting_down {
                    self.exited_count += 1;
                    if self.exited_count >= self.scripts.len() {
                        self.status_bar.clear();
                        return LoopAction::Break;
                    }
                }
            }
            Event::Output(OutputLine::Restarting { script_name: name }) => {
                if let Some(script) = self.scripts.iter_mut().find(|s| s.name == name) {
                    script.run_state = RunState::Restarting;
                }
                self.status_bar.draw(&script_views(&self.scripts));
            }
            Event::Output(OutputLine::Restarted { script_name: name }) => {
                let formatted = if let Some(script) =
                    self.scripts.iter_mut().find(|s| s.name == name)
                {
                    script.run_state = RunState::Running;
                    display::format_restart_line(
                        &script.name,
                        script.color,
                        self.status_bar.name_width(),
                    )
                } else {
                    return LoopAction::Continue;
                };
                push_log(
                    &mut self.log,
                    LogEntry {
                        script_name: name,
                        formatted: formatted.clone(),
                        always_visible: true,
                    },
                );
                self.status_bar
                    .print_line_with_status(&formatted, &script_views(&self.scripts));
            }
            Event::ConfigReloaded(new_config) => {
                if self.shutting_down {
                    return LoopAction::Continue;
                }
                self.urls = new_config.urls.clone().unwrap_or_default();
                apply_config_reload(
                    &new_config,
                    &mut self.scripts,
                    &mut self.status_bar,
                    &self.tx,
                    &self.work_dir,
                );
                self.status_bar.draw(&script_views(&self.scripts));
            }
            Event::ConfigError(msg) => {
                let err_msg = format!(
                    "{}{}config error: {}{}",
                    display::RED,
                    display::BOLD,
                    msg,
                    display::RESET,
                );
                self.status_bar
                    .print_line_with_status(&err_msg, &script_views(&self.scripts));
            }
            _ => {}
        }
        LoopAction::Continue
    }

    fn dismiss_dialog(&mut self) {
        self.url_dialog_open = false;
        // Process buffered events
        let buffered = std::mem::take(&mut self.buffered_events);
        for event in buffered {
            if matches!(self.handle_event(event), LoopAction::Break) {
                return;
            }
        }
        self.status_bar.draw(&script_views(&self.scripts));
    }
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

    process::init_globals(shutdown.clone(), child_pids.clone());

    let _guard = terminal::RawModeGuard::new();

    let scripts: Vec<ScriptState> = config
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

    let mut mux = Mux {
        urls: config.urls.clone().unwrap_or_default(),
        log: Vec::new(),
        status_bar: StatusBar::new(name_width),
        shutting_down: false,
        exited_count: 0,
        url_dialog_open: false,
        buffered_events: Vec::new(),
        tx: tx.clone(),
        work_dir: work_dir.clone(),
        shutdown: shutdown.clone(),
        scripts,
    };

    for script in &mux.scripts {
        spawn_supervisor(script, &tx, &work_dir);
    }

    {
        let tx = tx.clone();
        std::thread::spawn(move || process::read_stdin(tx));
    }

    {
        let tx = tx.clone();
        let path = config_path
            .canonicalize()
            .unwrap_or_else(|_| config_path.clone());
        std::thread::spawn(move || process::watch_config(path, tx));
    }

    mux.status_bar.draw(&script_views(&mux.scripts));

    for event in &rx {
        if matches!(mux.handle_event(event), LoopAction::Break) {
            break;
        }
    }
}

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

    for script in scripts.iter_mut() {
        if script.stopping {
            continue;
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

    let mut color_idx = scripts.iter().filter(|s| !s.stopping).count();
    for new_cfg in &new_config.scripts {
        let existing = scripts.iter().find(|s| s.name == new_cfg.name && !s.stopping);
        if existing.is_some() {
            continue;
        }

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

    for (i, script) in scripts.iter_mut().enumerate() {
        script.color = display::assign_color(i);
    }

    let new_name_width = scripts.iter().map(|s| s.name.len()).max().unwrap_or(1);
    status_bar.set_name_width(new_name_width);

    let reload_msg = format!(
        "{}--- config reloaded ---{}",
        display::BOLD,
        display::RESET,
    );
    status_bar.print_line_with_status(&reload_msg, &script_views(scripts));
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
    bar.replay(&visible_lines, &script_views(scripts));
}

fn script_views(scripts: &[ScriptState]) -> Vec<ScriptView<'_>> {
    scripts
        .iter()
        .map(|s| ScriptView {
            name: &s.name,
            visible: s.visible,
            run_state: &s.run_state,
        })
        .collect()
}

fn open_url(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}
