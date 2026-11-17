//! The only module that runs `git`.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

/// A dirty path that is a repository of its own, see [`Git::embedded_repos`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Embedded {
    /// A submodule with changes inside it or a new commit.
    Submodule(String),
    /// A repository inside the tree that this one does not track.
    Nested(String),
}

/// A git repository, identified by its top-level directory.
#[derive(Debug, Clone)]
pub struct Git {
    root: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl Git {
    /// Finds the repository that contains `dir`.
    pub fn discover(dir: &Path) -> Result<Git> {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--show-toplevel"])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .context("Cannot run `git`. Install git and make sure it is on your PATH.")?;
        if !output.status.success() {
            return Err(anyhow!(
                "Not inside a git repository. Run `git init` or move into a repository."
            ));
        }
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(Git {
            root: PathBuf::from(root),
            env: Vec::new(),
        })
    }

    /// For tests: extra environment for every git call.
    #[doc(hidden)]
    pub fn with_env<K, V>(mut self, vars: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.env.extend(
            vars.into_iter()
                .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned())),
        );
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn exclude_path(&self) -> Result<PathBuf> {
        let out = self.run(&["rev-parse", "--git-path", "info/exclude"])?;
        let rel = PathBuf::from(out.trim());
        Ok(if rel.is_absolute() {
            rel
        } else {
            self.root.join(rel)
        })
    }

    /// Whether `pattern` is a line of the exclude file. Read-only.
    pub fn is_excluded(&self, pattern: &str) -> Result<bool> {
        let path = self.exclude_path()?;
        match fs::read_to_string(&path) {
            Ok(text) => Ok(text.lines().any(|l| l == pattern)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("Cannot read {}", path.display())),
        }
    }

    /// Whether git ignores `path` (relative to the root): `git check-ignore`
    /// exits 0 when it is ignored and 1 when it is not.
    pub fn is_ignored(&self, path: &str) -> Result<bool> {
        let args = ["check-ignore", "-q", "--", path];
        let output = self.output(&args, None)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(self.failure(&args, &output)),
        }
    }

    /// Makes sure `pattern` is a line of `.git/info/exclude`. Returns whether
    /// the file changed.
    pub fn ensure_excluded(&self, pattern: &str) -> Result<bool> {
        let path = self.exclude_path()?;
        let existing = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(e).with_context(|| format!("Cannot read {}", path.display()));
            }
        };
        if existing.lines().any(|l| l == pattern) {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        let mut addition = String::new();
        if !existing.is_empty() && !existing.ends_with('\n') {
            addition.push('\n');
        }
        addition.push_str(pattern);
        addition.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Cannot open {}", path.display()))?;
        file.write_all(addition.as_bytes())
            .with_context(|| format!("Cannot write {}", path.display()))?;
        Ok(true)
    }

    /// Version string reported by `git --version` (no repository needed).
    pub fn version() -> Result<String> {
        let output = Command::new("git")
            .arg("--version")
            .output()
            .context("Cannot run `git`. Install git and make sure it is on your PATH.")?;
        if !output.status.success() {
            return Err(anyhow!(
                "`git --version` failed: {}. Check your git installation.",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Whether `HEAD` points to a commit.
    pub fn has_commits(&self) -> Result<bool> {
        let output = self.output(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"], None)?;
        Ok(output.status.success())
    }

    /// Current branch name, or `None` when `HEAD` is detached.
    pub fn current_branch(&self) -> Result<Option<String>> {
        let output = self.output(&["symbolic-ref", "--quiet", "--short", "HEAD"], None)?;
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok(Some(name).filter(|n| !n.is_empty()))
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(self.failure(&["symbolic-ref", "--quiet", "--short", "HEAD"], &output))
        }
    }

    pub fn head_sha(&self) -> Result<String> {
        self.rev_parse("HEAD")
    }

    /// Resolves `rev` to a full commit SHA.
    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        let spec = format!("{rev}^{{commit}}");
        Ok(self
            .run(&["rev-parse", "--verify", &spec])?
            .trim()
            .to_string())
    }

    /// Paths with changes (modified, staged or untracked).
    pub fn dirty_paths(&self) -> Result<Vec<String>> {
        let out = self.run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        let mut paths = Vec::new();
        let mut entries = out.split('\0').filter(|e| !e.is_empty());
        while let Some(entry) = entries.next() {
            let (Some(status), Some(path)) = (entry.get(..2), entry.get(3..)) else {
                continue;
            };
            paths.push(path.to_string());
            // Renames and copies are followed by an entry with the original path.
            if status.contains('R') || status.contains('C') {
                entries.next();
            }
        }
        Ok(paths)
    }

    /// Dirty paths that are a repository of their own: git cannot commit or
    /// discard what changed inside them as part of this one.
    pub fn embedded_repos(&self) -> Result<Vec<Embedded>> {
        let out = self.run(&["status", "--porcelain=v2", "-z", "--untracked-files=all"])?;
        let mut found = Vec::new();
        let mut entries = out.split('\0').filter(|e| !e.is_empty());
        while let Some(entry) = entries.next() {
            match entry.as_bytes().first() {
                // `1 XY sub mH mI mW hH hI path`; a submodule's `sub` starts with `S`.
                Some(b'1') => {
                    let mut fields = entry.splitn(9, ' ');
                    if let (Some(sub), Some(path)) = (fields.nth(2), fields.nth(5))
                        && sub.starts_with('S')
                    {
                        found.push(Embedded::Submodule(path.to_string()));
                    }
                }
                // Renames carry a score and are followed by the original path.
                Some(b'2') => {
                    entries.next();
                }
                // A repository nested in the tree is listed as a directory.
                Some(b'?') => {
                    if let Some(path) = entry.get(2..).and_then(|p| p.strip_suffix('/')) {
                        found.push(Embedded::Nested(path.to_string()));
                    }
                }
                _ => {}
            }
        }
        Ok(found)
    }

    /// What to do about a dirty path that is a repository of its own, which
    /// phasewise can neither commit nor discard (`rollback` leaves it alone),
    /// ready to follow an error message. Empty when there is none.
    pub fn embedded_repo_advice(&self) -> String {
        match self.embedded_repos().unwrap_or_default().first() {
            Some(Embedded::Submodule(path)) => format!(
                " `{path}` is a submodule with changes inside it, which phasewise cannot commit \
or discard: commit or discard them inside it (`git -C {path} status`), and if you commit them \
there, commit the updated `{path}` in this repository too. Then run this again."
            ),
            Some(Embedded::Nested(path)) => format!(
                " `{path}/` is a git repository inside this one, which phasewise cannot commit or \
discard: move it out of the repository or add it to `.git/info/exclude`, then run this again."
            ),
            None => String::new(),
        }
    }

    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.dirty_paths()?.is_empty())
    }

    pub fn branch_exists(&self, name: &str) -> Result<bool> {
        let reference = format!("refs/heads/{name}");
        let args = ["show-ref", "--verify", "--quiet", reference.as_str()];
        let output = self.output(&args, None)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(self.failure(&args, &output)),
        }
    }

    /// Creates `name` at `start_sha` and switches to it.
    pub fn create_branch(&self, name: &str, start_sha: &str) -> Result<()> {
        self.run(&["switch", "-c", name, start_sha]).map(|_| ())
    }

    pub fn switch(&self, branch: &str) -> Result<()> {
        self.run(&["switch", branch]).map(|_| ())
    }

    /// Stages everything except phasewise's own `.phasewise/` directory, even
    /// if it is not excluded in `.git/info/exclude`. (A pathspec exclude makes
    /// `git add` fail when the directory is ignored, so it is unstaged after.)
    pub fn add_all(&self) -> Result<()> {
        self.run(&["add", "-A"])?;
        self.run(&["reset", "-q", "--", ".phasewise"]).map(|_| ())
    }

    /// Checks that git knows who the author and committer are, so checkpoint
    /// commits will not fail later.
    pub fn check_identity(&self) -> Result<()> {
        for var in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
            let output = self.output(&["var", var], None)?;
            if !output.status.success() {
                return Err(anyhow!(
                    "git does not know who you are, so phasewise cannot create checkpoint commits. \
Set it with `git config user.name \"Your Name\"` and `git config user.email you@example.com` \
(add --global for every repository), then run phasewise again."
                ));
            }
        }
        Ok(())
    }

    /// Commits the index with `message` and returns the new `HEAD` SHA.
    pub fn commit(&self, message: &str, allow_empty: bool) -> Result<String> {
        let mut args = vec!["commit", "-q", "--no-verify", "--no-gpg-sign"];
        if allow_empty {
            args.push("--allow-empty");
        }
        args.extend(["-F", "-"]);
        let output = self.output(&args, Some(message))?;
        if !output.status.success() {
            return Err(self.failure(&args, &output));
        }
        self.head_sha()
    }

    /// Commits only `path` (relative to the repository root) with `message`
    /// and returns the short SHA of the new `HEAD`. This is the user's own
    /// commit: their hooks and signing apply.
    pub fn commit_file(&self, path: &str, message: &str) -> Result<String> {
        // A path is a path, not a pattern: without `--literal-pathspecs`, the
        // plan `plan[v2].md` or `a*.md` would also add and commit its neighbours.
        self.run(&["--literal-pathspecs", "add", "--", path])?;
        let args = ["--literal-pathspecs", "commit", "-q", "-F", "-", "--", path];
        let output = self.output(&args, Some(message))?;
        if !output.status.success() {
            return Err(self.failure(&args, &output));
        }
        Ok(self
            .run(&["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string())
    }

    /// `git merge --squash <branch>`: applies the changes of `branch` to the
    /// index and the working tree without committing. On a conflict it
    /// returns the conflicting paths and leaves the tree as git left it; the
    /// caller restores it. Any other failure is an error.
    pub fn merge_squash(&self, branch: &str) -> Result<SquashResult> {
        let args = ["merge", "--squash", branch];
        let output = self.output(&args, None)?;
        if output.status.success() {
            return Ok(SquashResult::Clean);
        }
        if output.status.code() == Some(1) {
            let conflicts = self.conflicted_paths()?;
            if !conflicts.is_empty() {
                return Ok(SquashResult::Conflict(conflicts));
            }
        }
        Err(self.failure(&args, &output))
    }

    /// Paths with unresolved conflicts.
    fn conflicted_paths(&self) -> Result<Vec<String>> {
        let out = self.run(&["diff", "--name-only", "--diff-filter=U", "-z"])?;
        Ok(out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// True when the index differs from `HEAD`.
    pub fn has_staged_changes(&self) -> Result<bool> {
        let args = ["diff", "--cached", "--quiet"];
        let output = self.output(&args, None)?;
        match output.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(self.failure(&args, &output)),
        }
    }

    /// Commits the index with `message` and returns the short SHA of the new
    /// `HEAD`. This is the user's own commit: their hooks and signing apply.
    pub fn commit_staged(&self, message: &str) -> Result<String> {
        let args = ["commit", "-q", "-F", "-"];
        let output = self.output(&args, Some(message))?;
        if !output.status.success() {
            return Err(self.failure(&args, &output));
        }
        Ok(self
            .run(&["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string())
    }

    /// The commits after `from` up to and including `to`, newest first; none
    /// when `from` is not an ancestor of `to`.
    pub fn commits_between(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let args = ["merge-base", "--is-ancestor", from, to];
        let output = self.output(&args, None)?;
        match output.status.code() {
            Some(0) => {}
            Some(1) => return Ok(Vec::new()),
            _ => return Err(self.failure(&args, &output)),
        }
        let range = format!("{from}..{to}");
        let listed = self.run(&["rev-list", &range])?;
        Ok(listed
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Commits between `target` and `HEAD` that a reset to `target` would
    /// leave on no branch or tag, newest first. `keep` lists the commits that
    /// are phasewise's own (checkpoints): they are never reported. A commit
    /// reachable from a ref other than the current branch (and the stash) is
    /// not lost either.
    pub fn commits_that_would_be_lost(
        &self,
        target: &str,
        keep: &[String],
    ) -> Result<Vec<CommitInfo>> {
        let range = format!("{target}..HEAD");
        let listed = self.run(&["rev-list", &range])?;
        let keep: Vec<String> = keep.iter().filter_map(|k| self.rev_parse(k).ok()).collect();
        let candidates: Vec<&str> = listed
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty() && !keep.iter().any(|k| k == sha))
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let current = self
            .output(&["symbolic-ref", "--quiet", "HEAD"], None)?
            .stdout;
        let current = String::from_utf8_lossy(&current).trim().to_string();
        let mut lost = Vec::new();
        for sha in candidates {
            let refs = self.run(&["for-each-ref", "--contains", sha, "--format=%(refname)"])?;
            let elsewhere = refs
                .lines()
                .map(str::trim)
                .any(|r| !r.is_empty() && r != current && r != "refs/stash");
            if !elsewhere {
                let subject = self.run(&["log", "-1", "--format=%s", sha])?;
                lost.push(CommitInfo {
                    sha: sha.to_string(),
                    subject: subject.trim().to_string(),
                });
            }
        }
        Ok(lost)
    }

    pub fn reset_hard(&self, sha: &str) -> Result<()> {
        self.run(&["reset", "--hard", sha]).map(|_| ())
    }

    /// Removes untracked files and directories (ignored files are kept;
    /// `.phasewise/` at the repository root is never removed).
    pub fn clean_untracked(&self) -> Result<()> {
        self.run(&["clean", "-fd", "-e", "/.phasewise/"])
            .map(|_| ())
    }

    pub fn diff(&self, from: &str, to: &str) -> Result<String> {
        self.run(&["diff", "--no-color", "--no-ext-diff", from, to])
    }

    /// Tracked files against the working tree (untracked files are not included).
    pub fn diff_worktree(&self, from: &str) -> Result<String> {
        self.run(&["diff", "--no-color", "--no-ext-diff", from])
    }

    /// Shows the changes from `from` to `to` (the working tree when `to` is
    /// `None`) in the user's `git difftool`. The tool is interactive, so it
    /// inherits the terminal and nothing is captured. The tool's exit status is
    /// trusted: without it git ignores a tool that fails.
    pub fn difftool(&self, from: &str, to: Option<&str>) -> Result<()> {
        let mut args = vec!["difftool", "--no-prompt", "--trust-exit-code", from];
        args.extend(to);
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(&args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| "Cannot run `git difftool`")?;
        if status.success() {
            return Ok(());
        }
        Err(anyhow!(
            "`git difftool` failed ({status}). Configure a tool with `diff.tool` (see \
`git difftool --tool-help`) and make sure it is installed, or use the Diff view."
        ))
    }

    /// Per-file added/removed line counts between two commits.
    pub fn numstat(&self, from: &str, to: &str) -> Result<Vec<FileStat>> {
        self.numstat_args(&[from, to])
    }

    /// Per-file line counts of tracked files against the working tree.
    pub fn numstat_worktree(&self, from: &str) -> Result<Vec<FileStat>> {
        self.numstat_args(&[from])
    }

    fn numstat_args(&self, revs: &[&str]) -> Result<Vec<FileStat>> {
        let mut args = vec![
            "-c",
            "core.quotepath=off",
            "diff",
            "--numstat",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
        ];
        args.extend_from_slice(revs);
        let out = self.run(&args)?;
        let mut stats = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(3, '\t');
            if let (Some(added), Some(removed), Some(path)) =
                (parts.next(), parts.next(), parts.next())
            {
                stats.push(FileStat {
                    path: path.to_string(),
                    added: added.parse().ok(),
                    removed: removed.parse().ok(),
                });
            }
        }
        Ok(stats)
    }

    pub fn changed_file_count(&self, from: &str, to: &str) -> Result<usize> {
        Ok(self.numstat(from, to)?.len())
    }

    /// Runs `git -C <root> <args>` and returns stdout; failures include the
    /// arguments and stderr.
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = self.output(args, None)?;
        if !output.status.success() {
            return Err(self.failure(args, &output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The `index.lock` of this repository, asked to git (`--git-path` also
    /// works in linked worktrees); stderr is parsed only if that fails.
    fn lock_path(&self, stderr: &str) -> String {
        let asked = self
            .output_once(&["rev-parse", "--git-path", "index.lock"], None)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|path| !path.is_empty())
            .map(|path| {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                }
            });
        asked
            .or_else(|| lock_path_from_stderr(stderr).map(PathBuf::from))
            .unwrap_or_else(|| self.root.join(".git").join("index.lock"))
            .display()
            .to_string()
    }

    /// The error of a failed git command: a lock error when another process
    /// holds `index.lock`, the arguments and stderr otherwise.
    fn failure(&self, args: &[&str], output: &Output) -> anyhow::Error {
        if is_lock_contention(output) {
            let path = self.lock_path(&String::from_utf8_lossy(&output.stderr));
            return anyhow!(
                "`git {}` failed: another git process is using the repository ({path} exists). \
Wait for it to finish or close the tool that runs it (an editor's git integration, for example). \
If no git command is running, the lock is left over from a crash: delete {path} and try again.",
                args.join(" ")
            );
        }
        // Some commands (`git commit` with nothing to commit) explain themselves
        // on stdout and leave stderr empty.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = match stderr.trim() {
            "" => stdout.trim(),
            text => text,
        };
        anyhow!(
            "`git {}` failed: {detail}. Fix the problem in the repository and try again.",
            args.join(" ")
        )
    }

    /// Runs git with captured output and optional stdin; does not check the
    /// status. A command that fails because `index.lock` exists is run again a
    /// few times (git fails before changing anything in that case).
    fn output(&self, args: &[&str], stdin: Option<&str>) -> Result<Output> {
        let mut waits = LOCK_RETRY_WAITS_MS.iter();
        loop {
            let output = self.output_once(args, stdin)?;
            if output.status.success() || !is_lock_contention(&output) {
                return Ok(output);
            }
            match waits.next() {
                Some(ms) => std::thread::sleep(Duration::from_millis(*ms)),
                None => return Ok(output),
            }
        }
    }

    fn output_once(&self, args: &[&str], stdin: Option<&str>) -> Result<Output> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command
            .spawn()
            .with_context(|| format!("Cannot run `git {}`", args.join(" ")))?;
        if let Some(input) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            // git may exit before it reads its input (a held `index.lock`, for
            // one). Then the pipe is closed, and git's own exit status and
            // message, which the caller needs, matter more than the write.
            if let Err(error) = pipe.write_all(input.as_bytes())
                && error.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(error)
                    .with_context(|| format!("Cannot write to `git {}`", args.join(" ")));
            }
        }
        child
            .wait_with_output()
            .with_context(|| format!("Cannot run `git {}`", args.join(" ")))
    }
}

/// Waits between attempts when `index.lock` is held (about 3.5 s in total).
const LOCK_RETRY_WAITS_MS: [u64; 7] = [50, 100, 200, 400, 800, 1000, 1000];

/// True when git could not take `index.lock`: git's fatal exit code (128)
/// and the file name in stderr. Only the file name is matched, the rest of
/// git's message is translated in other locales. A failing hook (exit 1) that
/// prints the name is not lock contention.
fn is_lock_contention(output: &Output) -> bool {
    output.status.code() == Some(128)
        && String::from_utf8_lossy(&output.stderr).contains("index.lock")
}

/// The lock file named in git's message (`Unable to create '<path>': File
/// exists.` in English), whatever the language of the message. The path
/// starts at the first quote that is followed by `/`, so quotes inside the
/// path (`/Users/O'Brien/repo`) do not matter.
fn lock_path_from_stderr(stderr: &str) -> Option<String> {
    const NAME: &str = "index.lock";
    let end = stderr.find(NAME)? + NAME.len();
    let head = &stderr[..end];
    let start = head
        .char_indices()
        .find(|(i, c)| {
            matches!(c, '\'' | '"' | '‘' | '«' | '`') && head[i + c.len_utf8()..].starts_with('/')
        })
        .map(|(i, c)| i + c.len_utf8())?;
    Some(head[start..].to_string())
}

/// How `Git::merge_squash` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SquashResult {
    /// The changes were applied (possibly none).
    Clean,
    /// These paths conflict.
    Conflict(Vec<String>),
}

/// A commit as shown to the user: full SHA and subject line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub subject: String,
}

/// Added/removed line counts of one file; `None` for binary files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub added: Option<u64>,
    pub removed: Option<u64>,
}

/// Commit message of a phase checkpoint.
pub fn checkpoint_message(run_id: &str, phase: usize, title: &str) -> String {
    format!(
        "phasewise: phase {phase} — {title}\n\nPhasewise-Run: {run_id}\nPhasewise-Phase: {phase}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: [(&str, &str); 6] = [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_AUTHOR_NAME", "T"),
        ("GIT_AUTHOR_EMAIL", "t@example.com"),
        ("GIT_COMMITTER_NAME", "T"),
        ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ];

    fn repo() -> (tempfile::TempDir, Git) {
        let dir = tempfile::tempdir().expect("tempdir");
        let init = Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .envs(ENV)
            .output()
            .expect("git init");
        assert!(init.status.success());
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com")] {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["config", k, v])
                .envs(ENV)
                .output()
                .expect("git config");
            assert!(out.status.success());
        }
        let git = Git::discover(dir.path()).expect("discover").with_env(ENV);
        (dir, git)
    }

    fn lock(git: &Git) -> PathBuf {
        git.root().join(".git").join("index.lock")
    }

    #[test]
    fn add_all_retries_while_the_lock_is_held() {
        let (dir, git) = repo();
        fs::write(dir.path().join("a.txt"), "a").expect("write");
        let lock = lock(&git);
        fs::write(&lock, "").expect("lock");
        let to_remove = lock.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = fs::remove_file(to_remove);
        });
        git.add_all().expect("add_all should wait for the lock");
        handle.join().expect("join");
    }

    #[test]
    fn stale_lock_error_names_the_lock_and_how_to_fix_it() {
        let (dir, git) = repo();
        fs::write(dir.path().join("a.txt"), "a").expect("write");
        let lock = lock(&git);
        fs::write(&lock, "").expect("lock");
        let err = git.add_all().expect_err("must fail").to_string();
        assert!(err.contains("index.lock"), "{err}");
        assert!(err.contains("delete"), "{err}");
        assert!(err.contains("another git process"), "{err}");
    }

    #[test]
    fn a_failure_that_only_wrote_to_stdout_still_says_why() {
        let (dir, git) = repo();
        fs::write(dir.path().join("a.txt"), "a").expect("write");
        git.add_all().expect("add");
        git.commit("first", false).expect("first commit");
        // Nothing to commit: git explains on stdout and leaves stderr empty.
        let err = git
            .commit("second", false)
            .expect_err("must fail")
            .to_string();
        assert!(err.contains("nothing to commit"), "{err}");
        assert!(!err.contains("failed: ."), "{err}");
    }

    #[test]
    fn lock_path_is_parsed_in_any_language() {
        let english = "fatal: Unable to create '/r/.git/index.lock': File exists.\n";
        assert_eq!(
            lock_path_from_stderr(english).as_deref(),
            Some("/r/.git/index.lock")
        );
        let spanish = "fatal: No se pudo crear '/r/.git/index.lock': El archivo existe.";
        assert_eq!(
            lock_path_from_stderr(spanish).as_deref(),
            Some("/r/.git/index.lock")
        );
        let quote = "fatal: Unable to create '/Users/O'Brien/repo/.git/index.lock': File exists.";
        assert_eq!(
            lock_path_from_stderr(quote).as_deref(),
            Some("/Users/O'Brien/repo/.git/index.lock")
        );
        assert_eq!(lock_path_from_stderr("fatal: something else"), None);
    }

    #[test]
    fn lock_error_names_the_full_path_of_an_odd_directory() {
        let parent = tempfile::tempdir().expect("tempdir");
        let dir = parent.path().join("O'Brien repo");
        fs::create_dir(&dir).expect("mkdir");
        let init = Command::new("git")
            .args(["init", "-q"])
            .arg(&dir)
            .envs(ENV)
            .output()
            .expect("git init");
        assert!(init.status.success());
        let git = Git::discover(&dir).expect("discover").with_env(ENV);
        fs::write(dir.join("a.txt"), "a").expect("write");
        let lock = lock(&git);
        fs::write(&lock, "").expect("lock");
        let err = git.add_all().expect_err("must fail").to_string();
        let expected = lock.display().to_string();
        assert!(expected.contains("O'Brien repo"), "{expected}");
        assert!(err.contains(&format!("({expected} exists)")), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn failing_hook_that_mentions_the_lock_is_not_retried() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, git) = repo();
        fs::write(dir.path().join("plan.md"), "plan").expect("write");
        let counter = dir.path().join("hook-runs");
        let hook = dir.path().join(".git/hooks/pre-commit");
        fs::create_dir_all(hook.parent().expect("parent")).expect("hooks dir");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\necho run >> '{}'\necho 'index.lock hook says no' >&2\nexit 1\n",
                counter.display()
            ),
        )
        .expect("hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod");
        let err = git
            .commit_file("plan.md", "Add phasewise plan")
            .expect_err("hook fails")
            .to_string();
        let runs = fs::read_to_string(&counter).expect("counter");
        assert_eq!(runs.lines().count(), 1, "hook must run once");
        assert!(err.contains("index.lock hook says no"), "{err}");
        assert!(!err.contains("another git process"), "{err}");
    }

    #[test]
    fn is_ignored_tells_ignored_from_not_ignored() {
        let (dir, git) = repo();
        fs::write(dir.path().join(".gitignore"), "plan.md\n").expect("write");
        assert!(git.is_ignored("plan.md").expect("ignored"));
        assert!(!git.is_ignored("other.md").expect("not ignored"));
    }

    #[test]
    fn commit_file_commits_only_that_file() {
        let (dir, git) = repo();
        fs::write(dir.path().join("plan.md"), "plan").expect("write");
        fs::write(dir.path().join("other.txt"), "other").expect("write");
        git.run(&["add", "--", "other.txt"]).expect("stage other");
        git.commit_file("plan.md", "Add phasewise plan")
            .expect("commit_file");
        let tracked = git
            .run(&["ls-tree", "-r", "--name-only", "HEAD"])
            .expect("ls-tree");
        assert_eq!(tracked.trim(), "plan.md");
        let status = git.run(&["status", "--porcelain"]).expect("status");
        assert_eq!(status.trim(), "A  other.txt");
    }

    #[test]
    fn commits_between_lists_what_is_after_from_up_to_to() {
        let (dir, git) = repo();
        let commit = |name: &str| {
            fs::write(dir.path().join(name), name).expect("write");
            git.run(&["add", "--", name]).expect("add");
            git.run(&["commit", "-q", "-m", name]).expect("commit");
            git.head_sha().expect("sha")
        };
        let a = commit("a");
        let b = commit("b");
        let c = commit("c");
        assert_eq!(git.commits_between(&a, &c).expect("range"), [c.clone(), b]);
        assert!(git.commits_between(&c, &c).expect("empty").is_empty());
        // Not an ancestor (a commit on a side branch): nothing is claimed as
        // part of the range, though `rev-list d..c` would list b and c.
        git.run(&["switch", "-q", "-c", "side", &a])
            .expect("branch");
        let d = commit("d");
        assert!(git.commits_between(&d, &c).expect("diverged").is_empty());
    }

    #[test]
    fn commit_file_takes_the_path_literally() {
        let (dir, git) = repo();
        // `a*.md` is a pattern for git unless told otherwise.
        fs::write(dir.path().join("a*.md"), "plan").expect("write");
        fs::write(dir.path().join("ab.md"), "other").expect("write");
        git.commit_file("a*.md", "Add phasewise plan")
            .expect("commit_file");
        let tracked = git
            .run(&["ls-tree", "-r", "--name-only", "HEAD"])
            .expect("ls-tree");
        assert_eq!(tracked.trim(), "a*.md");
        let status = git.run(&["status", "--porcelain"]).expect("status");
        assert_eq!(status.trim(), "?? ab.md");
    }

    #[test]
    fn commit_file_does_not_take_a_staged_neighbour() {
        let (dir, git) = repo();
        fs::write(dir.path().join("plan[v2].md"), "plan").expect("write");
        fs::write(dir.path().join("plan2.md"), "other").expect("write");
        git.run(&["add", "--", "plan2.md"]).expect("stage other");
        git.commit_file("plan[v2].md", "Add phasewise plan")
            .expect("commit_file");
        let tracked = git
            .run(&["ls-tree", "-r", "--name-only", "HEAD"])
            .expect("ls-tree");
        assert_eq!(tracked.trim(), "plan[v2].md");
        let status = git.run(&["status", "--porcelain"]).expect("status");
        assert_eq!(status.trim(), "A  plan2.md");
    }

    #[test]
    fn commit_file_returns_the_short_sha() {
        let (dir, git) = repo();
        fs::write(dir.path().join("plan.md"), "plan").expect("write");
        let sha = git
            .commit_file("plan.md", "Add phasewise plan")
            .expect("commit");
        let expected = git
            .run(&["rev-parse", "--short", "HEAD"])
            .expect("rev-parse");
        assert_eq!(sha, expected.trim());
        assert!(!sha.is_empty());
    }
}
