//! Process helpers: owned process groups, termination and liveness.

use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

/// Whether a process with this PID exists (`kill -0` through `sh`, no `unsafe`).
/// "Operation not permitted" (EPERM, read in the C locale) means it exists but
/// belongs to another user, so it counts as alive.
pub fn is_alive(pid: u32) -> bool {
    // `kill -0 0` signals the caller's own process group and always succeeds,
    // and a value above `i32::MAX` is not a PID: neither names a process.
    if pid == 0 || i32::try_from(pid).is_err() {
        return false;
    }
    Command::new("sh")
        .args(["-c", "kill -0 \"$1\"", "sh"])
        .arg(pid.to_string())
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map(|out| {
            out.status.success() || String::from_utf8_lossy(&out.stderr).contains("not permitted")
        })
        // When `sh` cannot be started nothing is known, and a lock must not be
        // taken from a process that may be alive.
        .unwrap_or(true)
}

/// Process groups of the live [`GroupChild`]ren, so a second signal or a panic
/// can stop everything they started.
static LIVE_GROUPS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn lock_groups() -> MutexGuard<'static, Vec<u32>> {
    LIVE_GROUPS.lock().unwrap_or_else(|e| e.into_inner())
}

fn register(pgid: u32) {
    lock_groups().push(pgid);
}

fn unregister(pgid: u32) {
    lock_groups().retain(|g| *g != pgid);
}

/// Sends `KILL` to every registered group. Waits for the registry lock.
pub fn kill_all_groups() {
    let groups = lock_groups().clone();
    for pgid in groups {
        signal_group(pgid, "KILL");
    }
}

/// Like [`kill_all_groups`] but skips the registry if it is busy, so it can
/// never deadlock (for the panic hook).
pub fn kill_all_groups_try() {
    let groups = match LIVE_GROUPS.try_lock() {
        Ok(guard) => guard.clone(),
        Err(TryLockError::Poisoned(e)) => e.into_inner().clone(),
        Err(TryLockError::WouldBlock) => return,
    };
    for pgid in groups {
        signal_group(pgid, "KILL");
    }
}

/// A child that leads its own process group and owns it: if it is dropped
/// without `terminate` or `finish_leftovers` (early return, panic), the whole
/// group is killed. The group id equals the child PID.
pub struct GroupChild {
    child: Child,
    pgid: u32,
    done: bool,
}

/// Spawns the command as the leader of a new process group and registers it.
pub fn spawn_in_group(cmd: &mut Command) -> io::Result<GroupChild> {
    let child = cmd.process_group(0).spawn()?;
    let pgid = child.id();
    register(pgid);
    Ok(GroupChild {
        child,
        pgid,
        done: false,
    })
}

impl GroupChild {
    pub fn id(&self) -> u32 {
        self.pgid
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// `TERM` to the group, wait up to `grace` for the leader to exit, then
    /// always `KILL` the group (members may outlive the leader) and reap the
    /// leader.
    pub fn terminate(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        signal_group(self.pgid, "TERM");
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // The group may already be gone; a failure here is expected and harmless.
        signal_group(self.pgid, "KILL");
        let status = self.child.wait();
        self.finish();
        status
    }

    /// After the leader exited and was reaped: stops what it left running.
    /// `TERM` to the group; if anything was there, wait up to `grace` for the
    /// group to disappear, then `KILL`.
    pub fn finish_leftovers(&mut self, grace: Duration) {
        if signal_group(self.pgid, "TERM") {
            let deadline = Instant::now() + grace;
            while group_exists(self.pgid) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            signal_group(self.pgid, "KILL");
        }
        self.finish();
    }

    fn finish(&mut self) {
        self.done = true;
        unregister(self.pgid);
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        if !self.done {
            signal_group(self.pgid, "KILL");
            let _ = self.child.wait();
        }
        unregister(self.pgid);
    }
}

/// Installs the handler for SIGINT, SIGTERM and SIGHUP (ctrlc `termination`
/// feature) and returns the shared cancel flag. The first signal sets the flag
/// and prints `notice` to stderr, if any; the second kills every live process
/// group and exits with 130. Calling it again returns the same flag.
pub fn install_signal_handler(notice: Option<&'static str>) -> anyhow::Result<Arc<AtomicBool>> {
    static CANCEL: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    if let Some(flag) = CANCEL.get() {
        return Ok(Arc::clone(flag));
    }
    let flag = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&flag);
    ctrlc::set_handler(move || {
        if handler_flag.swap(true, Ordering::SeqCst) {
            kill_all_groups();
            std::process::exit(130);
        }
        if let Some(text) = notice {
            // Never `println!`: a panic here would kill the handler thread.
            let _ = writeln!(io::stderr(), "{text}");
        }
    })
    .map_err(|e| anyhow::anyhow!("Cannot install the signal handler: {e}"))?;
    let _ = CANCEL.set(Arc::clone(&flag));
    Ok(flag)
}

/// Makes a panic kill the live process groups before the previous hook runs
/// (which, in the TUI, restores the terminal).
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        kill_all_groups_try();
        previous(info);
    }));
}

/// Notices that the process that started phasewise has ended.
///
/// When a terminal closes, the shell that started phasewise gets the hang-up,
/// but phasewise itself does not always get a signal (for example under
/// `sh -c`). It is then re-parented, and the TUI would keep polling a terminal
/// that only returns end-of-file. A changed parent id is treated like a
/// hang-up.
#[derive(Debug, Clone, Copy)]
pub struct ParentWatch {
    initial: u32,
}

impl ParentWatch {
    pub fn new() -> ParentWatch {
        ParentWatch {
            initial: std::os::unix::process::parent_id(),
        }
    }

    /// True once the parent at creation time is no longer the parent.
    pub fn parent_gone(&self) -> bool {
        std::os::unix::process::parent_id() != self.initial
    }
}

impl Default for ParentWatch {
    fn default() -> Self {
        ParentWatch::new()
    }
}

/// Signal 0 to a process group. `kill -0 -- -PGID` fails in dash (the `sh` of
/// Debian and Ubuntu: "Illegal number"), so the signal is given with `-s`.
const GROUP_PROBE: &str = "kill -s 0 -- \"-$1\"";

/// Whether any process of the group still exists.
fn group_exists(pgid: u32) -> bool {
    Command::new("sh")
        .args(["-c", GROUP_PROBE, "sh"])
        .arg(pgid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Sends `signal` (for example `TERM`) to every process of the group.
/// Returns whether `kill` reported success.
pub fn signal_group(pgid: u32, signal: &str) -> bool {
    Command::new("sh")
        .args(["-c", "kill -s \"$1\" -- \"-$2\"", "sh"])
        .arg(signal)
        .arg(pgid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use super::*;

    fn quiet(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    }

    #[test]
    fn the_parent_is_still_there() {
        let watch = ParentWatch::new();
        assert!(!watch.parent_gone());
        assert_eq!(watch.initial, std::os::unix::process::parent_id());
    }

    fn registered(pgid: u32) -> bool {
        lock_groups().contains(&pgid)
    }

    #[test]
    fn pid_zero_and_impossible_pids_are_not_alive() {
        assert!(!is_alive(0));
        assert!(!is_alive(u32::MAX));
        assert!(!is_alive(i32::MAX as u32 + 1));
    }

    #[test]
    fn a_process_of_another_user_counts_as_alive() {
        // PID 1 always exists; unless the tests run as root, `kill -0 1` fails
        // with EPERM, which must still mean "alive".
        assert!(is_alive(1));
    }

    #[test]
    fn terminate_kills_the_whole_tree() {
        let mut child = spawn_in_group(&mut quiet("sleep 30 & sleep 30 & wait")).expect("spawn");
        let pid = child.id();
        assert!(is_alive(pid));
        let started = Instant::now();
        let status = child.terminate(Duration::from_secs(5)).expect("terminate");
        assert!(!status.success());
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(!is_alive(pid));
        assert!(!group_exists(pid));
        assert!(!registered(pid));
    }

    #[test]
    fn the_group_probe_works_in_every_posix_shell() {
        let mut child = spawn_in_group(&mut quiet("sleep 30 & wait")).expect("spawn");
        let pgid = child.id().to_string();
        for shell in ["/bin/sh", "/bin/dash", "/bin/bash"] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            let status = Command::new(shell)
                .args(["-c", GROUP_PROBE, "sh", &pgid])
                .status()
                .expect("run shell");
            assert!(status.success(), "{shell} cannot probe a group");
        }
        child.terminate(Duration::from_secs(5)).expect("terminate");
    }

    #[test]
    fn drop_kills_the_tree_and_unregisters() {
        let child = spawn_in_group(&mut quiet("sleep 30 & sleep 30 & wait")).expect("spawn");
        let pgid = child.id();
        assert!(registered(pgid));
        assert!(group_exists(pgid));
        drop(child);
        // The killed processes may linger as zombies until init reaps them.
        let deadline = Instant::now() + Duration::from_secs(2);
        while group_exists(pgid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!group_exists(pgid));
        assert!(!registered(pgid));
    }

    #[test]
    fn finish_leftovers_kills_background_processes() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!; exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = spawn_in_group(&mut cmd).expect("spawn");
        let stdout = child.take_stdout().expect("stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("pid line");
        let sleeper: u32 = line.trim().parse().expect("pid");
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().expect("wait").is_none() {
            assert!(Instant::now() < deadline, "leader did not exit");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(is_alive(sleeper));
        child.finish_leftovers(Duration::from_millis(500));
        let deadline = Instant::now() + Duration::from_secs(2);
        while is_alive(sleeper) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!is_alive(sleeper));
        assert!(!registered(child.id()));
    }
}
