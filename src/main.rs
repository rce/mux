mod config;
mod display;
mod process;
mod terminal;

use process::{Event, OutputLine};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use terminal::{RunState, ScriptView, StatusBar};

struct ScriptState {
    name: String,
    visible: bool,
    run_state: RunState,
    color: &'static str,
}

fn main() {
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "mux.toml".into());

    let config = match config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mux: {e}");
            std::process::exit(1);
        }
    };

    let name_width = config.scripts.iter().map(|s| s.name.len()).max().unwrap();

    let (tx, rx) = mpsc::channel::<Event>();
    let shutdown = Arc::new(AtomicBool::new(false));

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

    // Spawn supervisor threads
    for (i, cfg) in config.scripts.iter().enumerate() {
        let tx = tx.clone();
        let cmd = cfg.cmd.clone();
        let shutdown = shutdown.clone();
        std::thread::spawn(move || process::supervise(i, cmd, tx, shutdown));
    }

    // Spawn stdin reader
    {
        let tx = tx.clone();
        std::thread::spawn(move || process::read_stdin(tx));
    }

    drop(tx);

    draw_status(&status_bar, &scripts);

    for event in &rx {
        match event {
            Event::Keypress(b'q') => {
                shutdown.store(true, Ordering::Relaxed);
                status_bar.clear();
                break;
            }
            Event::Keypress(b @ b'1'..=b'9') => {
                let idx = (b - b'1') as usize;
                if idx < scripts.len() {
                    scripts[idx].visible = !scripts[idx].visible;
                    draw_status(&status_bar, &scripts);
                }
            }
            Event::Output(OutputLine::Stdout {
                script_idx: idx,
                line,
            }) => {
                if scripts[idx].visible {
                    let formatted = display::format_stdout_line(
                        &scripts[idx].name,
                        scripts[idx].color,
                        &line,
                        status_bar.name_width(),
                    );
                    status_bar.clear();
                    status_bar.print_line(&formatted);
                    draw_status(&status_bar, &scripts);
                }
            }
            Event::Output(OutputLine::Stderr {
                script_idx: idx,
                line,
            }) => {
                if scripts[idx].visible {
                    let formatted = display::format_stderr_line(
                        &scripts[idx].name,
                        scripts[idx].color,
                        &line,
                        status_bar.name_width(),
                    );
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
                status_bar.clear();
                status_bar.print_line(&formatted);
                draw_status(&status_bar, &scripts);
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
                status_bar.clear();
                status_bar.print_line(&formatted);
                draw_status(&status_bar, &scripts);
            }
            _ => {}
        }
    }
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
