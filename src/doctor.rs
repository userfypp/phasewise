//! `phasewise doctor`: read-only diagnosis of git, the agents and the repository.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::agent::{Agent, agent_for};
use crate::git::Git;
use crate::plan::{self, AgentKind};
use crate::state::{PhaseStatus, PhasewiseDir, RunPaths, RunState, RunStatus, lock_holder};

/// A path with the home directory written as `~`.
fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    crate::display::path_with_home(home.as_deref(), path)
}

const PLAN_FILE: &str = "phasewise.md";
const EXCLUDE_PATTERN: &str = ".phasewise/";

/// What doctor found: the text to print and whether the tools are usable.
#[derive(Debug)]
pub struct Report {
    pub lines: Vec<String>,
    pub ok: bool,
}

/// Runs every check. Never modifies anything.
pub fn run(cwd: &Path) -> Report {
    let mut lines = vec!["Tools".to_string()];
    let git_ok = match Git::version() {
        Ok(version) => {
            let path = resolve_program(OsStr::new("git"))
                .map_or_else(String::new, |p| format!("  [{}]", tilde(&p)));
            lines.push(format!("  ✓ {:<7}{version}{path}", "git"));
            true
        }
        Err(err) => {
            lines.push(format!("  ✗ {:<7}{err}", "git"));
            false
        }
    };
    let mut agents_found = 0;
    let mut found_kinds = Vec::new();
    for (kind, name, var) in [
        (AgentKind::Claude, "claude", "PHASEWISE_CLAUDE_BIN"),
        (AgentKind::Codex, "codex", "PHASEWISE_CODEX_BIN"),
    ] {
        let found = agent_lines(agent_for(kind).as_ref(), name, var, &mut lines);
        agents_found += usize::from(found);
        if found {
            found_kinds.push(kind);
        }
    }

    lines.push(String::new());
    lines.push("Repository".to_string());
    let repository_start = lines.len();
    let mut plan_agents_ok = true;
    if git_ok {
        plan_agents_ok = repository_lines(cwd, &found_kinds, &mut lines);
    } else {
        lines.push("  ✗ cannot check the repository without git".to_string());
    }

    let repository_ok = !lines[repository_start..]
        .iter()
        .any(|line| line.starts_with("  ✗"));
    let ok = git_ok && agents_found > 0 && plan_agents_ok && repository_ok;
    lines.push(String::new());
    lines.push(
        if !git_ok {
            "Not ready: git is missing. Install git and make sure it is on your PATH."
        } else if agents_found == 0 {
            "Not ready: neither claude nor codex was found. Install one of them or set PHASEWISE_CLAUDE_BIN / PHASEWISE_CODEX_BIN."
        } else if !plan_agents_ok {
            "Not ready: the plan needs an agent that was not found. Install it or set its PHASEWISE_*_BIN variable."
        } else if !repository_ok {
            "Not ready: fix the ✗ lines in the repository section before `phasewise run`."
        } else {
            "Tools are ready and the repository is in order."
        }
        .to_string(),
    );
    Report { lines, ok }
}

/// Pushes the lines of one agent and returns whether it was found. A login that
/// is not confirmed is a warning, not a failure: an API key or a cloud
/// provider can work without it.
fn agent_lines(agent: &dyn Agent, name: &str, var: &str, lines: &mut Vec<String>) -> bool {
    let (line, found) = agent_line(agent, name, var);
    lines.push(line);
    if found && agent.logged_in() == Some(false) {
        lines.push(format!(
            "    ! login not confirmed: run `{}` (ignore this if you sign in with an API key or a cloud provider)",
            agent.login_hint()
        ));
    }
    found
}

fn agent_line(agent: &dyn Agent, name: &str, var: &str) -> (String, bool) {
    match agent.version() {
        Ok(version) => {
            let program = agent.program();
            let path = resolve_program(&program)
                .map_or_else(|| program.to_string_lossy().into_owned(), |p| tilde(&p));
            let via = if std::env::var_os(var).is_some_and(|v| !v.is_empty()) {
                format!(" ({var} in effect)")
            } else {
                String::new()
            };
            (format!("  ✓ {name:<7}{version}  [{path}]{via}"), true)
        }
        Err(err) => (format!("  ✗ {name:<7}{err}"), false),
    }
}

/// The program as given when it contains `/`, else the first executable
/// file on `PATH`.
fn resolve_program(program: &OsStr) -> Option<PathBuf> {
    if program.to_string_lossy().contains('/') {
        return Some(PathBuf::from(program));
    }
    let path = std::env::var_os("PATH")?;
    path.to_string_lossy()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Pushes the repository lines. Returns false when the plan has a phase whose
/// agent (other than the plan's own, which is the run's default) was not found.
fn repository_lines(cwd: &Path, found: &[AgentKind], lines: &mut Vec<String>) -> bool {
    let mut plan_agents_ok = true;
    let git = match Git::discover(cwd) {
        Ok(git) => git,
        Err(_) => {
            lines.push(
                "  ✗ not inside a git repository; run `git init` (then commit) here".to_string(),
            );
            return true;
        }
    };
    lines.push(format!("  ✓ git repository at {}", tilde(git.root())));

    match git.has_commits() {
        Ok(true) => lines.push("  ✓ has commits".to_string()),
        Ok(false) => lines.push("  ✗ no commits yet; make an initial commit".to_string()),
        Err(err) => lines.push(format!("  ✗ cannot read the history: {err}")),
    }
    match git.current_branch() {
        Ok(Some(branch)) => lines.push(format!("  ✓ on branch {branch}")),
        Ok(None) => {
            lines.push("  ✗ detached HEAD; switch to a branch with `git switch`".to_string())
        }
        Err(err) => lines.push(format!("  ✗ cannot read the branch: {err}")),
    }
    match git.dirty_paths() {
        Ok(paths) => {
            let count = paths
                .iter()
                .filter(|p| !p.starts_with(".phasewise/"))
                .count();
            if count == 0 {
                lines.push("  ✓ working tree clean".to_string());
            } else {
                lines.push(format!(
                    "  ✗ working tree has {count} uncommitted path(s); commit or stash them"
                ));
            }
        }
        Err(err) => lines.push(format!("  ✗ cannot read the working tree: {err}")),
    }
    match git.is_excluded(EXCLUDE_PATTERN) {
        Ok(true) => lines.push(format!("  ✓ {EXCLUDE_PATTERN} is in the git exclude file")),
        Ok(false) => lines.push(format!(
            "  ✓ {EXCLUDE_PATTERN} will be added to the git exclude file by `phasewise init`/`run`"
        )),
        Err(err) => lines.push(format!("  ✗ cannot read the git exclude file: {err}")),
    }

    let plan_path = git.root().join(PLAN_FILE);
    if !plan_path.is_file() {
        lines.push(format!(
            "  ✗ {PLAN_FILE} not found at the repository root; run `phasewise init`"
        ));
    } else {
        match plan::load(&plan_path) {
            Ok(plan) => {
                lines.push(format!(
                    "  ✓ {PLAN_FILE} is valid ({})",
                    crate::display::phase_count(plan.phases.len())
                ));
                if !found.contains(&plan.config.agent) {
                    let kind = plan.config.agent;
                    let var = match kind {
                        AgentKind::Claude => "PHASEWISE_CLAUDE_BIN",
                        AgentKind::Codex => "PHASEWISE_CODEX_BIN",
                    };
                    lines.push(format!(
                        "  ! the plan's agent is {kind}, which was not found; install it, set {var}, or run with `--agent`"
                    ));
                }
                for kind in [AgentKind::Claude, AgentKind::Codex] {
                    let phases: Vec<String> = plan
                        .phases
                        .iter()
                        .filter(|p| p.agent.is_some_and(|a| a == kind))
                        .map(|p| p.number.to_string())
                        .collect();
                    if !phases.is_empty() && !found.contains(&kind) {
                        plan_agents_ok = false;
                        let var = match kind {
                            AgentKind::Claude => "PHASEWISE_CLAUDE_BIN",
                            AgentKind::Codex => "PHASEWISE_CODEX_BIN",
                        };
                        lines.push(format!(
                            "  ✗ phase(s) {} use {kind}, which was not found; install it or set {var}",
                            phases.join(", ")
                        ));
                    }
                }
            }
            Err(err) => {
                let message = err.to_string();
                let first = message.lines().next().unwrap_or("invalid plan");
                lines.push(format!("  ✗ {PLAN_FILE} is invalid: {first}"));
            }
        }
    }
    lines.push(latest_run_line(&git));
    plan_agents_ok
}

/// Read-only: never saves a crash recovery.
fn latest_run_line(git: &Git) -> String {
    let run_id = match PhasewiseDir::new(git.root()).read_current() {
        Ok(Some(id)) => id,
        Ok(None) => return "  ✓ no run yet".to_string(),
        Err(err) => return format!("  ✗ cannot read the latest run: {err}"),
    };
    let paths = RunPaths::new(git.root(), &run_id);
    if !paths.state_file().exists() {
        return "  ✓ no run yet".to_string();
    }
    let state = match RunState::load(&paths) {
        Ok(state) => state,
        Err(err) => return format!("  ✗ cannot read run {run_id}: {err}"),
    };
    let status = match state.status {
        RunStatus::Active => "active",
        RunStatus::Completed => "completed",
        RunStatus::Aborted => "aborted",
    };
    let mut line = format!("  ✓ latest run {} ({status})", state.run_id);
    let alive = lock_holder(&paths).is_some();
    for phase in &state.phases {
        match phase.status {
            PhaseStatus::AwaitingReview => {
                line.push_str(&format!("; phase {} awaits review", phase.number));
            }
            PhaseStatus::Failed => line.push_str(&format!("; phase {} failed", phase.number)),
            PhaseStatus::Running | PhaseStatus::Verifying if !alive => line.push_str(&format!(
                "; phase {} interrupted (will be recovered on the next command)",
                phase.number
            )),
            PhaseStatus::Running | PhaseStatus::Verifying => {
                line.push_str(&format!("; phase {} is running", phase.number));
            }
            PhaseStatus::Pending | PhaseStatus::Done => {}
        }
    }
    line
}
