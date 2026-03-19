mod config;
mod display;
mod process;
mod terminal;

use std::collections::VecDeque;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use process::{Event, OutputLine};
use ratatui::backend::CrosstermBackend;
use ratatui::text::Line;
use ratatui::Terminal;
use terminal::{AppState, DialogState, RunState, ScriptView};

const MAX_LOG_LINES: usize = 100_000;

struct ScriptState {
    name: String,
    cmd: String,
    visible: bool,
    run_state: RunState,
    color: ratatui::style::Color,
    stop: Arc<AtomicBool>,
    stopping: bool,
    generation: u64,
}

struct LogEntry {
    script_name: String,
    formatted: Line<'static>,
    always_visible: bool,
}

enum Dialog {
    Urls,
    Restart,
}

struct Mux {
    scripts: Vec<ScriptState>,
    urls: Vec<config::UrlConfig>,
    log: VecDeque<LogEntry>,
    name_width: usize,
    scroll_offset: usize,
    shutting_down: bool,
    exited_count: usize,
    dialog: Option<Dialog>,
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

        // While a dialog is open, handle selection or buffer non-keypress events
        if let Some(ref dialog) = self.dialog {
            match event {
                Event::Keypress(b @ b'1'..=b'9') if !self.shutting_down => {
                    let idx = (b - b'1') as usize;
                    match dialog {
                        Dialog::Urls => {
                            if idx < self.urls.len() {
                                open_url(&self.urls[idx].url);
                            }
                        }
                        Dialog::Restart => {
                            if idx < self.scripts.len() && !self.scripts[idx].stopping {
                                self.restart_script(idx);
                            }
                        }
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
                self.show_dialog(Dialog::Urls);
            }
            Event::Keypress(b'r') if !self.shutting_down && !self.scripts.is_empty() => {
                self.show_dialog(Dialog::Restart);
            }
            Event::Keypress(b'q') if !self.shutting_down => {
                self.shutting_down = true;
                process::trigger_shutdown();
            }
            Event::Keypress(b @ b'1'..=b'9') if !self.shutting_down => {
                let idx = (b - b'1') as usize;
                if idx < self.scripts.len() {
                    self.scripts[idx].visible = !self.scripts[idx].visible;
                }
            }
            Event::Output(OutputLine::Stdout {
                script_name: name,
                line,
            }) => {
                self.handle_output(name, line, false);
            }
            Event::Output(OutputLine::Stderr {
                script_name: name,
                line,
            }) => {
                self.handle_output(name, line, true);
            }
            Event::Output(OutputLine::Exited {
                script_name: name,
                code,
                generation,
            }) => {
                let idx = self.scripts.iter().position(|s| s.name == name);
                if let Some(idx) = idx {
                    let script = &mut self.scripts[idx];
                    let was_stopping = script.stopping;
                    if script.generation == generation {
                        script.run_state = RunState::Exited(code);
                    }
                    let styled = display::styled_exit_line(
                        &script.name,
                        script.color,
                        code,
                        self.name_width,
                    );
                    push_log(
                        &mut self.log,
                        LogEntry {
                            script_name: name.clone(),
                            formatted: styled,
                            always_visible: true,
                        },
                    );
                    if was_stopping {
                        self.scripts.remove(idx);
                    }
                }
                if self.shutting_down {
                    self.exited_count += 1;
                    if self.exited_count >= self.scripts.len() {
                        return LoopAction::Break;
                    }
                }
            }
            Event::Output(OutputLine::Restarting {
                script_name: name,
                generation,
            }) => {
                if let Some(script) = self
                    .scripts
                    .iter_mut()
                    .find(|s| s.name == name && s.generation == generation)
                {
                    script.run_state = RunState::Restarting;
                }
            }
            Event::Output(OutputLine::Restarted {
                script_name: name,
                generation,
            }) => {
                let styled = if let Some(script) = self
                    .scripts
                    .iter_mut()
                    .find(|s| s.name == name && s.generation == generation)
                {
                    script.run_state = RunState::Running;
                    display::styled_restart_line(&script.name, script.color, self.name_width)
                } else {
                    return LoopAction::Continue;
                };
                push_log(
                    &mut self.log,
                    LogEntry {
                        script_name: name,
                        formatted: styled,
                        always_visible: true,
                    },
                );
            }
            Event::ConfigReloaded(new_config) => {
                if self.shutting_down {
                    return LoopAction::Continue;
                }
                self.urls = new_config.urls.clone().unwrap_or_default();
                apply_config_reload(
                    &new_config,
                    &mut self.scripts,
                    &mut self.name_width,
                    &self.tx,
                    &self.work_dir,
                );
                let styled = display::styled_config_reload_line();
                push_log(
                    &mut self.log,
                    LogEntry {
                        script_name: String::new(),
                        formatted: styled,
                        always_visible: true,
                    },
                );
            }
            Event::ConfigError(msg) => {
                let styled = display::styled_config_error_line(&msg);
                push_log(
                    &mut self.log,
                    LogEntry {
                        script_name: String::new(),
                        formatted: styled,
                        always_visible: true,
                    },
                );
            }
            Event::Resize(_, _) => {
                // Terminal will redraw on the next draw() call
            }
            _ => {}
        }
        LoopAction::Continue
    }

    fn handle_output(&mut self, name: String, line: String, is_stderr: bool) {
        if let Some(script) = self.scripts.iter().find(|s| s.name == name) {
            let styled = if is_stderr {
                display::styled_stderr_line(&script.name, script.color, &line, self.name_width)
            } else {
                display::styled_stdout_line(&script.name, script.color, &line, self.name_width)
            };
            push_log(
                &mut self.log,
                LogEntry {
                    script_name: name,
                    formatted: styled,
                    always_visible: false,
                },
            );
        }
    }

    fn show_dialog(&mut self, kind: Dialog) {
        self.dialog = Some(kind);
    }

    fn dismiss_dialog(&mut self) {
        self.dialog = None;
        let buffered = std::mem::take(&mut self.buffered_events);
        for event in buffered {
            if matches!(self.handle_event(event), LoopAction::Break) {
                return;
            }
        }
    }

    fn restart_script(&mut self, idx: usize) {
        let script = &mut self.scripts[idx];
        // Signal the old supervisor to stop
        script.stop.store(true, Ordering::Relaxed);
        // Bump generation so we ignore stale events from old supervisor
        script.generation += 1;
        let script_gen = script.generation;
        // Create a fresh stop flag and spawn a new supervisor
        let new_stop = Arc::new(AtomicBool::new(false));
        script.stop = new_stop.clone();
        script.run_state = RunState::Restarting;
        script.stopping = false;
        let tx = self.tx.clone();
        let name = script.name.clone();
        let cmd = script.cmd.clone();
        let cwd = self.work_dir.clone();
        std::thread::spawn(move || process::supervise(name, cmd, tx, cwd, new_stop, script_gen));
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

    let name_width = config.scripts.iter().map(|s| s.name.len()).max().unwrap_or(1);

    let (tx, rx) = mpsc::channel::<Event>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let child_pids = Arc::new(Mutex::new(Vec::new()));

    process::init_globals(shutdown.clone(), child_pids.clone());

    // Setup ratatui terminal (replaces RawModeGuard)
    let mut terminal = terminal::setup_terminal().expect("failed to setup terminal");

    // Install panic hook to restore terminal
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        default_hook(info);
    }));

    let scripts: Vec<ScriptState> = config
        .scripts
        .iter()
        .enumerate()
        .map(|(i, cfg)| ScriptState {
            name: cfg.name.clone(),
            cmd: cfg.cmd.clone(),
            visible: true,
            run_state: RunState::Running,
            color: display::ratatui_color(i),
            stop: Arc::new(AtomicBool::new(false)),
            stopping: false,
            generation: 0,
        })
        .collect();

    let mut mux = Mux {
        urls: config.urls.clone().unwrap_or_default(),
        log: VecDeque::new(),
        name_width,
        scroll_offset: usize::MAX, // start at bottom (auto-scroll)
        shutting_down: false,
        exited_count: 0,
        dialog: None,
        buffered_events: Vec::new(),
        tx: tx.clone(),
        work_dir: work_dir.clone(),
        shutdown: shutdown.clone(),
        scripts,
    };

    // Spawn supervisors
    for script in &mux.scripts {
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

    // Initial draw
    draw(&mut terminal, &mux);

    // Event loop
    loop {
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => break,
        };
        let mut action = mux.handle_event(event);
        // Drain pending events before drawing (coalescing)
        while let Ok(event) = rx.try_recv() {
            if matches!(action, LoopAction::Break) {
                break;
            }
            action = mux.handle_event(event);
        }
        draw(&mut terminal, &mux);
        if matches!(action, LoopAction::Break) {
            break;
        }
    }

    // Cleanup
    terminal::restore_terminal(&mut terminal);
}

fn draw(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mux: &Mux) {
    let views = script_views(&mux.scripts);
    let dialog_entries: Vec<(String, String)>;
    let dialog_state = match &mux.dialog {
        Some(Dialog::Urls) => {
            dialog_entries = mux
                .urls
                .iter()
                .map(|u| (u.name.clone(), u.url.clone()))
                .collect();
            Some(DialogState {
                title: "Open URL",
                entries: &dialog_entries,
                help_text: "Press 1-9 to open, Esc to cancel",
            })
        }
        Some(Dialog::Restart) => {
            dialog_entries = mux
                .scripts
                .iter()
                .filter(|s| !s.stopping)
                .map(|s| {
                    let state = match &s.run_state {
                        RunState::Running => "running".into(),
                        RunState::Exited(code) => {
                            let c = code.map_or("signal".into(), |c| c.to_string());
                            format!("exited ({c})")
                        }
                        RunState::Restarting => "restarting".into(),
                    };
                    (s.name.clone(), state)
                })
                .collect();
            Some(DialogState {
                title: "Restart Script",
                entries: &dialog_entries,
                help_text: "Press 1-9 to restart, Esc to cancel",
            })
        }
        None => None,
    };

    // Get terminal height for windowing
    let term_height = terminal.size().map(|s| s.height as usize).unwrap_or(24);
    let log_area_height = term_height.saturating_sub(1); // 1 row for status bar

    // Count total visible for scroll calculation
    let total_visible = mux
        .log
        .iter()
        .filter(|e| {
            e.always_visible
                || mux
                    .scripts
                    .iter()
                    .find(|s| s.name == e.script_name)
                    .is_some_and(|s| s.visible)
        })
        .count();

    let scroll_start = if mux.scroll_offset >= total_visible.saturating_sub(log_area_height) {
        total_visible.saturating_sub(log_area_height)
    } else {
        mux.scroll_offset
    };

    // Build filtered log lines — only the visible window
    let visible_lines: Vec<Line<'static>> = mux
        .log
        .iter()
        .filter(|e| {
            e.always_visible
                || mux
                    .scripts
                    .iter()
                    .find(|s| s.name == e.script_name)
                    .is_some_and(|s| s.visible)
        })
        .skip(scroll_start)
        .take(log_area_height)
        .map(|e| e.formatted.clone())
        .collect();

    let state = AppState {
        log_lines: &visible_lines,
        scripts: &views,
        dialog: dialog_state,
        scroll_offset: 0, // already windowed, no offset needed
    };

    let _ = terminal.draw(|frame| terminal::render_ui(frame, &state));
}

fn apply_config_reload(
    new_config: &config::Config,
    scripts: &mut Vec<ScriptState>,
    name_width: &mut usize,
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
        let existing = scripts
            .iter()
            .find(|s| s.name == new_cfg.name && !s.stopping);
        if existing.is_some() {
            continue;
        }

        let old_visible = scripts
            .iter()
            .find(|s| s.name == new_cfg.name)
            .map(|s| s.visible);

        let stop = Arc::new(AtomicBool::new(false));
        let state = ScriptState {
            name: new_cfg.name.clone(),
            cmd: new_cfg.cmd.clone(),
            visible: old_visible.unwrap_or(true),
            run_state: RunState::Running,
            color: display::ratatui_color(color_idx),
            stop: stop.clone(),
            stopping: false,
            generation: 0,
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
            config_order
                .iter()
                .position(|n| n == &a.name)
                .unwrap_or(usize::MAX)
        };
        let b_pos = if b.stopping {
            usize::MAX
        } else {
            config_order
                .iter()
                .position(|n| n == &b.name)
                .unwrap_or(usize::MAX)
        };
        a_pos.cmp(&b_pos)
    });

    for (i, script) in scripts.iter_mut().enumerate() {
        script.color = display::ratatui_color(i);
    }

    *name_width = scripts.iter().map(|s| s.name.len()).max().unwrap_or(1);
}

fn spawn_supervisor(script: &ScriptState, tx: &mpsc::Sender<Event>, work_dir: &PathBuf) {
    let tx = tx.clone();
    let name = script.name.clone();
    let cmd = script.cmd.clone();
    let cwd = work_dir.clone();
    let stop = script.stop.clone();
    let script_gen = script.generation;
    std::thread::spawn(move || process::supervise(name, cmd, tx, cwd, stop, script_gen));
}

fn push_log(log: &mut VecDeque<LogEntry>, entry: LogEntry) {
    log.push_back(entry);
    while log.len() > MAX_LOG_LINES {
        log.pop_front();
    }
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
