mod common;

use common::TestRepo;
use predicates::prelude::*;

const FAKE_CLAUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh");
const FAKE_CODEX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-codex.sh");
const MISSING: &str = "/nonexistent/phasewise-no-agent";

fn doctor(repo: &TestRepo, claude: &str, codex: &str) -> assert_cmd::assert::Assert {
    repo.phasewise()
        .arg("doctor")
        .env("PHASEWISE_CLAUDE_BIN", claude)
        .env("PHASEWISE_CODEX_BIN", codex)
        .assert()
}

#[test]
fn healthy_repo_with_both_agents() {
    let repo = TestRepo::new();
    repo.write(
        "phasewise.md",
        include_str!("../src/templates/phasewise.md"),
    );
    repo.commit_all("add plan");
    doctor(&repo, FAKE_CLAUDE, FAKE_CODEX)
        .success()
        .stdout(predicate::str::contains("0.0.0-fake (Claude Code)"))
        .stdout(predicate::str::contains("codex-cli 0.0.0-fake"))
        .stdout(predicate::str::contains("PHASEWISE_CLAUDE_BIN in effect"))
        .stdout(predicate::str::contains("working tree clean"))
        .stdout(predicate::str::contains("on branch main"))
        .stdout(predicate::str::contains("phasewise.md is valid"))
        .stdout(predicate::str::contains("no run yet"))
        .stdout(predicate::str::contains("Tools are ready"));
}

#[test]
fn missing_agents_exit_one() {
    let repo = TestRepo::new();
    doctor(&repo, MISSING, MISSING)
        .code(1)
        .stdout(predicate::str::contains("✗ claude"))
        .stdout(predicate::str::contains("✗ codex"))
        .stdout(predicate::str::contains("Not ready"));
}

#[test]
fn one_agent_is_enough() {
    let repo = TestRepo::new();
    repo.write(
        "phasewise.md",
        include_str!("../src/templates/phasewise.md"),
    );
    repo.commit_all("add plan");
    doctor(&repo, FAKE_CLAUDE, MISSING)
        .success()
        .stdout(predicate::str::contains("✗ codex"));
}

#[test]
fn a_missing_plan_agent_is_a_warning() {
    let repo = TestRepo::new();
    repo.write(
        "phasewise.md",
        include_str!("../src/templates/phasewise.md"),
    );
    repo.commit_all("add plan");
    doctor(&repo, MISSING, FAKE_CODEX)
        .success()
        .stdout(predicate::str::contains(
            "! the plan's agent is claude, which was not found; install it, set PHASEWISE_CLAUDE_BIN, or run with `--agent`",
        ))
        .stdout(predicate::str::contains("Tools are ready"));
}

#[test]
fn dirty_tree_and_missing_plan_are_reported() {
    let repo = TestRepo::new();
    repo.write("a.txt", "a");
    repo.write("b.txt", "b");
    doctor(&repo, FAKE_CLAUDE, FAKE_CODEX)
        .code(1)
        .stdout(predicate::str::contains("Not ready"))
        .stdout(predicate::str::contains("Tools are ready").not())
        .stdout(predicate::str::contains("2 uncommitted path(s)"))
        .stdout(predicate::str::contains("phasewise.md not found"))
        .stdout(predicate::str::contains("will be added"));
}

#[test]
fn outside_a_repository() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("phasewise"))
        .arg("doctor")
        .current_dir(dir.path())
        .envs(common::isolated_git_env())
        .env(
            "GIT_CEILING_DIRECTORIES",
            dir.path().parent().unwrap_or(dir.path()),
        )
        .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
        .env("PHASEWISE_CODEX_BIN", FAKE_CODEX)
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Not ready"))
        .stdout(predicate::str::contains("not inside a git repository"))
        .stdout(predicate::str::contains("git init"));
}

#[test]
fn doctor_writes_nothing() {
    let repo = TestRepo::new();
    let exclude = repo.path().join(".git/info/exclude");
    let before = std::fs::read_to_string(&exclude).unwrap_or_default();
    doctor(&repo, FAKE_CLAUDE, FAKE_CODEX).code(1);
    assert!(!repo.path().join(".phasewise").exists());
    assert_eq!(
        std::fs::read_to_string(&exclude).unwrap_or_default(),
        before
    );
}

#[test]
fn a_login_that_is_not_confirmed_is_a_warning() {
    let repo = TestRepo::new();
    repo.phasewise()
        .arg("doctor")
        .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
        .env("PHASEWISE_CODEX_BIN", FAKE_CODEX)
        .env("FAKE_LOGIN", "out")
        .assert()
        .stdout(predicate::str::contains(
            "! login not confirmed: run `claude auth login`",
        ))
        .stdout(predicate::str::contains(
            "! login not confirmed: run `codex login`",
        ));
    // Logged in: no warning.
    doctor(&repo, FAKE_CLAUDE, FAKE_CODEX)
        .stdout(predicate::str::contains("login not confirmed").not());
}

#[test]
fn a_missing_login_does_not_make_doctor_fail() {
    let repo = TestRepo::new();
    repo.write(
        "phasewise.md",
        include_str!("../src/templates/phasewise.md"),
    );
    repo.commit_all("add plan");
    repo.phasewise()
        .arg("doctor")
        .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
        .env("PHASEWISE_CODEX_BIN", MISSING)
        .env("FAKE_LOGIN", "out")
        .assert()
        .success();
}
