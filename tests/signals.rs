//! Signals and closed stdout, with the fake agents in `tests/fakes/`.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{TestRepo, isolated_git_env};
use phasewise::process::is_alive;

const FAKE_CLAUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh");
const PLAN: &str = "# Demo\n\n## Phase 1: First\n\nDo one.\n";

fn phasewise_command(repo: &TestRepo) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("phasewise"));
    cmd.current_dir(repo.path())
        .envs(isolated_git_env())
        .env("PHASEWISE_CONFIG_DIR", common::NO_CONFIG_DIR)
        .env_remove("PHASEWISE_CODEX_BIN");
    cmd
}

/// Starts `phasewise run` with the stubborn fake agent and waits for the agent's PID file.
// The callers wait for the child.
#[allow(clippy::zombie_processes)]
fn start_stubborn_run(repo: &TestRepo, logs: &Path) -> (Child, u32) {
    repo.write("phasewise.md", PLAN);
    repo.commit_all("add plan");
    let child = phasewise_command(repo)
        .arg("run")
        .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
        .env("FAKE_LOG_DIR", logs)
        .env("FAKE_MODE", "stubborn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn phasewise");
    let pid_file = logs.join("agent.pid");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = text.trim().parse()
        {
            return (child, pid);
        }
        assert!(Instant::now() < deadline, "the fake agent did not start");
        thread::sleep(Duration::from_millis(50));
    }
}

fn send_term(pid: u32) {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success());
}

fn wait_exit(child: &mut Child, limit: Duration) -> i32 {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            return status.code().expect("exit code");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("phasewise did not exit within {limit:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_dead(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while is_alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    true
}

#[test]
fn one_sigterm_interrupts_the_phase() {
    let repo = TestRepo::new();
    let logs = tempfile::tempdir().expect("log dir");
    let (mut child, agent) = start_stubborn_run(&repo, logs.path());
    send_term(child.id());
    let code = wait_exit(&mut child, Duration::from_secs(30));
    assert_eq!(code, 3);
    assert!(
        wait_dead(agent, Duration::from_secs(2)),
        "agent still alive"
    );
    let id = repo.read(".phasewise/current");
    let state: serde_json::Value =
        serde_json::from_str(&repo.read(&format!(".phasewise/runs/{}/state.json", id.trim())))
            .expect("state is JSON");
    assert_eq!(state["phases"][0]["status"], "failed");
    assert_eq!(state["phases"][0]["failure"]["kind"], "interrupted");
}

#[test]
fn second_signal_kills_everything_and_exits_130() {
    let repo = TestRepo::new();
    let logs = tempfile::tempdir().expect("log dir");
    let (mut child, agent) = start_stubborn_run(&repo, logs.path());
    send_term(child.id());
    thread::sleep(Duration::from_millis(500));
    send_term(child.id());
    let code = wait_exit(&mut child, Duration::from_secs(3));
    assert_eq!(code, 130);
    assert!(
        wait_dead(agent, Duration::from_secs(2)),
        "agent still alive"
    );
}

#[test]
fn closed_stdout_does_not_panic() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    // A pipe whose read end is already closed: every write to stdout fails
    // with EPIPE (like `phasewise check | head` after `head` exits).
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let output = phasewise_command(&repo)
        .args(["check", "phasewise.md"])
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("run phasewise");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(output.status.code(), Some(0), "{stderr}");
}
