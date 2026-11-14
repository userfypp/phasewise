//! Retry, rollback, abort and crash recovery with the fake agents in `tests/fakes/`.

mod common;

use std::time::Duration;

use common::{TestRepo, isolated_git_env};
use serde_json::Value;
use tempfile::TempDir;

const FAKE_CLAUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh");
const FAKE_CODEX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-codex.sh");
const PLAN: &str = "+++\nverify = [\"! grep -q broken phase-*.txt\"]\n+++\n\n# Demo plan\n\n\
Shared context.\n\n## Phase 1: First\n\nDo one.\n\n## Phase 2: Second\n\nDo two.\n\n\
## Phase 3: Third\n\nDo three.\n";

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Env {
    repo: TestRepo,
    main_sha: String,
    logs: TempDir,
}

impl Env {
    fn new() -> Env {
        let repo = TestRepo::new();
        repo.write("phasewise.md", PLAN);
        repo.commit_all("add plan");
        let main_sha = repo.git_trim(&["rev-parse", "main"]);
        Env {
            repo,
            main_sha,
            logs: tempfile::tempdir().expect("create log dir"),
        }
    }

    fn exec(&self, args: &[&str], envs: &[(&str, &str)]) -> Out {
        let mut cmd = self.repo.phasewise();
        cmd.args(args)
            .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
            .env("PHASEWISE_CODEX_BIN", FAKE_CODEX)
            .env("FAKE_LOG_DIR", self.logs.path())
            .envs(envs.iter().copied())
            .timeout(Duration::from_secs(60));
        let output = cmd.output().expect("run phasewise");
        Out {
            code: output.status.code().expect("exit code"),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn ok(&self, args: &[&str], envs: &[(&str, &str)]) -> Out {
        let out = self.exec(args, envs);
        assert_eq!(out.code, 0, "{args:?}\n{}\n{}", out.stdout, out.stderr);
        out
    }

    fn state_path(&self) -> String {
        let id = self.repo.read(".phasewise/current");
        format!(".phasewise/runs/{}/state.json", id.trim())
    }

    fn state(&self) -> Value {
        serde_json::from_str(&self.repo.read(&self.state_path())).expect("state.json is JSON")
    }

    fn statuses(&self) -> Vec<String> {
        self.state()["phases"]
            .as_array()
            .expect("phases")
            .iter()
            .map(|p| p["status"].as_str().expect("status").to_string())
            .collect()
    }

    fn run_id(&self) -> String {
        self.state()["run_id"].as_str().expect("run_id").to_string()
    }

    fn checkpoint(&self, n: usize) -> String {
        self.state()["phases"][n - 1]["checkpoint_sha"]
            .as_str()
            .expect("checkpoint")
            .to_string()
    }

    fn calls(&self) -> Vec<String> {
        let log = std::fs::read_to_string(self.logs.path().join("calls.log")).unwrap_or_default();
        log.split("=== call ===")
            .skip(1)
            .map(String::from)
            .collect()
    }

    fn exists(&self, rel: &str) -> bool {
        self.repo.path().join(rel).exists()
    }

    fn check_invariants(&self) {
        assert_eq!(self.repo.git_trim(&["rev-parse", "main"]), self.main_sha);
        let names = self.repo.git(&["log", "--all", "--name-only", "--format="]);
        assert!(!names.contains(".phasewise"), "{names}");
    }
}

#[test]
fn retry_with_note_after_verify_failure_starts_a_clean_session() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("FAKE_MODE_1", "broken_once")]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    assert!(env.exists("leftover-1.txt"));
    assert_eq!(env.statuses()[0], "failed");
    let calls_before = env.calls().len();

    let out = env.ok(
        &["retry", "--note", "Keep the public API unchanged"],
        &[("FAKE_MODE_1", "broken_once")],
    );
    assert!(out.stdout.contains("ready for review"), "{}", out.stdout);
    assert_eq!(env.statuses(), ["awaiting_review", "pending", "pending"]);
    assert!(!env.exists("leftover-1.txt"));
    assert_eq!(env.repo.read("phase-1.txt"), "phase 1\n");
    assert!(env.repo.git(&["status", "--porcelain"]).trim().is_empty());
    let calls = env.calls();
    assert_eq!(calls.len(), calls_before + 1);
    let last = calls.last().expect("a call");
    assert!(last.contains("Keep the public API unchanged"), "{last}");
    assert!(!last.contains("--resume"), "{last}");
    let state = env.state();
    assert_eq!(state["phases"][0]["attempts"], 3);
    assert!(state["phases"][0]["failure"].is_null());
    env.check_invariants();
}

#[test]
fn retry_of_a_phase_awaiting_review_reruns_it_from_the_previous_checkpoint() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    env.ok(&["next"], &[]);
    assert_eq!(env.statuses(), ["done", "awaiting_review", "pending"]);
    let first = env.checkpoint(1);
    let before = env.calls().len();

    env.ok(&["retry"], &[]);
    assert_eq!(env.statuses(), ["done", "awaiting_review", "pending"]);
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD~1"]), first);
    let calls = env.calls();
    assert_eq!(calls.len(), before + 1);
    assert!(calls[before].contains("phase 2 of"), "{}", calls[before]);
    assert!(!calls[before].contains("--resume"));
    assert_eq!(env.state()["phases"][1]["attempts"], 2);
    env.check_invariants();
}

#[test]
fn retry_with_nothing_to_retry_exits_1() {
    let env = Env::new();
    env.ok(&["run", "--auto"], &[]);
    env.ok(&["next"], &[]);
    assert_eq!(env.state()["status"], "completed");
    let out = env.exec(&["retry"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("not active"), "{}", out.stderr);

    // An active run where every phase is pending or done has nothing to retry.
    let env = Env::new();
    env.ok(&["run"], &[]);
    let mut state = env.state();
    state["phases"][0]["status"] = "done".into();
    std::fs::write(
        env.repo.path().join(env.state_path()),
        serde_json::to_string(&state).expect("serialize"),
    )
    .expect("write state");
    let out = env.exec(&["retry"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("Nothing to retry"), "{}", out.stderr);
    env.check_invariants();
}

#[test]
fn retry_that_fails_again_exits_3() {
    let env = Env::new();
    let envs = [("FAKE_MODE", "never_fixes")];
    assert_eq!(env.exec(&["run"], &envs).code, 3);
    let out = env.exec(&["retry", "--note", "try harder"], &envs);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("phasewise retry"), "{}", out.stdout);
    assert_eq!(env.statuses()[0], "failed");
    env.check_invariants();
}

#[test]
fn rollback_returns_to_a_checkpoint_and_next_reruns_the_phase() {
    let env = Env::new();
    env.ok(&["run", "--auto"], &[]);
    assert_eq!(env.statuses(), ["done", "done", "awaiting_review"]);
    let first = env.checkpoint(1);
    let run_id = env.run_id();

    let out = env.ok(&["rollback", "1", "--yes"], &[]);
    assert!(out.stdout.contains("phasewise next"), "{}", out.stdout);
    assert!(out.stdout.contains("git reset --hard"), "{}", out.stdout);
    assert!(!env.exists("phase-2.txt") && !env.exists("phase-3.txt"));
    assert!(env.exists("phase-1.txt"));
    assert_eq!(env.statuses(), ["done", "pending", "pending"]);
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD"]), first);
    let state = env.state();
    assert!(state["phases"][1]["checkpoint_sha"].is_null());
    assert_eq!(state["phases"][1]["attempts"], 1);
    assert_eq!(
        env.repo.git_trim(&["branch", "--show-current"]),
        format!("phasewise/{run_id}")
    );

    env.ok(&["next"], &[]);
    assert_eq!(env.statuses(), ["done", "awaiting_review", "pending"]);
    assert!(env.exists("phase-2.txt"));
    assert_eq!(env.state()["phases"][1]["attempts"], 2);
    env.check_invariants();
}

#[test]
fn rollback_refuses_a_target_that_changed_after_confirmation() {
    let env = Env::new();
    env.ok(&["run", "--auto"], &[]);
    let head = env.repo.git_trim(&["rev-parse", "HEAD"]);
    let mut run = phasewise::ops::load_current(env.repo.path())
        .expect("load run")
        .expect("a run exists");
    run.git = run.git.with_env(isolated_git_env());
    // As if the checkpoint the user confirmed had been replaced meanwhile.
    let err = phasewise::ops::rollback(&mut run, 1, Some("0000000"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("changed while waiting for confirmation"),
        "{err}"
    );
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD"]), head);
    assert_eq!(env.statuses(), ["done", "done", "awaiting_review"]);
    assert!(env.exists("phase-3.txt"));
}

#[test]
fn rollback_to_zero_returns_to_the_base_commit() {
    let env = Env::new();
    env.ok(&["run", "--auto"], &[]);
    env.ok(&["rollback", "0", "--yes"], &[]);
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD"]), env.main_sha);
    assert_eq!(env.statuses(), ["pending", "pending", "pending"]);
    assert!(!env.exists("phase-1.txt"));
    env.check_invariants();
}

#[test]
fn rollback_to_a_phase_without_checkpoint_lists_valid_targets() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let out = env.exec(&["rollback", "5", "--yes"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("Valid targets: 0"), "{}", out.stderr);
    assert!(out.stderr.contains(", 1"), "{}", out.stderr);
    assert_eq!(env.statuses()[0], "awaiting_review");
    env.check_invariants();
}

#[test]
fn rollback_without_yes_and_without_a_terminal_changes_nothing() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let head = env.repo.git_trim(&["rev-parse", "HEAD"]);
    let out = env.exec(&["rollback", "0"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD"]), head);
    assert_eq!(env.statuses()[0], "awaiting_review");
    assert!(env.exists("phase-1.txt"));
    env.check_invariants();
}

#[test]
fn rollback_off_the_run_branch_is_refused() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    env.repo.git(&["switch", "main"]);
    let out = env.exec(&["rollback", "0", "--yes"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.contains("git switch phasewise/"),
        "{}",
        out.stderr
    );
    assert_eq!(env.statuses()[0], "awaiting_review");
    env.check_invariants();
}

#[test]
fn abort_with_a_clean_tree_returns_to_the_original_branch() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let run_id = env.run_id();
    let out = env.ok(&["abort"], &[]);
    assert!(
        out.stdout.contains(&format!("phasewise/{run_id}")),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("git branch -D"), "{}", out.stdout);
    assert!(out.stdout.contains("git merge --squash"), "{}", out.stdout);
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
    assert_eq!(env.state()["status"], "aborted");
    assert!(
        env.repo
            .git(&["branch", "--list", &format!("phasewise/{run_id}")])
            .contains(&run_id)
    );
    assert!(env.ok(&["status"], &[]).stdout.contains("aborted"));
    env.ok(&["run"], &[]);
    assert_ne!(env.run_id(), run_id);
    env.check_invariants();
}

#[test]
fn abort_after_a_failed_phase_keeps_the_leftovers_in_a_commit() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("FAKE_MODE", "never_fixes")]);
    assert_eq!(out.code, 3);
    let run_id = env.run_id();
    let out = env.ok(&["abort"], &[]);
    assert!(out.stdout.contains("Uncommitted changes"), "{}", out.stdout);
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
    assert!(env.repo.git(&["status", "--porcelain"]).trim().is_empty());
    let branch = format!("phasewise/{run_id}");
    let message = env.repo.git(&["log", "-1", "--format=%B", &branch]);
    assert!(
        message.contains("phasewise: abort — uncommitted changes"),
        "{message}"
    );
    assert!(
        message.contains(&format!("Phasewise-Run: {run_id}")),
        "{message}"
    );
    let files = env.repo.git(&["show", "--name-only", "--format=", &branch]);
    assert!(files.contains("phase-1.txt"), "{files}");
    assert_eq!(env.state()["status"], "aborted");
    env.check_invariants();
}

#[test]
fn abort_off_the_run_branch_does_not_switch() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    env.repo.git(&["switch", "-c", "elsewhere", "main"]);
    let out = env.ok(&["abort"], &[]);
    assert!(out.stdout.contains("elsewhere"), "{}", out.stdout);
    assert_eq!(
        env.repo.git_trim(&["branch", "--show-current"]),
        "elsewhere"
    );
    assert_eq!(env.state()["status"], "aborted");
    env.check_invariants();
}

#[test]
fn orphaned_running_phase_is_recovered_once_and_can_be_retried() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let mut state = env.state();
    state["phases"][0]["status"] = "running".into();
    std::fs::write(
        env.repo.path().join(env.state_path()),
        serde_json::to_string_pretty(&state).expect("serialize"),
    )
    .expect("write state");

    let out = env.ok(&["status"], &[]);
    assert!(
        out.stderr
            .contains("Phase 1 was left running by a previous phasewise process"),
        "{}",
        out.stderr
    );
    assert!(out.stdout.contains("interrupted"), "{}", out.stdout);
    assert_eq!(env.statuses()[0], "failed");
    assert_eq!(env.state()["phases"][0]["failure"]["kind"], "interrupted");
    // Already recovered: the notice does not repeat.
    assert!(!env.ok(&["status"], &[]).stderr.contains("left running"));

    env.ok(&["retry"], &[]);
    assert_eq!(env.statuses()[0], "awaiting_review");
    env.check_invariants();
}

#[test]
fn a_damaged_run_says_how_to_forget_it() {
    let env = Env::new();
    env.repo.write(".phasewise/current", "r1\n");
    env.repo
        .write(".phasewise/runs/r1/state.json", "{ truncated");
    for args in [&["status"][..], &["next"][..]] {
        let out = env.exec(args, &[]);
        assert_ne!(out.code, 0, "{args:?}: {}", out.stderr);
        assert!(
            out.stderr.contains("run r1 cannot be read")
                && out.stderr.contains("delete `.phasewise/current`"),
            "{args:?}: {}",
            out.stderr
        );
    }
}

#[test]
fn abort_explains_what_to_do_when_the_original_branch_is_gone() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    env.repo.git(&["branch", "-m", "main", "trunk"]);
    let out = env.exec(&["abort"], &[]);
    assert_ne!(out.code, 0, "{}", out.stdout);
    assert!(
        out.stderr.contains("Cannot go back to `main`") && out.stderr.contains("phasewise abort"),
        "{}",
        out.stderr
    );
    assert_eq!(env.state()["status"], "active");
    env.repo.git(&["switch", "trunk"]);
    env.ok(&["abort"], &[]);
    assert_eq!(env.state()["status"], "aborted");
}
