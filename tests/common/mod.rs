// Shared by several test crates; each one uses a different subset of the helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A temporary git repository with an isolated environment.
pub struct TestRepo {
    dir: TempDir,
}

/// Environment that hides the global and system git configuration.
pub fn isolated_git_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ]
}

/// A user settings directory that does not exist: the tests never read the
/// real one (a missing settings file means the defaults).
pub const NO_CONFIG_DIR: &str = "/nonexistent/phasewise-config";

fn isolate(cmd: &mut Command) {
    cmd.envs(isolated_git_env())
        .env_remove("PHASEWISE_CLAUDE_BIN")
        .env_remove("PHASEWISE_CODEX_BIN");
}

impl TestRepo {
    /// Repository on branch `main` with one commit (`README.md`).
    pub fn new() -> TestRepo {
        let repo = TestRepo::empty();
        repo.write("README.md", "# test repo\n");
        repo.commit_all("initial commit");
        repo
    }

    /// Repository with no commits.
    pub fn empty() -> TestRepo {
        let repo = TestRepo {
            dir: tempfile::tempdir().expect("create temp dir"),
        };
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "Test User"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo
    }

    /// A `Git` handle on this repository with the isolated environment.
    pub fn git_handle(&self) -> phasewise::git::Git {
        phasewise::git::Git::discover(self.path())
            .expect("discover repository")
            .with_env(isolated_git_env())
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, rel: &str, content: &str) {
        let path: PathBuf = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, content).expect("write file");
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path().join(rel)).expect("read file")
    }

    /// Runs git in the repository, asserts success and returns stdout.
    pub fn git(&self, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(self.path()).args(args);
        isolate(&mut cmd);
        let output = cmd.output().expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    pub fn commit_all(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", msg]);
    }

    /// The phasewise binary, run from the repository root.
    pub fn phasewise(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("phasewise"));
        cmd.current_dir(self.path())
            .envs(isolated_git_env())
            .env("PHASEWISE_CONFIG_DIR", NO_CONFIG_DIR)
            .env_remove("PHASEWISE_CLAUDE_BIN")
            .env_remove("PHASEWISE_CODEX_BIN");
        cmd
    }
}

impl TestRepo {
    /// Trimmed stdout of a git command.
    pub fn git_trim(&self, args: &[&str]) -> String {
        self.git(args).trim().to_string()
    }

    /// Commits `rel` with `content` as a commit made "by the agent".
    pub fn commit_file(&self, rel: &str, content: &str, msg: &str) {
        self.write(rel, content);
        self.commit_all(msg);
    }
}
