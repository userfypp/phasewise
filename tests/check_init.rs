mod common;

use common::TestRepo;
use predicates::prelude::*;

const EXAMPLES: [&str; 3] = [
    "examples/plans/python-add-cli-option.md",
    "examples/plans/rust-add-subcommand.md",
    "examples/plans/typescript-migration.md",
];

fn example(rel: &str) -> String {
    format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn check_passes_on_example_plans() {
    let repo = TestRepo::new();
    for rel in EXAMPLES {
        repo.phasewise()
            .args(["check", &example(rel)])
            .assert()
            .success()
            .stdout(predicate::str::contains("The plan is valid."));
    }
}

#[test]
fn check_passes_on_init_template() {
    let repo = TestRepo::new();
    repo.phasewise().arg("init").assert().success();
    repo.phasewise()
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("2 phases. The plan is valid."))
        .stdout(predicate::str::contains("  1. First step"))
        .stdout(predicate::str::contains("My plan · phasewise.md"));
}

#[test]
fn init_shows_a_relative_path_and_mentions_the_home_screen() {
    let repo = TestRepo::new();
    repo.phasewise()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Created phasewise.md."))
        .stdout(predicate::str::contains("phasewise` with no arguments"));
}

#[test]
fn check_reports_errors_with_lines() {
    let repo = TestRepo::new();
    repo.write("bad.md", "# T\n\n## A\n\n## B\n\ntext\n");
    repo.phasewise()
        .args(["check", "bad.md"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("bad.md:3: "))
        .stderr(predicate::str::contains("error: the plan has 1 problem(s)"));
}

#[test]
fn check_missing_file_exits_1() {
    let repo = TestRepo::new();
    repo.phasewise()
        .arg("check")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Plan file not found"));
}

#[test]
fn check_finds_default_plan_from_subdirectory() {
    let repo = TestRepo::new();
    repo.phasewise().arg("init").assert().success();
    repo.write("sub/dir/.keep", "");
    repo.phasewise()
        .current_dir(repo.path().join("sub/dir"))
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("The plan is valid."));
}

#[test]
fn init_creates_plan_and_excludes_state_dir() {
    let repo = TestRepo::new();
    repo.phasewise()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("phasewise check"));
    assert!(repo.read("phasewise.md").starts_with("+++\n"));
    assert!(repo.read(".git/info/exclude").contains(".phasewise/"));
}

#[test]
fn second_init_leaves_plan_untouched_and_exclude_once() {
    let repo = TestRepo::new();
    repo.phasewise().arg("init").assert().success();
    repo.write("phasewise.md", "## Edited\n\nmine\n");
    repo.phasewise()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "phasewise.md already exists; left untouched.",
        ));
    assert_eq!(repo.read("phasewise.md"), "## Edited\n\nmine\n");
    let exclude = repo.read(".git/info/exclude");
    assert_eq!(exclude.lines().filter(|l| *l == ".phasewise/").count(), 1);
}

#[test]
fn init_works_in_repo_without_commits() {
    let repo = TestRepo::empty();
    repo.phasewise().arg("init").assert().success();
    assert!(repo.path().join("phasewise.md").exists());
}

#[cfg(unix)]
#[test]
fn init_refuses_a_plan_path_that_is_a_directory() {
    let repo = TestRepo::new();
    std::fs::create_dir(repo.path().join("phasewise.md")).unwrap();
    repo.phasewise()
        .arg("init")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("phasewise.md is a directory"))
        .stdout(predicate::str::contains("left untouched").not());
}

#[cfg(unix)]
#[test]
fn init_refuses_a_dangling_plan_symlink() {
    let repo = TestRepo::new();
    std::os::unix::fs::symlink("nowhere", repo.path().join("phasewise.md")).unwrap();
    repo.phasewise()
        .arg("init")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("points nowhere"));
    assert!(!repo.path().join("nowhere").exists());
}

#[test]
fn init_outside_repo_exits_1() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("phasewise"))
        .current_dir(dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CEILING_DIRECTORIES",
            dir.path().parent().unwrap_or(dir.path()),
        )
        .arg("init")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Not inside a git repository"));
}

#[test]
fn check_notes_that_codex_phases_do_not_count_against_the_limit() {
    let repo = TestRepo::new();
    repo.write(
        "phasewise.md",
        "+++\nmax_cost_usd = 2\n+++\n\n## A\n\nx\n\n```phasewise\nagent = \"codex\"\n```\n",
    );
    repo.phasewise()
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("Spending limit: $2.00"))
        .stdout(predicate::str::contains("Codex reports no cost"));
    repo.write("phasewise.md", "+++\nmax_cost_usd = 2\n+++\n\n## A\n\nx\n");
    repo.phasewise()
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("Codex reports no cost").not());
}
