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
fn report_summarizes_the_run_on_stdout_and_in_a_file() {
    let env = Env::new();
    assert_eq!(env.exec(&["report"], &[]).code, 1, "no run yet");
    env.ok(&["run"], &[]);
    env.ok(&["next"], &[]);

    let out = env.ok(&["report"], &[]);
    let id = env.run_id();
    let text = &out.stdout;
    assert!(text.starts_with("# Demo plan\n\n"), "{text}");
    assert!(
        text.contains(&format!(
            "Run `{id}` on branch `phasewise/{id}` (from `main`)"
        )),
        "{text}"
    );
    assert!(text.contains("| # | Phase | Status | Time | Cost | Files |"));
    assert!(text.contains("| 1 | First | done |"), "{text}");
    assert!(text.contains("| 2 | Second | awaiting review |"), "{text}");
    assert!(
        text.contains("| 3 | Third | pending | - | - | - |"),
        "{text}"
    );
    assert!(text.contains("- `! grep -q broken phase-*.txt`"), "{text}");
    assert!(text.contains("1 of 3 phases done"), "{text}");

    let written = env.ok(&["report", "--output", "report.md"], &[]);
    assert!(written.stdout.contains("Report written to report.md"));
    assert_eq!(env.repo.read("report.md"), *text);

    let again = env.exec(&["report", "--output", "report.md"], &[]);
    assert_eq!(again.code, 1, "{}", again.stdout);
    assert!(again.stderr.contains("already exists"), "{}", again.stderr);
    env.ok(&["report", "--output", "report.md", "--force"], &[]);
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

/// A finished run plus a commit made by hand on the run branch.
fn env_with_a_hand_made_commit() -> (Env, String, String) {
    let env = Env::new();
    env.ok(&["run", "--auto"], &[]);
    env.repo.commit_file("mine.txt", "mine\n", "my own work");
    let head = env.repo.git_trim(&["rev-parse", "HEAD"]);
    let log = env.repo.git_trim(&["log", "--format=%H"]);
    (env, head, log)
}

#[test]
fn retry_refuses_to_drop_a_commit_made_by_hand_on_the_run_branch() {
    let (env, head, log) = env_with_a_hand_made_commit();
    let out = env.exec(&["retry"], &[]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("my own work"), "{}", out.stderr);
    assert!(
        out.stderr.contains("git branch keep-my-work"),
        "{}",
        out.stderr
    );
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD"]), head);
    assert_eq!(env.repo.git_trim(&["log", "--format=%H"]), log);
    assert!(env.exists("mine.txt"));
    assert_eq!(env.statuses(), ["done", "done", "awaiting_review"]);

    env.repo.git(&["branch", "keep"]);
    env.ok(&["retry"], &[]);
    assert!(!env.exists("mine.txt"));
    assert_eq!(env.repo.git_trim(&["rev-parse", "keep"]), head);
}

#[test]
fn rollback_refuses_to_drop_a_commit_made_by_hand_on_the_run_branch() {
    let (env, head, log) = env_with_a_hand_made_commit();
    let out = env.exec(&["rollback", "1", "--yes"], &[]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("my own work"), "{}", out.stderr);
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD"]), head);
    assert_eq!(env.repo.git_trim(&["log", "--format=%H"]), log);
    assert!(env.exists("mine.txt") && env.exists("phase-3.txt"));
    assert_eq!(env.statuses(), ["done", "done", "awaiting_review"]);

    env.repo.git(&["branch", "keep"]);
    env.ok(&["rollback", "1", "--yes"], &[]);
    assert_eq!(env.statuses(), ["done", "pending", "pending"]);
    assert!(!env.exists("mine.txt"));
    assert_eq!(env.repo.git_trim(&["rev-parse", "keep"]), head);
}

#[test]
fn rollback_does_not_report_phases_that_were_already_pending() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    assert_eq!(env.statuses(), ["awaiting_review", "pending", "pending"]);
    let out = env.ok(&["rollback", "1", "--yes"], &[]);
    assert!(!out.stdout.contains("pending again"), "{}", out.stdout);
    assert!(out.stdout.contains("phasewise next"), "{}", out.stdout);
}

#[test]
fn rollback_counts_only_the_phases_it_reset() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    env.ok(&["next"], &[]);
    assert_eq!(env.statuses(), ["done", "awaiting_review", "pending"]);
    let out = env.ok(&["rollback", "1", "--yes"], &[]);
    assert!(
        out.stdout.contains("1 later phase(s) are pending again."),
        "{}",
        out.stdout
    );
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
fn abort_completes_when_the_only_dirty_path_cannot_be_staged() {
    let mut env = Env::new();
    // Changes inside a submodule show up in `git status` but `git add` cannot
    // stage them from the parent repository.
    let sub = TestRepo::new();
    let sub_path = sub.path().to_str().expect("utf-8 path").to_string();
    env.repo.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        &sub_path,
        "sub",
    ]);
    env.repo.commit_all("add a submodule");
    env.main_sha = env.repo.git_trim(&["rev-parse", "main"]);
    env.ok(&["run"], &[]);
    let run_id = env.run_id();
    env.repo
        .write("sub/scratch.txt", "not committed in the submodule\n");
    assert!(
        env.repo.git(&["status", "--porcelain"]).contains("sub"),
        "the submodule must look dirty"
    );

    let out = env.ok(&["abort"], &[]);
    assert!(
        !out.stdout.contains("Uncommitted changes"),
        "{}",
        out.stdout
    );
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
    assert_eq!(env.state()["status"], "aborted");
    let branch = format!("phasewise/{run_id}");
    let message = env.repo.git(&["log", "-1", "--format=%B", &branch]);
    assert!(!message.contains("uncommitted changes"), "{message}");
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

/// A repository whose run is completed; HEAD is on the run branch.
fn completed_env() -> Env {
    let env = Env::new();
    env.ok(&["run", "--auto"], &[]);
    env.ok(&["next"], &[]);
    assert_eq!(env.state()["status"], "completed");
    env
}

#[test]
fn leaving_a_finished_run_goes_back_to_the_original_branch_and_keeps_the_run_branch() {
    let env = completed_env();
    let run_branch = env.repo.git_trim(&["branch", "--show-current"]);
    assert!(run_branch.starts_with("phasewise/"), "{run_branch}");
    phasewise::ops::leave_finished_run(env.repo.path()).expect("leave");
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
    assert!(
        env.repo
            .git(&["branch", "--list", &run_branch])
            .contains(&run_branch)
    );
    // Off the run branch it does nothing.
    phasewise::ops::leave_finished_run(env.repo.path()).expect("noop");
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
}

#[test]
fn leaving_an_active_run_does_not_switch_branches() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let run_branch = env.repo.git_trim(&["branch", "--show-current"]);
    phasewise::ops::leave_finished_run(env.repo.path()).expect("noop");
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), run_branch);
}

#[test]
fn merge_commits_the_squashed_run_on_the_original_branch_and_keeps_the_run_branch() {
    let env = completed_env();
    let run_id = env.run_id();
    let run_branch = format!("phasewise/{run_id}");
    let out = env.ok(&["merge", "--yes"], &[]);
    assert!(out.stdout.contains("Committed"), "{}", out.stdout);
    assert!(out.stdout.contains("on main"), "{}", out.stdout);
    assert!(out.stdout.contains("is kept"), "{}", out.stdout);
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD~1"]), env.main_sha);
    let message = env.repo.git_trim(&["log", "-1", "--format=%B"]);
    assert!(message.starts_with("phasewise: Demo plan"), "{message}");
    assert!(
        message.contains(&format!("Phasewise-Run: {run_id}")),
        "{message}"
    );
    assert_eq!(env.repo.read("phase-3.txt"), "phase 3\n");
    assert!(env.repo.git(&["status", "--porcelain"]).trim().is_empty());
    assert!(
        env.repo
            .git(&["branch", "--list", &run_branch])
            .contains(&run_branch)
    );
    // The history of main is one commit longer, not the phases' checkpoints.
    assert_eq!(
        env.repo.git_trim(&["rev-list", "--count", "main"]),
        env.repo
            .git_trim(&["rev-list", "--count", &env.main_sha])
            .parse::<u32>()
            .expect("count")
            .saturating_add(1)
            .to_string()
    );
}

#[test]
fn merging_twice_says_there_is_nothing_to_merge() {
    let env = completed_env();
    env.ok(&["merge", "--yes"], &[]);
    let head = env.repo.git_trim(&["rev-parse", "HEAD"]);
    let out = env.ok(&["merge", "--yes"], &[]);
    assert!(out.stdout.contains("Nothing to merge"), "{}", out.stdout);
    assert_eq!(env.repo.git_trim(&["rev-parse", "HEAD"]), head);
}

#[test]
fn merge_without_yes_needs_a_terminal() {
    let env = completed_env();
    let out = env.exec(&["merge"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("Pass --yes"), "{}", out.stderr);
    assert_eq!(env.repo.git_trim(&["rev-parse", "main"]), env.main_sha);
}

#[test]
fn merge_is_refused_while_the_run_is_not_completed() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let out = env.exec(&["merge", "--yes"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("not completed"), "{}", out.stderr);
    assert_eq!(env.statuses()[0], "awaiting_review");
    env.check_invariants();
}

#[test]
fn merge_is_refused_with_uncommitted_changes() {
    let env = completed_env();
    env.repo.write("notes.txt", "mine\n");
    let out = env.exec(&["merge", "--yes"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.contains("notes.txt") && out.stderr.contains("Commit or stash"),
        "{}",
        out.stderr
    );
    assert_eq!(
        env.repo.git_trim(&["branch", "--show-current"]),
        format!("phasewise/{}", env.run_id())
    );
    assert_eq!(env.repo.read("notes.txt"), "mine\n");
    env.check_invariants();
}

#[test]
fn a_conflicting_merge_restores_the_tree_and_leaves_the_original_branch_alone() {
    let env = completed_env();
    let run_branch = format!("phasewise/{}", env.run_id());
    env.repo.git(&["switch", "main"]);
    env.repo
        .commit_file("phase-1.txt", "mine\n", "add phase-1 by hand");
    let main_sha = env.repo.git_trim(&["rev-parse", "main"]);
    let out = env.exec(&["merge", "--yes"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("conflicts in"), "{}", out.stderr);
    assert!(out.stderr.contains("phase-1.txt"), "{}", out.stderr);
    assert!(out.stderr.contains("git merge --squash"), "{}", out.stderr);
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
    assert_eq!(env.repo.git_trim(&["rev-parse", "main"]), main_sha);
    assert!(env.repo.git(&["status", "--porcelain"]).trim().is_empty());
    assert_eq!(env.repo.read("phase-1.txt"), "mine\n");
    assert!(!env.exists("phase-3.txt"));
    assert!(
        env.repo
            .git(&["branch", "--list", &run_branch])
            .contains(&run_branch)
    );
    assert_eq!(env.state()["status"], "completed");
}
