//! Runs verification commands with a timeout and appends their output to the
//! verify log.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::process::spawn_in_group;

const TAIL_LINES: usize = 200;
const KILL_GRACE: Duration = Duration::from_secs(2);
const LEFTOVER_GRACE: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(20);

/// Progress of the verification, reported through the `on_event` callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyEvent {
    Started {
        command: String,
    },
    Finished {
        command: String,
        ok: bool,
        exit_code: Option<i32>,
        timed_out: bool,
    },
}

/// The command that made the verification fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCommand {
    pub command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub timeout_secs: u64,
    /// Last 200 lines of the command's output.
    pub output_tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    Passed,
    Failed(FailedCommand),
    Cancelled,
}

/// Runs the commands in order with `sh -c` in `root`, stopping at the first
/// failure, timeout or cancellation. Output goes to `log_path` (append).
pub fn run_all(
    commands: &[String],
    root: &Path,
    timeout: Duration,
    log_path: &Path,
    attempt: u32,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(VerifyEvent),
) -> Result<VerifyOutcome> {
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("Cannot open the verification log {}", log_path.display()))?;
    writeln!(log, "=== attempt {attempt} ===")?;
    if commands.is_empty() {
        writeln!(log, "(no verification commands)")?;
        return Ok(VerifyOutcome::Passed);
    }

    for command in commands {
        if cancel.load(Ordering::SeqCst) {
            return Ok(VerifyOutcome::Cancelled);
        }
        writeln!(log, "$ {command}")?;
        log.flush()?;
        let start = log.metadata()?.len();
        on_event(VerifyEvent::Started {
            command: command.clone(),
        });

        let mut cmd = Command::new("sh");
        cmd.args(["-c", command])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        let mut child = spawn_in_group(&mut cmd)
            .with_context(|| format!("Cannot start the verification command `{command}`"))?;

        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let mut cancelled = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
            } else if Instant::now() >= deadline {
                timed_out = true;
            }
            if cancelled || timed_out {
                break child.terminate(KILL_GRACE)?;
            }
            thread::sleep(POLL);
        };

        if !cancelled && !timed_out {
            // Do not let background processes outlive the command.
            child.finish_leftovers(LEFTOVER_GRACE);
        }
        let exit_code = status.code();
        end_the_line(&mut log, log_path, start)?;
        if cancelled {
            writeln!(log, "interrupted")?;
            on_event(VerifyEvent::Finished {
                command: command.clone(),
                ok: false,
                exit_code,
                timed_out: false,
            });
            return Ok(VerifyOutcome::Cancelled);
        }
        if timed_out {
            writeln!(log, "timed out after {} s", timeout.as_secs())?;
        } else {
            match exit_code {
                Some(code) => writeln!(log, "exit code: {code}")?,
                None => writeln!(log, "exit code: terminated by a signal")?,
            }
        }
        log.flush()?;
        let ok = status.success() && !timed_out;
        on_event(VerifyEvent::Finished {
            command: command.clone(),
            ok,
            exit_code,
            timed_out,
        });
        if !ok {
            let end = log.metadata()?.len();
            let output_tail = read_tail(log_path, start, end)?;
            return Ok(VerifyOutcome::Failed(FailedCommand {
                command: command.clone(),
                exit_code,
                timed_out,
                timeout_secs: timeout.as_secs(),
                output_tail,
            }));
        }
    }
    Ok(VerifyOutcome::Passed)
}

/// Starts a new line in the log when the command's output, which begins at
/// `start`, does not end with one, so that the trailer phasewise writes next is
/// never glued to it.
fn end_the_line(log: &mut File, log_path: &Path, start: u64) -> Result<()> {
    let end = log.metadata()?.len();
    if end > start {
        let mut file = File::open(log_path)?;
        file.seek(SeekFrom::Start(end - 1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            writeln!(log)?;
        }
    }
    Ok(())
}

/// Last `TAIL_LINES` lines of the log between two byte offsets. The trailer
/// line written by phasewise itself (`exit code: N` / `timed out after N s`)
/// is dropped, so only the command's own output remains.
fn read_tail(log_path: &Path, start: u64, end: u64) -> Result<String> {
    let mut file = File::open(log_path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(end.saturating_sub(start))
        .read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if lines
        .last()
        .is_some_and(|l| l.starts_with("exit code: ") || l.starts_with("timed out after "))
    {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(TAIL_LINES);
    Ok(lines[skip..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn run(
        list: &[&str],
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> (VerifyOutcome, Vec<VerifyEvent>, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("verify.log");
        let mut events = Vec::new();
        let outcome = run_all(
            &cmds(list),
            dir.path(),
            timeout,
            &log,
            1,
            cancel,
            &mut |e| events.push(e),
        )
        .expect("run_all");
        let text = std::fs::read_to_string(&log).expect("log");
        (outcome, events, text, dir)
    }

    #[test]
    fn passes_and_logs() {
        let cancel = AtomicBool::new(false);
        let (outcome, events, log, _dir) = run(
            &["echo hello", "echo oops >&2"],
            Duration::from_secs(10),
            &cancel,
        );
        assert_eq!(outcome, VerifyOutcome::Passed);
        assert_eq!(events.len(), 4);
        assert_eq!(
            log,
            "=== attempt 1 ===\n$ echo hello\nhello\nexit code: 0\n$ echo oops >&2\noops\nexit code: 0\n"
        );
    }

    #[test]
    fn output_without_a_final_newline_keeps_the_trailer_on_its_own_line() {
        let cancel = AtomicBool::new(false);
        let (outcome, _, log, _dir) = run(
            &["printf nonewline; exit 3", "printf done"],
            Duration::from_secs(10),
            &cancel,
        );
        match outcome {
            VerifyOutcome::Failed(f) => assert_eq!(f.output_tail, "nonewline"),
            other => panic!("unexpected outcome {other:?}"),
        }
        assert_eq!(
            log,
            "=== attempt 1 ===\n$ printf nonewline; exit 3\nnonewline\nexit code: 3\n"
        );

        let (outcome, _, log, _dir) = run(
            &["printf done", "echo", "true"],
            Duration::from_secs(10),
            &cancel,
        );
        assert_eq!(outcome, VerifyOutcome::Passed);
        assert_eq!(
            log,
            "=== attempt 1 ===\n$ printf done\ndone\nexit code: 0\n$ echo\n\nexit code: 0\n$ true\nexit code: 0\n"
        );
    }

    #[test]
    fn fails_with_exit_code_and_stops() {
        let cancel = AtomicBool::new(false);
        let (outcome, events, log, dir) = run(
            &["echo before; exit 3", "touch second-ran"],
            Duration::from_secs(10),
            &cancel,
        );
        match outcome {
            VerifyOutcome::Failed(f) => {
                assert_eq!(f.command, "echo before; exit 3");
                assert_eq!(f.exit_code, Some(3));
                assert!(!f.timed_out);
                assert_eq!(f.output_tail, "before");
            }
            other => panic!("unexpected outcome {other:?}"),
        }
        assert!(!dir.path().join("second-ran").exists());
        assert!(log.contains("exit code: 3"));
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1],
            VerifyEvent::Finished {
                ok: false,
                exit_code: Some(3),
                timed_out: false,
                ..
            }
        ));
    }

    #[test]
    fn times_out() {
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let (outcome, _events, log, _dir) = run(&["sleep 30"], Duration::from_secs(1), &cancel);
        assert!(started.elapsed() < Duration::from_secs(8));
        match outcome {
            VerifyOutcome::Failed(f) => {
                assert!(f.timed_out);
                assert_eq!(f.timeout_secs, 1);
            }
            other => panic!("unexpected outcome {other:?}"),
        }
        assert!(log.contains("timed out after 1 s"));
    }

    #[test]
    fn cancel_flag_interrupts() {
        let cancel = AtomicBool::new(true);
        let (outcome, events, _log, _dir) = run(&["sleep 30"], Duration::from_secs(10), &cancel);
        assert_eq!(outcome, VerifyOutcome::Cancelled);
        assert!(events.is_empty());

        // Cancelled while a command runs.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("verify.log");
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let outcome = thread::scope(|s| {
            s.spawn(|| {
                thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::SeqCst);
            });
            run_all(
                &cmds(&["sleep 30"]),
                dir.path(),
                Duration::from_secs(60),
                &log,
                2,
                &cancel,
                &mut |_| {},
            )
            .expect("run_all")
        });
        assert_eq!(outcome, VerifyOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(8));
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(text.contains("=== attempt 2 ===\n$ sleep 30\ninterrupted\n"));
    }

    #[test]
    fn output_tail_is_limited_to_200_lines() {
        let cancel = AtomicBool::new(false);
        let (outcome, _events, _log, _dir) = run(
            &["i=1; while [ $i -le 250 ]; do echo line$i; i=$((i+1)); done; exit 1"],
            Duration::from_secs(10),
            &cancel,
        );
        match outcome {
            VerifyOutcome::Failed(f) => {
                let lines: Vec<&str> = f.output_tail.lines().collect();
                assert_eq!(lines.len(), 200);
                assert_eq!(lines[0], "line51");
                assert_eq!(lines[199], "line250");
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn empty_list_passes() {
        let cancel = AtomicBool::new(false);
        let (outcome, events, log, _dir) = run(&[], Duration::from_secs(1), &cancel);
        assert_eq!(outcome, VerifyOutcome::Passed);
        assert!(events.is_empty());
        assert_eq!(log, "=== attempt 1 ===\n(no verification commands)\n");
    }
}
