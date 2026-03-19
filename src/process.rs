use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::config;

/// Global shutdown flag, accessible from signal handlers.
static SHUTDOWN: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// Global child PIDs, accessible from signal handlers.
static CHILD_PIDS: OnceLock<Arc<Mutex<Vec<u32>>>> = OnceLock::new();

/// Initialize global shutdown state. Must be called once from main.
pub fn init_globals(shutdown: Arc<AtomicBool>, pids: Arc<Mutex<Vec<u32>>>) {
    SHUTDOWN.set(shutdown).ok();
    CHILD_PIDS.set(pids).ok();
}

/// Trigger a clean shutdown: set the flag and SIGINT all children.
/// Safe to call from signal handlers (uses try_lock to avoid deadlock).
pub fn trigger_shutdown() {
    if let Some(shutdown) = SHUTDOWN.get() {
        shutdown.store(true, Ordering::Relaxed);
    }
    if let Some(pids) = CHILD_PIDS.get() {
        // try_lock because we might be in a signal handler
        if let Ok(pids) = pids.try_lock() {
            for &pid in pids.iter() {
                unsafe {
                    // Negative PID signals the entire process group
                    libc::kill(-(pid as i32), libc::SIGINT);
                }
            }
        }
    }
}

pub fn shutdown_flag() -> Arc<AtomicBool> {
    SHUTDOWN.get().unwrap().clone()
}

pub fn child_pids() -> Arc<Mutex<Vec<u32>>> {
    CHILD_PIDS.get().unwrap().clone()
}

pub enum Event {
    Output(OutputLine),
    Keypress(u8),
    #[allow(dead_code)]
    Resize(u16, u16),
    ConfigReloaded(config::Config),
    ConfigError(String),
}

pub enum OutputLine {
    Stdout { script_name: String, line: String },
    Stderr { script_name: String, line: String },
    Exited { script_name: String, code: Option<i32>, generation: u64 },
    Restarting { script_name: String, generation: u64 },
    Restarted { script_name: String, generation: u64 },
}

/// Supervisor loop: spawns the command, reads output, waits for exit, restarts.
/// Checks both the per-script `stop` flag and the global shutdown flag.
/// When `stop` is set, SIGINTs the child, waits for exit, sends Exited, and returns.
pub fn supervise(
    name: String,
    cmd: String,
    tx: Sender<Event>,
    cwd: PathBuf,
    stop: Arc<AtomicBool>,
    generation: u64,
) {
    let shutdown = shutdown_flag();
    let pids = child_pids();

    loop {
        if shutdown.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return;
        }

        let child = Command::new("sh")
            .args(["-c", &cmd])
            .current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0) // own process group so we can signal the whole tree
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Event::Output(OutputLine::Stderr {
                    script_name: name.clone(),
                    line: format!("failed to spawn: {e}"),
                }));
                let _ = tx.send(Event::Output(OutputLine::Exited {
                    script_name: name.clone(),
                    code: None,
                    generation,
                }));
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let _ = tx.send(Event::Output(OutputLine::Restarting {
                    script_name: name.clone(),
                    generation,
                }));
                sleep_or_stop(Duration::from_secs(5), &shutdown, &stop);
                if shutdown.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                    return;
                }
                let _ = tx.send(Event::Output(OutputLine::Restarted {
                    script_name: name.clone(),
                    generation,
                }));
                continue;
            }
        };

        let pid = child.id();
        pids.lock().unwrap().push(pid);

        // If stop was set while we were spawning, kill the child immediately
        if stop.load(Ordering::Relaxed) {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGINT);
            }
            let _ = child.wait();
            pids.lock().unwrap().retain(|&p| p != pid);
            let _ = tx.send(Event::Output(OutputLine::Exited {
                script_name: name.clone(),
                code: None,
                generation,
            }));
            return;
        }

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let tx_out = tx.clone();
        let name_out = name.clone();
        let stdout_thread = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx_out
                            .send(Event::Output(OutputLine::Stdout {
                                script_name: name_out.clone(),
                                line: l,
                            }))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        });

        let tx_err = tx.clone();
        let name_err = name.clone();
        let stderr_thread = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx_err
                            .send(Event::Output(OutputLine::Stderr {
                                script_name: name_err.clone(),
                                line: l,
                            }))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        });

        // Spawn a watcher thread that SIGINTs the child when stop is set
        let stop_watcher = stop.clone();
        let shutdown_watcher = shutdown.clone();
        let watcher_pid = pid;
        let stop_thread = std::thread::spawn(move || {
            loop {
                if stop_watcher.load(Ordering::Relaxed) {
                    unsafe {
                        libc::kill(-(watcher_pid as i32), libc::SIGINT);
                    }
                    return;
                }
                // Don't need to signal on global shutdown - trigger_shutdown handles that
                if shutdown_watcher.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });

        let status = child.wait();
        stdout_thread.join().ok();
        stderr_thread.join().ok();
        stop_thread.join().ok();

        pids.lock().unwrap().retain(|&p| p != pid);

        let code = status.ok().and_then(|s| s.code());
        let _ = tx.send(Event::Output(OutputLine::Exited {
            script_name: name.clone(),
            code,
            generation,
        }));

        if shutdown.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return;
        }

        let _ = tx.send(Event::Output(OutputLine::Restarting {
            script_name: name.clone(),
            generation,
        }));
        sleep_or_stop(Duration::from_secs(5), &shutdown, &stop);

        if shutdown.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return;
        }

        let _ = tx.send(Event::Output(OutputLine::Restarted {
            script_name: name.clone(),
            generation,
        }));
    }
}

/// Watches the config file for changes and sends reload events.
/// This function blocks forever (keeps the watcher alive).
pub fn watch_config(path: PathBuf, tx: Sender<Event>) {
    use notify::{EventKind, RecursiveMode, Watcher};

    let (ntx, nrx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = ntx.send(event);
        }
    })
    .expect("failed to create file watcher");

    // Watch the parent directory so we catch atomic renames (editor save)
    let watch_path = path.parent().unwrap_or(&path);
    watcher
        .watch(watch_path, RecursiveMode::NonRecursive)
        .expect("failed to watch config file");

    let mut last_reload = std::time::Instant::now();
    for event in nrx {
        // Only react to events that touch our config file
        let touches_config = event.paths.iter().any(|p| p == &path);
        if !touches_config {
            continue;
        }

        if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
            if last_reload.elapsed() < Duration::from_millis(500) {
                continue; // debounce
            }
            last_reload = std::time::Instant::now();
            // Small delay for editors that write atomically (rename)
            std::thread::sleep(Duration::from_millis(50));
            match config::load(path.to_str().unwrap()) {
                Ok(cfg) => {
                    let _ = tx.send(Event::ConfigReloaded(cfg));
                }
                Err(e) => {
                    let _ = tx.send(Event::ConfigError(e));
                }
            }
        }
    }
}

/// Reads terminal events via crossterm, sending keypress and resize events.
pub fn read_stdin(tx: Sender<Event>) {
    loop {
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) => {
                // Only handle Press events (crossterm may send Release too)
                if key.kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }
                // Convert key events to our Event type
                match key.code {
                    crossterm::event::KeyCode::Char(c) => {
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                            && c == 'c'
                        {
                            // Ctrl+C: trigger shutdown and send a keypress so main loop sees it
                            trigger_shutdown();
                            // Send a sentinel so the main loop can detect ctrl-c
                            let _ = tx.send(Event::Keypress(0x03)); // ETX
                            return;
                        }
                        let _ = tx.send(Event::Keypress(c as u8));
                    }
                    crossterm::event::KeyCode::Esc => {
                        let _ = tx.send(Event::Keypress(0x1b));
                    }
                    _ => {}
                }
            }
            Ok(crossterm::event::Event::Resize(w, h)) => {
                let _ = tx.send(Event::Resize(w, h));
            }
            Ok(_) => {} // ignore mouse, focus, etc
            Err(_) => return,
        }
    }
}

fn sleep_or_stop(duration: Duration, shutdown: &AtomicBool, stop: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < duration {
        if shutdown.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(step);
        elapsed += step;
    }
}
