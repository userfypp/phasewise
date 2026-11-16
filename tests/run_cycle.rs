//! End-to-end phase cycles with the fake agents in `tests/fakes/`.

mod common;

use std::time::{Duration, Instant};

use common::TestRepo;
use serde_json::Value;
use tempfile::TempDir;

const FAKE_CLAUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh");
const FAKE_CODEX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-codex.sh");
const VERIFY: &str = "! grep -q broken phase-*.txt";

/// Plan with three phases; `front` is extra front matter.
fn plan_with(front: &str) -> String {
    format!(
        "+++\nverify = [\"{VERIFY}\"]\n{front}+++\n\n# Demo plan\n\nShared context.\n\n\
## Phase 1: First\n\nDo one.\n\n## Phase 2: Second\n\nDo two.\n\n## Phase 3: Third\n\nDo three.\n"
    )
}

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
        Env::with_plan(&plan_with(""))
    }

    fn with_plan(plan: &str) -> Env {
        let repo = TestRepo::new();
        repo.write("phasewise.md", plan);
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

    fn state(&self) -> Value {
        let id = self.repo.read(".phasewise/current");
        let text = self
            .repo
            .read(&format!(".phasewise/runs/{}/state.json", id.trim()));
        serde_json::from_str(&text).expect("state.json is JSON")
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

    fn calls_log(&self) -> String {
        std::fs::read_to_string(self.logs.path().join("calls.log")).unwrap_or_default()
    }

    /// Invariants of every test: the original branch is untouched and
    /// phasewise's own files never reach a commit.
    fn check_invariants(&self) {
        assert_eq!(self.repo.git_trim(&["rev-parse", "main"]), self.main_sha);
        let names = self.repo.git(&["log", "--all", "--name-only", "--format="]);
        assert!(!names.contains(".phasewise"), "{names}");
    }
}

fn failure_of(state: &Value, phase: usize) -> (String, String) {
    let failure = &state["phases"][phase - 1]["failure"];
    (
        failure["kind"].as_str().expect("failure kind").to_string(),
        failure["detail"]
            .as_str()
            .expect("failure detail")
            .to_string(),
    )
}

fn full_cycle(agent_args: &[&str]) {
    let env = Env::new();
    let mut run_args = vec!["run"];
    run_args.extend_from_slice(agent_args);
    let out = env.ok(&run_args, &[]);
    assert!(out.stdout.contains("phasewise/"), "{}", out.stdout);
    assert!(out.stdout.contains("▶ Phase 1/3 · First"), "{}", out.stdout);
    assert!(out.stdout.contains("attempt 1 · "), "{}", out.stdout);
    assert!(
        out.stdout.contains("✓ Phase 1 ready for review · 1 file"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("● Checkpoint "), "{}", out.stdout);
    // Not a terminal: no colour codes.
    assert!(!out.stdout.contains('\x1b'), "{}", out.stdout);
    assert!(out.stdout.contains("phasewise next"), "{}", out.stdout);
    assert_eq!(env.statuses(), ["awaiting_review", "pending", "pending"]);

    env.ok(&["next"], &[]);
    assert_eq!(env.statuses(), ["done", "awaiting_review", "pending"]);
    env.ok(&["next"], &[]);
    assert_eq!(env.statuses(), ["done", "done", "awaiting_review"]);

    let run_id = env.run_id();
    let state = env.state();
    for n in 1..=3 {
        let sha = state["phases"][n - 1]["checkpoint_sha"]
            .as_str()
            .expect("checkpoint");
        let message = env.repo.git(&["log", "-1", "--format=%B", sha]);
        let title = ["First", "Second", "Third"][n - 1];
        assert_eq!(
            message.trim_end(),
            format!(
                "phasewise: phase {n} — {title}\n\nPhasewise-Run: {run_id}\nPhasewise-Phase: {n}"
            )
        );
        assert_eq!(
            env.repo.read(&format!("phase-{n}.txt")),
            format!("phase {n}\n")
        );
    }

    let diff = env.ok(&["diff", "2"], &[]).stdout;
    assert!(diff.contains("phase-2.txt"), "{diff}");
    assert!(diff.contains("+phase 2"), "{diff}");
    assert!(!diff.contains("phase-1.txt"), "{diff}");

    let status = env.ok(&["status"], &[]).stdout;
    assert!(status.contains(&run_id), "{status}");
    assert!(status.contains("awaiting review"), "{status}");
    assert!(
        status.contains("$0.0100") || agent_args.contains(&"codex"),
        "{status}"
    );
    assert!(status.contains("Third"), "{status}");

    let done = env.ok(&["next"], &[]);
    assert!(done.stdout.contains("All phases done."), "{}", done.stdout);
    assert!(
        done.stdout.contains(&format!(
            "git switch main && git merge --squash phasewise/{run_id} && git commit"
        )),
        "{}",
        done.stdout
    );
    assert_eq!(env.state()["status"], "completed");
    assert_eq!(env.statuses(), ["done", "done", "done"]);
    assert!(env.ok(&["status"], &[]).stdout.contains("completed"));
    env.check_invariants();
}

#[test]
fn full_three_phase_cycle() {
    full_cycle(&[]);
}

#[test]
fn full_cycle_with_codex() {
    full_cycle(&["--agent", "codex"]);
}

#[test]
fn auto_stops_before_approving_the_last_phase() {
    let env = Env::new();
    env.ok(&["run", "--auto"], &[]);
    assert_eq!(env.statuses(), ["done", "done", "awaiting_review"]);
    assert_eq!(env.state()["status"], "active");
    env.check_invariants();
}

#[test]
fn until_stops_when_the_phase_is_awaiting_review() {
    let env = Env::new();
    env.ok(&["run", "--until", "2"], &[]);
    assert_eq!(env.statuses(), ["done", "awaiting_review", "pending"]);
    // The review of phase 2 is approved by `next`, which can stop at the last.
    env.ok(&["next", "--until", "3"], &[]);
    assert_eq!(env.statuses(), ["done", "done", "awaiting_review"]);
    env.check_invariants();
}

#[test]
fn until_beats_auto() {
    let env = Env::new();
    env.ok(&["run", "--auto", "--until", "1"], &[]);
    assert_eq!(env.statuses(), ["awaiting_review", "pending", "pending"]);
}

#[test]
fn until_out_of_range_is_refused_without_side_effects() {
    let env = Env::new();
    for n in ["0", "4"] {
        let out = env.exec(&["run", "--until", n], &[]);
        assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
        assert!(out.stderr.contains("from 1"), "{}", out.stderr);
        assert!(out.stderr.contains("to 3"), "{}", out.stderr);
    }
    // No run was started.
    assert_eq!(env.ok(&["status", "--json"], &[]).stdout.trim(), "null");

    env.ok(&["run", "--until", "2"], &[]);
    let out = env.exec(&["next", "--until", "2"], &[]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("--until 3"), "{}", out.stderr);
    // The review stays open.
    assert_eq!(env.statuses(), ["done", "awaiting_review", "pending"]);
}

#[test]
fn verify_failure_is_retried_in_the_same_session() {
    let env = Env::new();
    env.ok(&["run"], &[("FAKE_MODE", "fix_on_resume")]);
    assert_eq!(env.statuses()[0], "awaiting_review");
    assert_eq!(env.state()["phases"][0]["attempts"], 2);
    let log = env.calls_log();
    let calls: Vec<&str> = log.split("=== call ===").skip(1).collect();
    assert_eq!(calls.len(), 2, "{log}");
    assert!(calls[1].contains("--resume fake-session-1"), "{}", calls[1]);
    assert!(calls[1].contains(VERIFY), "{}", calls[1]);
    assert_eq!(env.repo.read("phase-1.txt"), "phase 1\n");
    env.check_invariants();
}

#[test]
fn the_spending_limit_stops_the_run_before_the_next_phase() {
    // The fake agent reports $0.01 per attempt.
    let env = Env::with_plan(&plan_with("max_cost_usd = 0.02\n"));
    let out = env.exec(&["run", "--auto"], &[]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("$0.02 of its $0.02 limit"),
        "{}",
        out.stderr
    );
    assert_eq!(
        env.statuses(),
        ["done", "awaiting_review", "pending"],
        "{}",
        out.stderr
    );
}

#[test]
fn status_shows_the_cost_against_the_limit() {
    let env = Env::with_plan(&plan_with("max_cost_usd = 5\n"));
    env.ok(&["run"], &[]);
    let out = env.ok(&["status"], &[]);
    assert!(
        out.stdout.contains("Cost:   $0.01 of $5.00 limit"),
        "{}",
        out.stdout
    );
}

#[test]
fn status_json_describes_the_run() {
    let env = Env::with_plan(&plan_with("max_cost_usd = 5\n"));
    assert_eq!(env.ok(&["status", "--json"], &[]).stdout.trim(), "null");
    env.ok(&["run"], &[]);
    let out = env.ok(&["status", "--json"], &[]);
    let value: Value = serde_json::from_str(&out.stdout).expect("valid JSON");
    assert_eq!(value["status"], "active");
    assert_eq!(value["agent"], "claude");
    assert_eq!(value["max_cost_usd"], 5.0);
    assert_eq!(value["cost_usd"], 0.01);
    let phases = value["phases"].as_array().expect("phases");
    assert_eq!(phases.len(), 3);
    assert_eq!(phases[0]["status"], "awaiting_review");
    assert_eq!(phases[0]["title"], "First");
    assert_eq!(phases[0]["attempts"], 1);
    assert!(phases[0]["checkpoint_sha"].is_string());
    assert_eq!(phases[0]["failure"], Value::Null);
    assert_eq!(phases[1]["status"], "pending");
    assert_eq!(phases[1]["cost_usd"], Value::Null);
    assert!(phases[0]["duration_secs"].is_u64());
    assert_eq!(phases[1]["duration_secs"], Value::Null);
    assert!(value["duration_secs"].is_u64());
}

#[test]
fn status_shows_the_time_of_the_phases() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let out = env.ok(&["status"], &[]);
    assert!(out.stdout.contains("TIME"), "{}", out.stdout);
    assert!(out.stdout.contains("Time:   "), "{}", out.stdout);
}

#[test]
fn status_shows_the_limits_the_agent_reported() {
    let env = Env::new();
    let before: Value =
        serde_json::from_str(&env.ok(&["status", "--json"], &[]).stdout).unwrap_or(Value::Null);
    assert_eq!(before, Value::Null);
    env.ok(&["run"], &[]);
    let stored: Value =
        serde_json::from_str(&env.repo.read(".phasewise/rate-limits.json")).expect("valid JSON");
    assert_eq!(stored["agent"], "claude");
    assert!(stored["seen_at"].is_i64(), "{stored}");
    assert_eq!(stored["windows"][0]["name"], "five_hour");

    let json: Value =
        serde_json::from_str(&env.ok(&["status", "--json"], &[]).stdout).expect("status json");
    let limits = json["limits"].as_array().expect("limits");
    assert_eq!(limits.len(), 2);
    assert_eq!(limits[0]["name"], "five_hour");
    assert_eq!(limits[0]["utilization"], 0.36);
    assert_eq!(limits[0]["resets_at"], 4_102_444_800_i64);
    assert_eq!(limits[0]["status"], "allowed");
    assert_eq!(limits[1]["name"], "seven_day");
    assert_eq!(limits[1]["status"], "allowed_warning");
    assert!(json["limits_seen_at"].is_i64());

    let text = env.ok(&["status"], &[]).stdout;
    let line = text
        .lines()
        .find(|l| l.starts_with("Limits: "))
        .unwrap_or_else(|| panic!("no Limits line in {text}"));
    assert!(line.contains("5 h 36 % · resets"), "{line}");
    assert!(line.contains("7 d 49 % · allowed_warning"), "{line}");
    env.check_invariants();
}

#[test]
fn status_hides_the_limits_when_the_setting_is_off() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let config = tempfile::tempdir().expect("config dir");
    std::fs::write(config.path().join("settings.toml"), "limits = false\n").expect("settings");
    let envs = [(
        "PHASEWISE_CONFIG_DIR",
        config.path().to_str().expect("utf-8"),
    )];

    let shown = env.ok(&["status"], &[]).stdout;
    assert!(shown.lines().any(|l| l.starts_with("Limits: ")), "{shown}");
    let hidden = env.ok(&["status"], &envs).stdout;
    assert!(!hidden.contains("Limits:"), "{hidden}");
    let json: Value =
        serde_json::from_str(&env.ok(&["status", "--json"], &envs).stdout).expect("status json");
    assert_eq!(json["limits"], Value::Null);
}

#[test]
fn the_bell_is_never_written_when_stderr_is_not_a_terminal() {
    let env = Env::new();
    let config = tempfile::tempdir().expect("config dir");
    std::fs::write(config.path().join("settings.toml"), "bell = true\n").expect("settings");
    let envs = [(
        "PHASEWISE_CONFIG_DIR",
        config.path().to_str().expect("utf-8"),
    )];
    let out = env.ok(&["run"], &envs);
    assert!(!out.stderr.contains('\x07') && !out.stdout.contains('\x07'));
    let out = env.ok(&["next"], &envs);
    assert!(!out.stderr.contains('\x07') && !out.stdout.contains('\x07'));
}

#[test]
fn logs_describe_a_failed_phase() {
    let env = Env::new();
    assert_eq!(env.exec(&["logs"], &[]).code, 1);
    env.exec(&["run"], &[("FAKE_MODE", "never_fixes")]);
    let out = env.ok(&["logs"], &[]);
    assert!(
        out.stdout.contains("Phase 1 · First (failed, 2 attempts)"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("Failure:"), "{}", out.stdout);
    assert!(
        out.stdout.contains("phase-01.attempt-2.jsonl"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("phase-01.verify.log"), "{}", out.stdout);
    assert_eq!(env.exec(&["logs", "9"], &[]).code, 1);
}

#[test]
fn an_exhausted_budget_refuses_retry_and_next_without_losing_work() {
    let env = Env::with_plan(&plan_with("max_cost_usd = 0.01\n"));
    env.ok(&["run"], &[]);
    for command in ["retry", "next"] {
        let out = env.exec(&[command], &[]);
        assert_eq!(out.code, 1, "{command}: {}\n{}", out.stdout, out.stderr);
        assert!(out.stderr.contains("limit"), "{}", out.stderr);
        assert_eq!(env.statuses()[0], "awaiting_review", "{command}");
        assert_eq!(env.repo.read("phase-1.txt"), "phase 1\n");
    }
    // A retry keeps what the first attempt spent, so the limit still counts it.
    let limit = [("PHASEWISE_MAX_COST_USD", "1")];
    let json = env.ok(&["status", "--json"], &limit).stdout;
    let value: Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(value["max_cost_usd"], 1.0);
    env.ok(&["retry"], &limit);
    assert_eq!(env.state()["phases"][0]["cost_usd"], 0.02);
}

#[test]
fn run_refuses_a_model_for_a_first_phase_of_the_other_agent() {
    let plan = "+++\nagent = \"claude\"\n+++\n\n## Phase 1: First\n\nDo one.\n\n```phasewise\nagent = \"codex\"\n```\n";
    let env = Env::with_plan(plan);
    let out = env.exec(&["run", "--model", "sonnet"], &[]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("Phase 1 runs with codex"),
        "{}",
        out.stderr
    );
    assert!(!env.repo.path().join(".phasewise/current").exists());
}

#[test]
fn the_environment_raises_the_spending_limit() {
    let env = Env::with_plan(&plan_with("max_cost_usd = 0.015\n"));
    env.ok(&["run", "--auto"], &[("PHASEWISE_MAX_COST_USD", "1")]);
    assert_eq!(env.statuses()[2], "awaiting_review");
}

#[test]
fn a_bad_spending_limit_in_the_environment_is_refused() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("PHASEWISE_MAX_COST_USD", "abc")]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("PHASEWISE_MAX_COST_USD must be"),
        "{}",
        out.stderr
    );
}

#[test]
fn the_spending_limit_prevents_a_retry() {
    let env = Env::with_plan(&plan_with("max_cost_usd = 0.005\n"));
    let out = env.exec(&["run"], &[("FAKE_MODE", "fix_on_resume")]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    let (kind, detail) = failure_of(&env.state(), 1);
    assert_eq!(kind, "verify_failed");
    assert!(detail.contains("Not retrying"), "{detail}");
    assert_eq!(env.state()["phases"][0]["attempts"], 1);
}

#[test]
fn definitive_verify_failure_keeps_the_tree() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("FAKE_MODE", "never_fixes")]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("verify_failed"), "{}", out.stdout);
    assert!(out.stdout.contains("phasewise retry"), "{}", out.stdout);
    let state = env.state();
    assert_eq!(env.statuses()[0], "failed");
    assert_eq!(state["phases"][0]["attempts"], 2);
    assert!(state["phases"][0]["checkpoint_sha"].is_null());
    let (kind, detail) = failure_of(&state, 1);
    assert_eq!(kind, "verify_failed");
    assert!(
        detail.contains(VERIFY) && detail.contains("exit code 1"),
        "{detail}"
    );
    assert_eq!(env.repo.read("phase-1.txt"), "broken\n");
    assert!(
        env.repo
            .git(&["status", "--porcelain"])
            .contains("phase-1.txt")
    );
    let status = env.ok(&["status"], &[]).stdout;
    assert!(status.contains("verify_failed"), "{status}");
    // `next` refuses while a phase is failed.
    let next = env.exec(&["next"], &[]);
    assert_eq!(next.code, 1);
    assert!(next.stderr.contains("phasewise retry"), "{}", next.stderr);
    env.check_invariants();
}

#[test]
fn agent_error_fails_the_phase() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("FAKE_MODE", "fail")]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    let (kind, detail) = failure_of(&env.state(), 1);
    assert_eq!(kind, "agent_error");
    assert!(
        detail.contains("claude") && detail.contains("exit code 1"),
        "{detail}"
    );
    assert!(detail.contains("fake failure"), "{detail}");
    assert!(detail.contains("stderr.log"), "{detail}");
    assert_eq!(env.state()["phases"][0]["attempts"], 1);
    env.check_invariants();
}

#[test]
fn agent_timeout_stops_the_process() {
    let env = Env::new();
    let started = Instant::now();
    let out = env.exec(
        &["run"],
        &[
            ("FAKE_MODE", "slow"),
            ("PHASEWISE_PHASE_TIMEOUT_SECONDS", "2"),
        ],
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(failure_of(&env.state(), 1).0, "timeout");
    env.check_invariants();
}

#[test]
fn timeout_override_wins_over_a_phase_timeout() {
    let plan = plan_with("").replace(
        "Do one.\n",
        "Do one.\n\n```phasewise\nphase_timeout_minutes = 30\n```\n",
    );
    let env = Env::with_plan(&plan);
    let started = Instant::now();
    let out = env.exec(
        &["run"],
        &[
            ("FAKE_MODE", "slow"),
            ("PHASEWISE_PHASE_TIMEOUT_SECONDS", "1"),
        ],
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(failure_of(&env.state(), 1).0, "timeout");
    env.check_invariants();
}

#[test]
fn invalid_timeout_override_is_an_error() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("PHASEWISE_PHASE_TIMEOUT_SECONDS", "soon")]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("PHASEWISE_PHASE_TIMEOUT_SECONDS"),
        "{}",
        out.stderr
    );
    env.check_invariants();
}

#[test]
fn a_timeout_override_above_a_year_is_refused_before_the_agent_starts() {
    let env = Env::new();
    let out = env.exec(
        &["run"],
        &[("PHASEWISE_PHASE_TIMEOUT_SECONDS", "18446744073709551615")],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("PHASEWISE_PHASE_TIMEOUT_SECONDS must be at most 31536000 seconds"),
        "{}",
        out.stderr
    );
    assert!(env.calls_log().is_empty(), "the agent must not start");
    env.check_invariants();
}

#[test]
fn a_zero_timeout_override_is_refused_before_the_agent_starts() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("PHASEWISE_PHASE_TIMEOUT_SECONDS", "0")]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("PHASEWISE_PHASE_TIMEOUT_SECONDS must be at least 1 second"),
        "{}",
        out.stderr
    );
    assert!(env.calls_log().is_empty(), "the agent must not start");
    env.check_invariants();
}

#[test]
fn verify_timeout_fails_the_phase() {
    let plan = "+++\nverify = [\"sleep 30\"]\nverify_timeout_seconds = 1\nmax_retries = 0\n+++\n\n# T\n\n## Phase 1: Only\n\nDo it.\n";
    let env = Env::with_plan(plan);
    let out = env.exec(&["run"], &[]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    let (kind, detail) = failure_of(&env.state(), 1);
    assert_eq!(kind, "verify_failed");
    assert!(detail.contains("timed out"), "{detail}");
    assert!(out.stdout.contains("timed out after 1 s"), "{}", out.stdout);
    env.check_invariants();
}

#[test]
fn agent_commits_are_kept() {
    let env = Env::new();
    env.ok(&["run"], &[("FAKE_MODE", "commit")]);
    assert_eq!(env.statuses()[0], "awaiting_review");
    let run_id = env.run_id();
    let subjects = env
        .repo
        .git(&["log", "--format=%s", &format!("phasewise/{run_id}")]);
    assert!(subjects.contains("agent commit for phase 1"), "{subjects}");
    assert!(subjects.contains("phasewise: phase 1"), "{subjects}");
    let diff = env.ok(&["diff", "1"], &[]).stdout;
    assert!(diff.contains("phase-1.txt"), "{diff}");
    env.check_invariants();
}

#[test]
fn agent_switching_branch_is_an_error() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("FAKE_MODE", "branch")]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    let (kind, detail) = failure_of(&env.state(), 1);
    assert_eq!(kind, "agent_error");
    assert!(detail.contains("fake-agent-branch-1"), "{detail}");
    assert!(
        detail.contains(&format!("git switch phasewise/{}", env.run_id())),
        "{detail}"
    );
    env.check_invariants();
}

#[test]
fn checkpoint_failure_fails_the_phase() {
    let env = Env::new();
    let out = env.exec(&["run"], &[("FAKE_MODE", "index_lock")]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    let state = env.state();
    assert_eq!(env.statuses()[0], "failed");
    let (kind, detail) = failure_of(&state, 1);
    assert_eq!(kind, "checkpoint_failed");
    assert!(detail.contains("phasewise retry"), "{detail}");
    assert!(
        !env.repo
            .path()
            .join(format!(".phasewise/runs/{}/lock", env.run_id()))
            .exists()
    );
}

#[test]
fn unknown_git_identity_is_refused_up_front() {
    let env = Env::new();
    env.repo.git(&["config", "--unset", "user.name"]);
    env.repo.git(&["config", "--unset", "user.email"]);
    env.repo.git(&["config", "user.useConfigOnly", "true"]);
    let out = env.exec(&["run"], &[]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("git does not know who you are"),
        "{}",
        out.stderr
    );
    assert!(
        env.repo
            .git(&["branch", "--list", "phasewise/*"])
            .trim()
            .is_empty()
    );
    assert!(!env.repo.path().join(".phasewise/current").exists());
}

#[test]
fn next_without_a_git_identity_leaves_the_review_open() {
    let env = Env::new();
    assert_eq!(env.exec(&["run"], &[]).code, 0);
    assert_eq!(env.statuses()[0], "awaiting_review");
    env.repo.git(&["config", "--unset", "user.name"]);
    env.repo.git(&["config", "--unset", "user.email"]);
    env.repo.git(&["config", "user.useConfigOnly", "true"]);
    let out = env.exec(&["next"], &[]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("git does not know who you are"),
        "{}",
        out.stderr
    );
    assert_eq!(env.statuses()[0], "awaiting_review");
}

#[test]
fn dirty_tree_is_refused_without_side_effects() {
    let env = Env::new();
    env.repo.write("dirty.txt", "x\n");
    let out = env.exec(&["run"], &[]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("dirty.txt"), "{}", out.stderr);
    assert!(out.stderr.contains("stash"), "{}", out.stderr);
    assert!(
        env.repo
            .git(&["branch", "--list", "phasewise/*"])
            .trim()
            .is_empty()
    );
    assert!(!env.repo.path().join(".phasewise/runs").exists());
    assert_eq!(env.repo.git_trim(&["branch", "--show-current"]), "main");
    env.check_invariants();
}

#[test]
fn second_run_while_active_is_refused() {
    let env = Env::new();
    env.ok(&["run"], &[]);
    let out = env.exec(&["run"], &[]);
    assert_eq!(out.code, 1);
    for hint in ["phasewise next", "phasewise review", "phasewise abort"] {
        assert!(out.stderr.contains(hint), "{}", out.stderr);
    }
    env.check_invariants();
}

#[test]
fn status_and_diff_without_a_run() {
    let env = Env::new();
    let status = env.ok(&["status"], &[]);
    assert!(
        status
            .stdout
            .contains("No run yet. Start one with `phasewise run`.")
    );
    let diff = env.exec(&["diff"], &[]);
    assert_eq!(diff.code, 1);
    env.check_invariants();
}

/// Claude plan whose second phase runs with Codex.
fn mixed_plan() -> String {
    format!(
        "+++\nagent = \"claude\"\nverify = [\"{VERIFY}\"]\nagent_args = [\"--claude-only-flag\"]\n+++\n\n\
# Mixed plan\n\n## Phase 1: First\n\nDo one.\n\n## Phase 2: Second\n\nDo two.\n\n\
```phasewise\nagent = \"codex\"\n```\n"
    )
}

fn calls(log: &str) -> Vec<&str> {
    log.split("=== call ===").skip(1).collect()
}

#[test]
fn mixed_agents_run_each_phase_with_its_agent() {
    let env = Env::with_plan(&mixed_plan());
    let out = env.ok(&["run", "--auto"], &[]);
    assert!(out.stdout.contains("attempt 1 · claude"), "{}", out.stdout);
    assert!(out.stdout.contains("attempt 1 · codex"), "{}", out.stdout);
    assert_eq!(env.statuses(), ["done", "awaiting_review"]);
    let log = env.calls_log();
    let calls = calls(&log);
    assert_eq!(calls.len(), 2, "{log}");
    assert!(calls[0].contains("--output-format"), "{}", calls[0]);
    assert!(calls[0].contains("--claude-only-flag"), "{}", calls[0]);
    assert!(calls[1].contains("exec --json"), "{}", calls[1]);
    assert!(!calls[1].contains("--claude-only-flag"), "{}", calls[1]);

    let json: Value =
        serde_json::from_str(&env.ok(&["status", "--json"], &[]).stdout).expect("status json");
    assert_eq!(json["agent"], "claude");
    assert_eq!(json["phases"][0]["agent"], "claude");
    assert_eq!(json["phases"][1]["agent"], "codex");
    let status = env.ok(&["status"], &[]).stdout;
    assert!(
        status.contains("Agent:  claude (phase 2: codex)"),
        "{status}"
    );
    env.check_invariants();
}

#[test]
fn run_agent_switch_forces_every_phase() {
    let env = Env::with_plan(&mixed_plan());
    env.ok(&["run", "--agent", "codex", "--auto"], &[]);
    let log = env.calls_log();
    let calls = calls(&log);
    assert_eq!(calls.len(), 2, "{log}");
    for call in calls {
        assert!(call.contains("exec --json"), "{call}");
    }
    let json: Value =
        serde_json::from_str(&env.ok(&["status", "--json"], &[]).stdout).expect("status json");
    assert_eq!(json["phases"][0]["agent"], "codex");
    assert_eq!(json["phases"][1]["agent"], "codex");
    env.check_invariants();
}

#[test]
fn verify_retry_resumes_with_the_phase_agent() {
    let env = Env::with_plan(&mixed_plan());
    env.ok(&["run", "--auto"], &[("FAKE_MODE_2", "fix_on_resume")]);
    assert_eq!(env.state()["phases"][1]["attempts"], 2);
    let log = env.calls_log();
    let calls = calls(&log);
    assert_eq!(calls.len(), 3, "{log}");
    assert!(calls[2].contains("exec --json"), "{}", calls[2]);
    assert!(calls[2].contains("resume fake-thread-2"), "{}", calls[2]);
    assert_eq!(env.repo.read("phase-2.txt"), "phase 2\n");
    env.check_invariants();
}

#[test]
fn next_model_refused_for_other_agent_phase() {
    let env = Env::with_plan(&mixed_plan());
    env.ok(&["run"], &[]);
    let out = env.exec(&["next", "--model", "opus"], &[]);
    assert_ne!(out.code, 0, "{}", out.stdout);
    assert!(
        out.stderr
            .contains("Phase 2 runs with codex; --model applies only to the run's agent (claude) phases. Run without --model."),
        "{}",
        out.stderr
    );
    assert_eq!(env.statuses(), ["awaiting_review", "pending"]);
    assert!(env.state()["model_override"].is_null());
    env.check_invariants();
}
