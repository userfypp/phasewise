//! `phasewise plan` end to end, with the fake agents in `tests/fakes/`.

mod common;

use std::time::Duration;

use common::TestRepo;
use tempfile::TempDir;

const FAKE_CLAUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh");
const FAKE_CODEX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-codex.sh");

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Env {
    repo: TestRepo,
    logs: TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            repo: TestRepo::new(),
            logs: tempfile::tempdir().expect("create log dir"),
        }
    }

    fn exec(&self, args: &[&str], envs: &[(&str, &str)]) -> Out {
        let mut cmd = self.repo.phasewise();
        cmd.arg("plan")
            .args(args)
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

    fn calls(&self) -> String {
        std::fs::read_to_string(self.logs.path().join("calls.log")).unwrap_or_default()
    }

    /// The `args:` lines of calls.log.
    fn arg_lines(&self) -> Vec<String> {
        self.calls()
            .lines()
            .filter_map(|l| l.strip_prefix("args: "))
            .map(str::to_string)
            .collect()
    }

    /// The only plan directory under `.phasewise/plans/`.
    fn plan_dir(&self) -> std::path::PathBuf {
        let dir = self.repo.path().join(".phasewise/plans");
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("plans dir")
            .map(|e| e.expect("entry").path())
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        entries.remove(0)
    }

    fn assert_nothing_committed(&self, head: &str) {
        assert_eq!(self.repo.git_trim(&["rev-parse", "HEAD"]), head);
        assert_eq!(self.repo.git_trim(&["branch", "--show-current"]), "main");
        assert_eq!(self.repo.git_trim(&["branch", "--list"]).lines().count(), 1);
        assert_eq!(self.repo.git_trim(&["status", "--porcelain"]), "");
    }
}

const READ_ONLY: [&str; 6] = [
    "--permission-mode dontAsk",
    "--tools Read,Grep,Glob --strict-mcp-config",
    "--allowedTools Read,Grep,Glob",
    "--disallowedTools Edit,Write,NotebookEdit,Bash,WebFetch,WebSearch",
    "--permission-prompts none",
    "--output-format stream-json --verbose",
];

#[test]
fn success_writes_the_plan_and_prints_the_summary() {
    let env = Env::new();
    let head = env.repo.git_trim(&["rev-parse", "HEAD"]);
    let out = env.ok(&["Add a stats command"], &[]);
    let plan = env.repo.read("phasewise.md");
    assert!(plan.contains("agent = \"claude\""), "{plan}");
    assert!(plan.contains("## Phase 2: Second"));
    assert!(!plan.contains("PHASEWISE-PLAN"));
    assert!(out.stdout.contains("▶ Planning · claude"), "{}", out.stdout);
    assert!(out.stdout.contains("2 phases. The plan is valid."));
    assert!(out.stdout.contains("Cost: $0.02"), "{}", out.stdout);
    assert!(out.stdout.contains("Next steps:") && out.stdout.contains("phasewise run"));
    // The agent was called read-only and got the goal on stdin.
    let calls = env.arg_lines();
    assert_eq!(calls.len(), 1);
    for flag in READ_ONLY {
        assert!(calls[0].contains(flag), "{flag} in {}", calls[0]);
    }
    assert!(!calls[0].contains("acceptEdits"));
    assert!(env.calls().contains("Add a stats command"));
    // Files of the session; `.phasewise/` stays out of git.
    let dir = env.plan_dir();
    for file in ["attempt-1.jsonl", "attempt-1.stderr.log", "draft-1.md"] {
        assert!(dir.join(file).exists(), "{file}");
    }
    env.repo.git(&["add", "-A"]);
    assert_eq!(
        env.repo.git_trim(&["status", "--porcelain"]),
        "A  phasewise.md"
    );
    env.repo.git(&["reset", "-q"]);
    env.repo.git(&["clean", "-fdq", "--exclude=.phasewise"]);
    env.assert_nothing_committed(&head);
}

#[test]
fn a_repair_round_resumes_the_same_session() {
    let env = Env::new();
    let out = env.ok(&["goal"], &[("FAKE_PLAN_MODE", "invalid_then_valid")]);
    let calls = env.arg_lines();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(!calls[0].contains("--resume"));
    assert!(
        calls[1].contains("--resume fake-plan-session"),
        "{}",
        calls[1]
    );
    assert!(calls[1].contains("--allowedTools Read,Grep,Glob"));
    assert!(env.calls().contains("The plan you wrote cannot be used"));
    assert!(env.calls().contains("duplicate"), "{}", env.calls());
    assert!(out.stdout.contains("Repair"), "{}", out.stdout);
    assert!(env.repo.read("phasewise.md").contains("## Phase 2: Second"));
    let dir = env.plan_dir();
    assert!(dir.join("draft-1.md").exists() && dir.join("draft-2.md").exists());
}

#[test]
fn gives_up_after_two_repairs_and_leaves_the_drafts() {
    let env = Env::new();
    let out = env.exec(&["goal"], &[("FAKE_PLAN_MODE", "always_invalid")]);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert_eq!(env.arg_lines().len(), 3);
    assert!(!env.repo.path().join("phasewise.md").exists());
    assert!(
        out.stderr.contains("could not get a valid plan"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("duplicate"), "{}", out.stderr);
    let dir = env.plan_dir();
    // Paths inside the current directory are shown relative to it.
    let id = dir
        .file_name()
        .expect("plan id")
        .to_string_lossy()
        .into_owned();
    let draft = format!(".phasewise/plans/{id}/draft-3.md");
    assert!(
        out.stderr.contains(&format!("Last draft: {draft} ")),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(&format!("`phasewise check {draft}`")),
        "{}",
        out.stderr
    );
    for k in 1..=3 {
        assert!(dir.join(format!("draft-{k}.md")).exists());
    }
}

#[test]
fn missing_markers_and_agent_failure_are_repaired_or_reported() {
    let env = Env::new();
    let out = env.exec(&["goal"], &[("FAKE_PLAN_MODE", "no_markers")]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("no plan between"), "{}", out.stderr);
    assert!(!env.repo.path().join("phasewise.md").exists());

    let env = Env::new();
    let out = env.exec(&["goal"], &[("FAKE_PLAN_MODE", "fail")]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("fake planning failure"),
        "{}",
        out.stderr
    );
    assert!(!env.repo.path().join("phasewise.md").exists());
}

#[test]
fn an_existing_output_needs_force() {
    let env = Env::new();
    env.repo.write("phasewise.md", "keep me\n");
    let out = env.exec(&["goal"], &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("already exists") && out.stderr.contains("--force"));
    assert_eq!(env.repo.read("phasewise.md"), "keep me\n");
    assert!(env.calls().is_empty(), "the agent must not be called");

    env.ok(&["goal", "--force"], &[]);
    assert!(env.repo.read("phasewise.md").contains("## Phase 1: First"));
}

#[test]
fn output_path_is_used() {
    let env = Env::new();
    let out = env.ok(&["goal", "--output", "docs/plan.md"], &[]);
    assert!(env.repo.read("docs/plan.md").contains("agent = \"claude\""));
    assert!(!env.repo.path().join("phasewise.md").exists());
    assert!(out.stdout.contains("phasewise run "), "{}", out.stdout);
    assert!(out.stdout.contains("docs/plan.md"), "{}", out.stdout);
}

#[test]
fn a_goal_is_needed_without_a_terminal() {
    let env = Env::new();
    let out = env.exec(&[], &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("No goal given"), "{}", out.stderr);
    let out = env.exec(&["   "], &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("goal is empty"), "{}", out.stderr);
    assert!(env.calls().is_empty());
}

#[test]
fn codex_variant() {
    let env = Env::new();
    let out = env.ok(&["goal", "--agent", "codex"], &[]);
    assert!(env.repo.read("phasewise.md").contains("agent = \"codex\""));
    let calls = env.arg_lines();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0], "exec --json --sandbox read-only -");
    assert!(out.stdout.contains("▶ Planning · codex"), "{}", out.stdout);
    // The answer with the plan is one short line, and the path is relative.
    assert!(
        out.stdout.contains("› Plan draft (19 lines)"),
        "{}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("› <<<PHASEWISE-PLAN"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("✓ Written  phasewise.md\n"),
        "{}",
        out.stdout
    );

    let env = Env::new();
    env.ok(
        &["goal", "--agent", "codex"],
        &[("FAKE_PLAN_MODE", "invalid_then_valid")],
    );
    let calls = env.arg_lines();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        "exec --json --sandbox read-only resume fake-plan-thread -"
    );
}

#[test]
fn the_timeout_can_be_overridden() {
    let env = Env::new();
    let out = env.exec(
        &["goal"],
        &[
            ("FAKE_PLAN_MODE", "slow"),
            ("PHASEWISE_PLAN_TIMEOUT_SECONDS", "1"),
        ],
    );
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("did not finish within 1 s"),
        "{}",
        out.stderr
    );
    assert_eq!(env.arg_lines().len(), 1, "a timeout is not repaired");
    assert!(!env.repo.path().join("phasewise.md").exists());

    let out = env.exec(&["goal"], &[("PHASEWISE_PLAN_TIMEOUT_SECONDS", "soon")]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("PHASEWISE_PLAN_TIMEOUT_SECONDS"));
}

#[test]
fn model_and_effort_reach_the_command_line() {
    let env = Env::new();
    let out = env.ok(&["goal", "--model", "opus", "--effort", "high"], &[]);
    let calls = env.arg_lines();
    assert!(
        calls[0].contains("--model opus --effort high"),
        "{}",
        calls[0]
    );
    assert!(out.stdout.contains("▶ Planning · claude · opus · high"));

    let env = Env::new();
    env.ok(
        &[
            "goal", "--agent", "codex", "--model", "gpt-x", "--effort", "low",
        ],
        &[],
    );
    assert_eq!(
        env.arg_lines()[0],
        "exec --json --sandbox read-only -m gpt-x -c model_reasoning_effort=\"low\" -"
    );

    let out = Env::new().exec(&["goal", "--effort", "huge"], &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("Invalid --effort"), "{}", out.stderr);
}

#[test]
fn a_plan_for_another_agent_is_repaired() {
    let env = Env::new();
    env.ok(&["goal"], &[("FAKE_PLAN_MODE", "wrong_agent_then_valid")]);
    assert_eq!(env.arg_lines().len(), 2);
    assert!(
        env.calls().contains("it must set agent = \"claude\""),
        "{}",
        env.calls()
    );
    assert!(env.repo.read("phasewise.md").contains("agent = \"claude\""));
}

#[test]
fn needs_a_git_repository() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut cmd = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("phasewise"));
    let output = cmd
        .current_dir(dir.path())
        .env(
            "GIT_CEILING_DIRECTORIES",
            dir.path().parent().expect("parent"),
        )
        .args(["plan", "goal"])
        .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert!(!dir.path().join("phasewise.md").exists());
}

#[test]
fn a_signal_cancels_the_planning_session() {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let env = Env::new();
    let child = Command::new(assert_cmd::cargo::cargo_bin!("phasewise"))
        .current_dir(env.repo.path())
        .envs(common::isolated_git_env())
        .env("PHASEWISE_CONFIG_DIR", common::NO_CONFIG_DIR)
        .args(["plan", "goal"])
        .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
        .env("FAKE_LOG_DIR", env.logs.path())
        .env("FAKE_PLAN_MODE", "slow")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phasewise");
    let deadline = Instant::now() + Duration::from_secs(20);
    while env.calls().is_empty() {
        assert!(Instant::now() < deadline, "the fake agent did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(status.success());
    let output = child.wait_with_output().expect("wait");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Planning was interrupted"), "{stderr}");
    assert!(!env.repo.path().join("phasewise.md").exists());
}

#[test]
fn a_fence_around_the_whole_plan_is_removed() {
    for agent in ["claude", "codex"] {
        let env = Env::new();
        env.ok(&["--agent", agent, "Goal"], &[("FAKE_PLAN_MODE", "fenced")]);
        let plan = env.repo.read("phasewise.md");
        assert!(plan.starts_with("+++\n"), "{plan}");
        assert!(!plan.contains("```markdown"), "{plan}");
        assert!(plan.contains("## Phase 1: First"), "{plan}");
        // Both phases survive: the inner closing fence did not end the plan early.
        let check = env
            .repo
            .phasewise()
            .arg("check")
            .output()
            .expect("run check");
        let stdout = String::from_utf8_lossy(&check.stdout);
        assert!(stdout.contains("2 phases"), "{stdout}");
        assert_eq!(env.arg_lines().len(), 1, "no repair round needed");
    }
}

#[test]
fn an_agent_that_cannot_start_is_not_asked_again() {
    for agent in ["claude", "codex"] {
        let env = Env::new();
        let out = env.exec(&["--agent", agent, "goal"], &[("FAKE_PLAN_MODE", "crash")]);
        assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
        assert_eq!(env.arg_lines().len(), 1, "no repair round for {agent}");
        assert!(
            out.stderr
                .contains(&format!("{agent} failed (exit code 2)")),
            "{}",
            out.stderr
        );
        assert!(
            out.stderr.contains("attempt-1.stderr.log"),
            "{}",
            out.stderr
        );
        let log = std::fs::read_to_string(env.plan_dir().join("attempt-1.stderr.log"))
            .expect("stderr log");
        assert!(log.contains("fake option rejected"), "{log}");
        assert!(!env.repo.path().join("phasewise.md").exists());
    }
}

#[test]
fn a_plan_file_written_meanwhile_is_kept() {
    let env = Env::new();
    let out = env.exec(&["goal"], &[("FAKE_PLAN_MODE", "creates_output")]);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert_eq!(env.repo.read("phasewise.md"), "mine\n");
    assert!(
        out.stderr
            .contains("appeared while the agent was working and was left as it is"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("Last draft: "), "{}", out.stderr);
    let draft = std::fs::read_to_string(env.plan_dir().join("draft-1.md")).expect("draft");
    assert!(draft.contains("## Phase 2: Second"), "{draft}");
    assert!(!out.stdout.contains("Written"), "{}", out.stdout);
}
