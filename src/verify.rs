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
/// Most bytes of a check's output read back for the retry prompt.
const TAIL_WINDOW: u64 = 64 * 1024;
/// Most bytes one check may write to the log. A check that goes over it is
/// stopped, so that a noisy command (`yes`, a test in a loop) cannot fill the
/// disk during its timeout. The agent's logs use the same limit (see
/// `engine::run_agent_session`), but are cut instead of stopping the agent.
pub const OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;
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
    /// The check was stopped because its output went over the limit.
    pub too_much_output: bool,
    /// Last 200 lines of the command's output.
    pub output_tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    Passed,
    Failed(FailedCommand),
    Cancelled,
}

/// Message of a check stopped for writing too much, for the user and the agent.
pub fn output_limit_text() -> String {
    output_limit_text_for(OUTPUT_LIMIT)
}

fn output_limit_text_for(limit: u64) -> String {
    if limit >= 1024 * 1024 {
        format!("its output went over {} MiB", limit / (1024 * 1024))
    } else {
        format!("its output went over {limit} bytes")
    }
}

/// Runs the commands in order with `sh -c` in `root`, stopping at the first
/// failure, timeout, excess of output or cancellation. Output goes to
/// `log_path` (append).
pub fn run_all(
    commands: &[String],
    root: &Path,
    timeout: Duration,
    log_path: &Path,
    attempt: u32,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(VerifyEvent),
) -> Result<VerifyOutcome> {
    run_limited(
        commands,
        root,
        (timeout, OUTPUT_LIMIT),
        log_path,
        attempt,
        cancel,
        on_event,
    )
}

/// `run_all` with the (timeout, most output bytes per command) given.
fn run_limited(
    commands: &[String],
    root: &Path,
    (timeout, output_limit): (Duration, u64),
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
        let mut too_much = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
            } else if Instant::now() >= deadline {
                timed_out = true;
            } else if log.metadata()?.len().saturating_sub(start) > output_limit {
                too_much = true;
            }
            if cancelled || timed_out || too_much {
                break child.terminate(KILL_GRACE)?;
            }
            thread::sleep(POLL);
        };

        if !cancelled && !timed_out && !too_much {
            // Do not let background processes outlive the command.
            child.finish_leftovers(LEFTOVER_GRACE);
        }
        let exit_code = status.code();
        if too_much {
            // The command wrote on until it was stopped: keep what fits.
            log.set_len(start + output_limit)?;
        }
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
        } else if too_much {
            writeln!(log, "stopped: {}", output_limit_text_for(output_limit))?;
        } else {
            match exit_code {
                Some(code) => writeln!(log, "exit code: {code}")?,
                None => writeln!(log, "exit code: terminated by a signal")?,
            }
        }
        log.flush()?;
        let ok = status.success() && !timed_out && !too_much;
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
                too_much_output: too_much,
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
/// is dropped, so only the command's own output remains. At most the last
/// `TAIL_WINDOW` bytes of the slice are read, however much the check printed.
fn read_tail(log_path: &Path, start: u64, end: u64) -> Result<String> {
    let from = start.max(end.saturating_sub(TAIL_WINDOW));
    let mut file = File::open(log_path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.take(end.saturating_sub(from))
        .read_to_end(&mut bytes)?;
    // Colours and progress bars are for a terminal, not for the agent.
    let text = clean_output(&String::from_utf8_lossy(&bytes));
    // The window may begin in the middle of a line (or of a character): drop
    // that partial line, unless nothing else is left (one very long line).
    let mut body = text.as_str();
    if from > start
        && let Some(i) = body.find('\n')
        && !tail_lines(&body[i + 1..]).is_empty()
    {
        body = &body[i + 1..];
    }
    Ok(tail_lines(body).join("\n"))
}

/// Output of a check as plain text lines: escape sequences (colours, cursor
/// moves, titles) are removed and a carriage return that is not part of a
/// line ending starts a new line, so that a progress bar redrawn hundreds of
/// times counts as the lines it is and not as one.
fn clean_output(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters, then a final byte between @ and ~.
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or the string terminator `ESC \`.
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                // Any other two-character sequence.
                _ => {}
            },
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => out.push('\n'),
            c => out.push(c),
        }
    }
    out
}

/// The last `TAIL_LINES` lines of `text` without phasewise's own trailer line.
fn tail_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.last().is_some_and(|l| {
        l.starts_with("exit code: ")
            || l.starts_with("timed out after ")
            || l.starts_with("stopped: ")
    }) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(TAIL_LINES);
    lines.split_off(skip)
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

    fn tail_of(content: &[u8], start: usize) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("verify.log");
        std::fs::write(&log, content).expect("write");
        read_tail(&log, start as u64, content.len() as u64).expect("read_tail")
    }

    #[test]
    fn a_check_that_writes_without_end_is_stopped_at_the_output_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("verify.log");
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let outcome = run_limited(
            &cmds(&["yes 'a line of noise'", "echo never"]),
            dir.path(),
            (Duration::from_secs(60), 4096),
            &log,
            1,
            &cancel,
            &mut |e| events.push(e),
        )
        .expect("run");
        let VerifyOutcome::Failed(failed) = outcome else {
            panic!("expected a failure, got {outcome:?}");
        };
        assert!(failed.too_much_output && !failed.timed_out);
        assert!(
            !failed.output_tail.contains("stopped: "),
            "{}",
            failed.output_tail
        );
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(
            text.contains("stopped: its output went over 4096 bytes"),
            "{text}"
        );
        assert!(!text.contains("never"), "the next check must not run");
        // The command's own output was cut to the limit.
        assert!(text.len() < 4096 + 200, "{} bytes", text.len());
        assert_eq!(events.len(), 2, "{events:?}");
    }

    #[test]
    fn tail_drops_colours_and_splits_progress_bars_into_lines() {
        let out = b"\x1b[31mred\x1b[0m text\n\x1b]0;title\x07a\rb\rc\r\nd\nexit code: 1\n";
        assert_eq!(tail_of(out, 0), "red text\na\nb\nc\nd");
    }

    #[test]
    fn a_progress_bar_does_not_hide_the_failure_behind_the_line_limit() {
        // One "line" of 500 redraws, then the real error: the error stays and
        // the bar counts as the many lines it is.
        let mut out = Vec::new();
        for i in 0..500 {
            out.extend_from_slice(format!("progress {i}\r").as_bytes());
        }
        out.extend_from_slice(b"\nerror: boom\nexit code: 1\n");
        let tail = tail_of(&out, 0);
        assert!(tail.ends_with("error: boom"), "{tail}");
        assert!(
            !tail.contains('\r') && tail.lines().count() <= TAIL_LINES,
            "{tail}"
        );
    }

    #[test]
    fn tail_of_small_output_is_unchanged() {
        assert_eq!(tail_of(b"one\ntwo\nexit code: 1\n", 0), "one\ntwo");
    }

    #[test]
    fn tail_of_large_output_is_the_last_lines() {
        let mut content = vec![b'x'; 1024 * 1024];
        content.push(b'\n');
        let mut n = 0;
        while content.len() < 5 * 1024 * 1024 {
            content.extend_from_slice(format!("line {n}\n").as_bytes());
            n += 1;
        }
        content.extend_from_slice(b"exit code: 1\n");
        let expected: Vec<String> = (n - TAIL_LINES..n).map(|i| format!("line {i}")).collect();
        assert_eq!(tail_of(&content, 0), expected.join("\n"));
    }

    #[test]
    fn tail_reads_only_a_bounded_window() {
        // The garbage before the window is never read: it would not decode.
        let mut content = vec![0xffu8; 2 * 1024 * 1024];
        content.extend_from_slice(b"\nlast\nexit code: 0\n");
        assert_eq!(tail_of(&content, 0), "last");
    }

    #[test]
    fn tail_of_one_long_line_is_bounded() {
        let mut content = vec![b'a'; 1024 * 1024];
        content.extend_from_slice(b"\nexit code: 0\n");
        // The window holds the end of the line and the trailer: the end of
        // the line is kept, bounded, rather than nothing at all.
        let out = tail_of(&content, 0);
        assert_eq!(
            out.len() as u64,
            TAIL_WINDOW - "\nexit code: 0\n".len() as u64
        );
        assert!(out.bytes().all(|b| b == b'a'));
        // Without a trailing newline the window itself is returned, bounded.
        let out = tail_of(&vec![b'a'; 1024 * 1024], 0);
        assert_eq!(out.len() as u64, TAIL_WINDOW);
    }

    #[test]
    fn tail_cut_inside_a_multibyte_character() {
        let mut content = "é".repeat(100_000).into_bytes();
        content.extend_from_slice(b"\nend\nexit code: 0\n");
        assert_eq!(tail_of(&content, 0), "end");
        let out = tail_of(&"é".repeat(100_000).into_bytes(), 0);
        assert!(out.len() as u64 <= 2 * TAIL_WINDOW);
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
