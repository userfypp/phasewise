//! `ops::commit_plan` and the refusal to start a run from a run branch.

mod common;

use std::path::Path;

use common::TestRepo;
use phasewise::ops::commit_plan_in;

const PLAN: &str = "# Demo\n\n## One\n\nDo one.\n";

fn commit(repo: &TestRepo, plan: &str) -> anyhow::Result<String> {
    commit_plan_in(&repo.git_handle(), repo.path(), Path::new(plan))
}

fn error(repo: &TestRepo, plan: &str) -> String {
    commit(repo, plan).expect_err("must fail").to_string()
}

fn head_files(repo: &TestRepo) -> String {
    repo.git_trim(&["ls-tree", "-r", "--name-only", "HEAD"])
}

#[test]
fn commits_a_plan_at_the_root() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    let sha = commit(&repo, "phasewise.md").expect("commit");
    assert_eq!(sha, repo.git_trim(&["rev-parse", "--short", "HEAD"]));
    assert!(head_files(&repo).contains("phasewise.md"));
}

#[test]
fn commits_a_plan_in_a_subdirectory_and_with_spaces() {
    let repo = TestRepo::new();
    repo.write("docs/my plan.md", PLAN);
    repo.write("other.txt", "unrelated");
    commit(&repo, "docs/my plan.md").expect("commit");
    let files = head_files(&repo);
    assert!(files.contains("docs/my plan.md"), "{files}");
    assert!(!files.contains("other.txt"), "{files}");
}

#[cfg(unix)]
#[test]
fn commits_through_a_symlinked_path() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    let links = tempfile::tempdir().expect("tempdir");
    let link = links.path().join("link");
    std::os::unix::fs::symlink(repo.path(), &link).expect("symlink");
    let git = phasewise::git::Git::discover(&link)
        .expect("discover")
        .with_env(common::isolated_git_env());
    commit_plan_in(&git, &link, &link.join("phasewise.md")).expect("commit");
    assert!(head_files(&repo).contains("phasewise.md"));
}

#[test]
fn refuses_a_plan_outside_the_repository() {
    let repo = TestRepo::new();
    let outside = tempfile::tempdir().expect("tempdir");
    let plan = outside.path().join("plan.md");
    std::fs::write(&plan, PLAN).expect("write");
    let err = commit_plan_in(&repo.git_handle(), repo.path(), &plan)
        .expect_err("must fail")
        .to_string();
    assert!(err.contains("is outside the repository"), "{err}");
}

#[test]
fn refuses_a_missing_plan() {
    let repo = TestRepo::new();
    let err = error(&repo, "nope.md");
    assert!(err.contains("nope.md does not exist"), "{err}");
    assert!(err.contains("write a plan first"), "{err}");
}

#[test]
fn refuses_a_plan_that_is_already_committed() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    repo.commit_all("add plan");
    let err = error(&repo, "phasewise.md");
    assert!(err.contains("already committed"), "{err}");
}

#[test]
fn refuses_an_ignored_plan() {
    let repo = TestRepo::new();
    repo.write(".gitignore", "phasewise.md\n");
    repo.commit_all("ignore");
    repo.write("phasewise.md", PLAN);
    let err = error(&repo, "phasewise.md");
    assert!(err.contains("is ignored by git"), "{err}");
    assert!(err.contains("git add -f phasewise.md"), "{err}");
}

#[test]
fn refuses_a_detached_head() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    repo.git(&["switch", "--detach", "HEAD"]);
    let err = error(&repo, "phasewise.md");
    assert!(err.contains("HEAD is detached"), "{err}");
    assert!(err.contains("before you commit the plan"), "{err}");
}

#[test]
fn refuses_a_run_branch_without_a_run() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    repo.git(&["switch", "-c", "phasewise/old-run"]);
    let err = error(&repo, "phasewise.md");
    assert!(err.contains("You are on phasewise/old-run"), "{err}");
    assert!(err.contains("`git switch <branch>`"), "{err}");
}

#[test]
fn a_run_branch_of_the_current_run_names_the_original_branch() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    repo.commit_all("add plan");
    let out = repo
        .phasewise()
        .args(["run"])
        .env(
            "PHASEWISE_CLAUDE_BIN",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh"),
        )
        .output()
        .expect("run phasewise");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let branch = repo.git_trim(&["branch", "--show-current"]);
    assert!(branch.starts_with("phasewise/"), "{branch}");
    repo.write("other.md", PLAN);
    let err = error(&repo, "other.md");
    assert!(err.contains(&format!("You are on {branch}")), "{err}");
    assert!(err.contains("`git switch main`"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_failing_hook_reports_its_output() {
    use std::os::unix::fs::PermissionsExt;
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    let hook = repo.path().join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().expect("parent")).expect("hooks dir");
    std::fs::write(&hook, "#!/bin/sh\necho 'hook says no' >&2\nexit 1\n").expect("hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let err = error(&repo, "phasewise.md");
    assert!(err.contains("hook says no"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_hook_that_changes_the_plan_after_staging_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    let hook = repo.path().join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().expect("parent")).expect("hooks dir");
    std::fs::write(&hook, "#!/bin/sh\necho '<!-- fmt -->' >> phasewise.md\n").expect("hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let err = error(&repo, "phasewise.md");
    assert!(err.contains("a git hook changed it afterwards"), "{err}");
    assert!(err.contains("git add phasewise.md"), "{err}");
}

#[test]
fn start_refuses_a_run_branch() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", PLAN);
    repo.commit_all("add plan");
    repo.git(&["switch", "-c", "phasewise/old-run"]);
    let out = repo
        .phasewise()
        .args(["run"])
        .env(
            "PHASEWISE_CLAUDE_BIN",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh"),
        )
        .output()
        .expect("run phasewise");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("You are on phasewise/old-run"), "{stderr}");
    assert!(stderr.contains("before you start a run"), "{stderr}");
    assert_eq!(
        repo.git_trim(&["branch", "--show-current"]),
        "phasewise/old-run"
    );
}
