use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

pub enum Event {
    Output(OutputLine),
    Keypress(u8),
}

pub enum OutputLine {
    Stdout { script_idx: usize, line: String },
    Stderr { script_idx: usize, line: String },
    Exited { script_idx: usize, code: Option<i32> },
    Restarting { script_idx: usize },
    Restarted { script_idx: usize },
}

/// Supervisor loop: spawns the command, reads output, waits for exit, restarts.
pub fn supervise(idx: usize, cmd: String, tx: Sender<Event>, shutdown: Arc<AtomicBool>, cwd: PathBuf) {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }

        let child = Command::new("sh")
            .args(["-c", &cmd])
            .current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Event::Output(OutputLine::Stderr {
                    script_idx: idx,
                    line: format!("failed to spawn: {e}"),
                }));
                let _ = tx.send(Event::Output(OutputLine::Exited {
                    script_idx: idx,
                    code: None,
                }));
                let _ = tx.send(Event::Output(OutputLine::Restarting { script_idx: idx }));
                sleep_or_shutdown(Duration::from_secs(5), &shutdown);
                if !shutdown.load(Ordering::Relaxed) {
                    let _ = tx.send(Event::Output(OutputLine::Restarted { script_idx: idx }));
                }
                continue;
            }
        };

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let tx_out = tx.clone();
        let stdout_thread = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx_out
                            .send(Event::Output(OutputLine::Stdout {
                                script_idx: idx,
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
        let stderr_thread = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx_err
                            .send(Event::Output(OutputLine::Stderr {
                                script_idx: idx,
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

        let status = child.wait();
        stdout_thread.join().ok();
        stderr_thread.join().ok();

        let code = status.ok().and_then(|s| s.code());
        let _ = tx.send(Event::Output(OutputLine::Exited {
            script_idx: idx,
            code,
        }));

        if shutdown.load(Ordering::Relaxed) {
            return;
        }

        let _ = tx.send(Event::Output(OutputLine::Restarting { script_idx: idx }));
        sleep_or_shutdown(Duration::from_secs(5), &shutdown);

        if shutdown.load(Ordering::Relaxed) {
            return;
        }

        let _ = tx.send(Event::Output(OutputLine::Restarted { script_idx: idx }));
    }
}

/// Reads stdin one byte at a time, sending keypress events.
pub fn read_stdin(tx: Sender<Event>) {
    use std::io::Read;
    let stdin = std::io::stdin();
    let mut buf = [0u8; 1];
    loop {
        match stdin.lock().read(&mut buf) {
            Ok(1) => {
                if tx.send(Event::Keypress(buf[0])).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

fn sleep_or_shutdown(duration: Duration, shutdown: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < duration {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(step);
        elapsed += step;
    }
}
