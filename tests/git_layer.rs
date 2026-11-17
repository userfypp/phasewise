mod common;

use common::TestRepo;
use phasewise::git::{Embedded, Git, checkpoint_message};

fn git_for(repo: &TestRepo) -> Git {
    repo.git_handle()
}

#[test]
fn version_basic_queries_and_branches() {
    assert!(Git::version().unwrap().starts_with("git version"));
    let repo = TestRepo::new();
    let git = git_for(&repo);
    assert!(git.has_commits().unwrap());
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("main"));
    let head = git.head_sha().unwrap();
    assert_eq!(head.len(), 40);
    assert_eq!(git.rev_parse("main").unwrap(), head);
    assert!(git.rev_parse("no-such-rev").is_err());
    assert!(git.branch_exists("main").unwrap());
    assert!(!git.branch_exists("other").unwrap());
    assert!(git.is_clean().unwrap());
}

#[test]
fn empty_repo_has_no_commits() {
    let repo = TestRepo::empty();
    let git = git_for(&repo);
    assert!(!git.has_commits().unwrap());
}

#[test]
fn detached_head_has_no_branch() {
    let repo = TestRepo::new();
    repo.git(&["checkout", "--detach"]);
    assert_eq!(git_for(&repo).current_branch().unwrap(), None);
}

#[test]
fn errors_include_git_args_and_stderr() {
    let repo = TestRepo::new();
    let err = git_for(&repo)
        .switch("missing-branch")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("git switch --no-guess missing-branch"),
        "{err}"
    );
    assert!(err.contains("missing-branch"), "{err}");
}

#[test]
fn dirty_detection() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    repo.write("README.md", "changed\n");
    assert_eq!(git.dirty_paths().unwrap(), vec!["README.md"]);
    repo.git(&["add", "README.md"]);
    assert_eq!(git.dirty_paths().unwrap(), vec!["README.md"]);
    repo.write("dir/new.txt", "x\n");
    let mut paths = git.dirty_paths().unwrap();
    paths.sort();
    assert_eq!(paths, vec!["README.md", "dir/new.txt"]);
    assert!(!git.is_clean().unwrap());
}

#[test]
fn dirty_detection_handles_renames() {
    let repo = TestRepo::new();
    repo.git(&["mv", "README.md", "RENAMED.md"]);
    repo.write("extra.txt", "x\n");
    let mut paths = git_for(&repo).dirty_paths().unwrap();
    paths.sort();
    assert_eq!(paths, vec!["RENAMED.md", "extra.txt"]);
}

#[test]
fn run_branch_checkpoints_and_agent_commits() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    let main_before = repo.git_trim(&["rev-parse", "main"]);
    let base = git.head_sha().unwrap();

    git.create_branch("phasewise/test-run", &base).unwrap();
    assert_eq!(
        git.current_branch().unwrap().as_deref(),
        Some("phasewise/test-run")
    );
    assert!(git.branch_exists("phasewise/test-run").unwrap());

    // Phase 1 changes nothing: empty checkpoint.
    git.add_all().unwrap();
    let cp1 = git
        .commit(&checkpoint_message("test-run", 1, "First"), true)
        .unwrap();
    assert_ne!(cp1, base);
    let body = repo.git_trim(&["log", "-1", "--format=%B"]);
    assert_eq!(
        body,
        "phasewise: phase 1 — First\n\nPhasewise-Run: test-run\nPhasewise-Phase: 1"
    );
    assert_eq!(git.changed_file_count(&base, &cp1).unwrap(), 0);

    // Phase 2: the agent commits on its own, then phasewise checkpoints.
    repo.commit_file("agent.txt", "by agent\n", "agent commit");
    let agent_sha = git.head_sha().unwrap();
    repo.write("left.txt", "uncommitted\n");
    git.add_all().unwrap();
    let cp2 = git
        .commit(&checkpoint_message("test-run", 2, "Second"), false)
        .unwrap();
    let diff = git.diff(&cp1, &cp2).unwrap();
    assert!(diff.contains("agent.txt"), "{diff}");
    assert!(diff.contains("left.txt"), "{diff}");
    assert_eq!(
        repo.git_trim(&["merge-base", "--is-ancestor", &agent_sha, &cp2]),
        ""
    );

    // Going back to the base and returning to main.
    git.switch("main").unwrap();
    assert_eq!(repo.git_trim(&["rev-parse", "main"]), main_before);
}

#[test]
fn reset_and_clean_keep_phasewise_dir() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    assert!(git.ensure_excluded(".phasewise/").unwrap());
    let main_before = repo.git_trim(&["rev-parse", "main"]);
    let base = git.head_sha().unwrap();
    git.create_branch("phasewise/r", &base).unwrap();

    repo.write(".phasewise/runs/r/state.json", "{}\n");
    repo.write("README.md", "tracked change\n");
    repo.write("untracked/file.txt", "x\n");
    assert!(!git.is_clean().unwrap());

    git.reset_hard(&base).unwrap();
    git.clean_untracked().unwrap();

    assert_eq!(repo.read("README.md"), "# test repo\n");
    assert!(!repo.path().join("untracked").exists());
    assert_eq!(repo.read(".phasewise/runs/r/state.json"), "{}\n");
    assert!(git.is_clean().unwrap());

    git.switch("main").unwrap();
    assert_eq!(repo.git_trim(&["rev-parse", "main"]), main_before);
}

#[test]
fn phasewise_dir_is_never_committed() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    git.ensure_excluded(".phasewise/").unwrap();
    let main_before = repo.git_trim(&["rev-parse", "main"]);
    let base = git.head_sha().unwrap();
    git.create_branch("phasewise/r", &base).unwrap();

    repo.write(".phasewise/current", "r\n");
    repo.write("src/lib.rs", "// code\n");
    git.add_all().unwrap();
    git.commit("work", false).unwrap();

    let tree = repo.git(&["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(tree.lines().any(|l| l == "src/lib.rs"));
    assert!(!tree.contains(".phasewise"), "{tree}");

    git.switch("main").unwrap();
    assert_eq!(repo.git_trim(&["rev-parse", "main"]), main_before);
}

#[test]
fn phasewise_dir_survives_without_exclude_entry() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    repo.write(".phasewise/x", "keep\n");
    repo.write("other.txt", "junk\n");
    git.clean_untracked().unwrap();
    assert_eq!(repo.read(".phasewise/x"), "keep\n");
    assert!(!repo.path().join("other.txt").exists());

    repo.write("other.txt", "work\n");
    git.add_all().unwrap();
    let staged = repo.git(&["diff", "--cached", "--name-only"]);
    assert_eq!(staged.trim(), "other.txt");

    // Also fine when the directory does not exist.
    std::fs::remove_dir_all(repo.path().join(".phasewise")).unwrap();
    git.add_all().unwrap();
}

#[test]
fn numstat_counts_and_binary() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    let main_before = repo.git_trim(&["rev-parse", "main"]);
    let base = git.head_sha().unwrap();
    git.create_branch("phasewise/r", &base).unwrap();

    repo.write("README.md", "# test repo\nline 2\nline 3\n");
    repo.write("ñandú/nuevo.txt", "a\n");
    std::fs::write(repo.path().join("blob.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
    git.add_all().unwrap();
    let cp = git.commit("work", false).unwrap();

    let mut stats = git.numstat(&base, &cp).unwrap();
    stats.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(stats.len(), 3);
    assert_eq!(stats[0].path, "README.md");
    assert_eq!((stats[0].added, stats[0].removed), (Some(2), Some(0)));
    assert_eq!(stats[1].path, "blob.bin");
    assert_eq!((stats[1].added, stats[1].removed), (None, None));
    assert_eq!(stats[2].path, "ñandú/nuevo.txt");
    assert_eq!((stats[2].added, stats[2].removed), (Some(1), Some(0)));
    assert_eq!(git.changed_file_count(&base, &cp).unwrap(), 3);

    git.switch("main").unwrap();
    assert_eq!(repo.git_trim(&["rev-parse", "main"]), main_before);
}

#[test]
fn commits_that_would_be_lost_reports_only_unreferenced_user_commits() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    let base = git.head_sha().unwrap();
    git.create_branch("phasewise/test-run", &base).unwrap();

    repo.write("a.txt", "a\n");
    git.add_all().unwrap();
    let cp1 = git
        .commit(&checkpoint_message("test-run", 1, "First"), true)
        .unwrap();

    // Only phasewise's checkpoints above the target: nothing to lose.
    let keep = vec![cp1.clone()];
    assert!(
        git.commits_that_would_be_lost(&base, &keep)
            .unwrap()
            .is_empty()
    );

    // A commit made by hand, on no other ref: reported with its subject.
    repo.commit_file("mine.txt", "mine\n", "my work");
    let mine = git.head_sha().unwrap();
    let lost = git.commits_that_would_be_lost(&cp1, &keep).unwrap();
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].sha, mine);
    assert_eq!(lost[0].subject, "my work");

    // The same commit on another branch (or tag) is safe.
    repo.git(&["branch", "keep", &mine]);
    assert!(
        git.commits_that_would_be_lost(&cp1, &keep)
            .unwrap()
            .is_empty()
    );
    repo.git(&["branch", "-D", "keep"]);
    assert_eq!(
        git.commits_that_would_be_lost(&cp1, &keep).unwrap().len(),
        1
    );
    repo.git(&["tag", "keep-tag", &mine]);
    assert!(
        git.commits_that_would_be_lost(&cp1, &keep)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn checkpoint_message_format() {
    assert_eq!(
        checkpoint_message("20260101-000000-abcd", 3, "Add API"),
        "phasewise: phase 3 — Add API\n\nPhasewise-Run: 20260101-000000-abcd\nPhasewise-Phase: 3\n"
    );
}

/// A repository with one commit, to be a submodule or a nested repository.
fn other_repo() -> TestRepo {
    TestRepo::new()
}

#[test]
fn a_dirty_submodule_and_a_nested_repository_are_told_from_ordinary_changes() {
    let repo = TestRepo::new();
    let lib = other_repo();
    let url = lib.path().to_str().expect("utf-8 path").to_string();
    repo.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        &url,
        "libs",
    ]);
    repo.commit_all("add the submodule");
    let git = git_for(&repo);
    assert_eq!(git.embedded_repos().unwrap(), []);

    // Changes inside the submodule show as one dirty path of the outer repository.
    repo.write("libs/README.md", "changed inside\n");
    repo.git(&["-C", "libs", "add", "README.md"]);
    // A nested repository that the outer one does not track, and a plain file.
    let nested = repo.path().join("tools").join("x");
    std::fs::create_dir_all(&nested).expect("create nested dir");
    repo.git(&["-C", "tools/x", "init", "-q"]);
    repo.write("tools/x/f.txt", "f\n");
    repo.write("plain.txt", "plain\n");

    let found = git.embedded_repos().unwrap();
    assert!(
        found.contains(&Embedded::Submodule("libs".into())),
        "{found:?}"
    );
    assert!(
        found.contains(&Embedded::Nested("tools/x".into())),
        "{found:?}"
    );
    assert_eq!(found.len(), 2, "the plain file is not one: {found:?}");
}

fn configure_fake_difftool(repo: &TestRepo, cmd: &str) {
    repo.git(&["config", "diff.tool", "fake"]);
    repo.git(&["config", "difftool.fake.cmd", cmd]);
}

#[test]
fn difftool_runs_the_configured_tool_for_the_range() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    repo.commit_file("a.txt", "one\n", "add a");
    let first = git.head_sha().unwrap();
    repo.commit_file("a.txt", "two\n", "change a");
    let second = git.head_sha().unwrap();
    let out = tempfile::tempdir().unwrap();
    let log = out.path().join("seen");
    configure_fake_difftool(
        &repo,
        &format!("cat \"$LOCAL\" \"$REMOTE\" >> '{}'", log.display()),
    );
    git.difftool(&first, Some(&second)).unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\ntwo\n");

    // Without a second revision the working tree is the right-hand side.
    std::fs::remove_file(&log).unwrap();
    repo.write("a.txt", "three\n");
    git.difftool(&first, None).unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\nthree\n");
}

#[test]
fn difftool_failures_say_what_to_do() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    repo.commit_file("a.txt", "one\n", "add a");
    let first = git.head_sha().unwrap();
    repo.commit_file("a.txt", "two\n", "change a");
    // git exits 128 when the tool fails (the tool is not installed, say).
    configure_fake_difftool(&repo, "exit 3");
    let err = git.difftool(&first, Some("HEAD")).unwrap_err().to_string();
    assert!(err.contains("`git difftool` failed"), "{err}");
    assert!(err.contains("128"), "{err}");
    assert!(err.contains("diff.tool"), "{err}");
    assert!(err.contains("--tool-help"), "{err}");
}

#[test]
fn switch_does_not_bring_back_a_branch_from_its_remote_copy() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    let remote = tempfile::tempdir().expect("remote dir");
    let remote_path = remote.path().to_str().expect("utf-8 path").to_string();
    repo.git(&["clone", "-q", "--bare", ".", &remote_path]);
    repo.git(&["remote", "add", "origin", &remote_path]);
    repo.git(&["fetch", "-q", "origin"]);
    // `main` is renamed: only `origin/main` is left under that name.
    repo.git(&["branch", "-m", "main", "renamed"]);
    assert!(git.switch("main").is_err());
    assert!(!git.branch_exists("main").unwrap());
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("renamed"));
    assert!(git.is_ancestor("HEAD", "renamed").unwrap());
    assert!(
        !git.is_ancestor("0000000000000000000000000000000000000001", "renamed")
            .unwrap()
    );
}

#[test]
fn commits_with_lines_finds_only_the_commits_that_carry_every_line() {
    let repo = TestRepo::new();
    let git = git_for(&repo);
    let base = git.head_sha().unwrap();
    repo.write("a.txt", "1\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "ours\n\nTag-A: x\nTag-B: 2\n"]);
    let ours = git.head_sha().unwrap();
    repo.commit_file("b.txt", "2\n", "theirs\n\nTag-A: x\n");
    let both = ["Tag-A: x".to_string(), "Tag-B: 2".to_string()];
    assert_eq!(
        git.commits_with_lines(&base, "HEAD", &both).unwrap(),
        [ours]
    );
    assert!(
        git.commits_with_lines(&base, "HEAD", &[])
            .unwrap()
            .is_empty()
    );
    assert!(
        git.commits_with_lines("HEAD", &base, &both)
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn a_failing_post_checkout_hook_does_not_fail_a_switch_that_worked() {
    use std::os::unix::fs::PermissionsExt;
    let repo = TestRepo::new();
    let git = git_for(&repo);
    let hook = repo.path().join(".git/hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'git-lfs was not found' >&2\nexit 2\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let head = git.head_sha().unwrap();
    git.create_branch("phasewise/x", &head).unwrap();
    assert_eq!(
        git.current_branch().unwrap().as_deref(),
        Some("phasewise/x")
    );
    git.switch("main").unwrap();
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("main"));
    // A switch that really fails is still an error.
    assert!(git.switch("missing-branch").is_err());
}
