//! Read-only views of a run for the TUI: plain data plus the loaders that
//! build them from a [`Run`].

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::agent::AgentEvent;
use crate::agent::agent_for;
use crate::display::{
    EventLine, agent_event_line, failure_kind_label, failure_kind_words, first_line_short,
    model_name,
};
use crate::engine::phase_agent;
use crate::git::Git;
use crate::ops::Run;
use crate::plan::{self, AgentKind};
use crate::state::{PhaseStatus, RateLimits, RunStatus};
use jiff::Timestamp;

/// One line of the Log tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogLine {
    /// Heading of an attempt; `resumed` when it continued an earlier session.
    Attempt {
        number: u32,
        resumed: bool,
    },
    Event(EventLine),
}

impl LogLine {
    /// Text of an attempt heading, for example `── attempt 2 · resumed ──`.
    pub fn attempt_heading(number: u32, resumed: bool) -> String {
        if resumed {
            format!("── attempt {number} · resumed ──")
        } else {
            format!("── attempt {number} ──")
        }
    }
}

const NO_VERIFY: &str = "No verification output yet.";

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseView {
    pub number: usize,
    pub title: String,
    pub status: PhaseStatus,
    pub attempts: u32,
    pub failure: Option<String>,
    pub has_checkpoint: bool,
    /// Display name of the model of the last attempt: the one the agent
    /// reported, else the requested one. Effort is the requested one.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The phase's agent, when it is not the run's.
    pub agent: Option<AgentKind>,
    /// Cost of the phase so far, when the agent reported it.
    pub cost_usd: Option<f64>,
    /// When the phase started (for the elapsed time of a running one).
    pub started_at: Option<Timestamp>,
    /// When the phase ended, if it has.
    pub finished_at: Option<Timestamp>,
    /// Failure in plain words (`Checks failed`) and its full detail.
    pub failure_kind: Option<String>,
    pub failure_detail: Option<String>,
}

/// Diff of one phase. `files` holds the path and the added/removed line
/// counts (`None` for binary files).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffView {
    pub files: Vec<(String, Option<u64>, Option<u64>)>,
    pub text: String,
    pub untracked: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunView {
    pub run_id: String,
    pub plan_title: String,
    pub agent: String,
    pub run_branch: String,
    pub original_branch: String,
    pub active: bool,
    /// Every phase is approved: the run can be merged.
    pub completed: bool,
    pub phases: Vec<PhaseView>,
    /// The usage limits the agent last reported in this repository.
    pub limits: Option<RateLimits>,
}

pub fn load_run_view(run: &Run) -> RunView {
    let state = &run.state;
    let agent_of = |number| phase_agent(state, &run.plan, number);
    let phases = state
        .phases
        .iter()
        .map(|p| PhaseView {
            number: p.number,
            title: p.title.clone(),
            status: p.status,
            attempts: p.attempts,
            failure: p.failure.as_ref().map(|f| {
                format!(
                    "{}: {}",
                    failure_kind_label(f.kind),
                    first_line_short(&f.detail)
                )
            }),
            has_checkpoint: p.checkpoint_sha.is_some(),
            model: p
                .agent_model
                .as_ref()
                .or(p.model.as_ref())
                .map(|m| model_name(agent_of(p.number), m)),
            effort: p.effort.clone(),
            agent: Some(agent_of(p.number)).filter(|a| *a != state.agent),
            cost_usd: p.cost_usd,
            started_at: p.started_at,
            finished_at: p.finished_at,
            failure_kind: p
                .failure
                .as_ref()
                .map(|f| failure_kind_words(f.kind).to_string()),
            failure_detail: p.failure.as_ref().map(|f| f.detail.clone()),
        })
        .collect();
    RunView {
        run_id: state.run_id.clone(),
        plan_title: state
            .plan_title
            .clone()
            .unwrap_or_else(|| run.plan.title.clone().unwrap_or_default()),
        agent: state.agent.to_string(),
        run_branch: state.run_branch.clone(),
        original_branch: state.original_branch.clone(),
        active: state.status == RunStatus::Active,
        completed: state.status == RunStatus::Completed,
        phases,
        limits: RateLimits::load(run.git.root()).filter(|l| l.agent == state.agent),
    }
}

fn note(text: &str) -> DiffView {
    DiffView {
        note: Some(text.to_string()),
        ..DiffView::default()
    }
}

/// Diff of `phase`: between its checkpoints when it has one, otherwise the
/// working tree against the previous checkpoint plus the untracked files.
pub fn load_diff(run: &Run, phase: usize) -> Result<DiffView> {
    let state = &run.state;
    let Some(p) = state.phase(phase) else {
        return Ok(note("This phase does not exist."));
    };
    if p.status == PhaseStatus::Pending {
        return Ok(note("No changes yet."));
    }
    let Some(from) = state.checkpoint(phase - 1) else {
        return Ok(note(
            "The previous phase has no checkpoint to compare with.",
        ));
    };
    if let Some(to) = state.checkpoint(phase) {
        let files = run
            .git
            .numstat(from, to)?
            .into_iter()
            .map(|s| (s.path, s.added, s.removed))
            .collect::<Vec<_>>();
        let text = run.git.diff(from, to)?;
        let note = files
            .is_empty()
            .then(|| "This phase made no changes.".to_string());
        return Ok(DiffView {
            files,
            text,
            untracked: Vec::new(),
            note,
        });
    }
    match p.status {
        PhaseStatus::Failed | PhaseStatus::Running | PhaseStatus::Verifying => {
            let stats = run.git.numstat_worktree(from)?;
            let text = run.git.diff_worktree(from)?;
            let untracked: Vec<String> = run
                .git
                .dirty_paths()?
                .into_iter()
                .filter(|path| !stats.iter().any(|s| &s.path == path))
                .collect();
            let files: Vec<_> = stats
                .into_iter()
                .map(|s| (s.path, s.added, s.removed))
                .collect();
            let note =
                (files.is_empty() && untracked.is_empty()).then(|| "No changes yet.".to_string());
            Ok(DiffView {
                files,
                text,
                untracked,
                note,
            })
        }
        PhaseStatus::Pending | PhaseStatus::AwaitingReview | PhaseStatus::Done => {
            Ok(note("This phase has no checkpoint."))
        }
    }
}

/// Normalized agent events of every attempt of `phase`, with a heading line
/// per attempt. Missing attempt files are skipped. An attempt counts as
/// resumed when its session id was already seen in an earlier attempt.
pub fn load_log(run: &Run, phase: usize) -> Vec<LogLine> {
    let attempts = run.state.phase(phase).map_or(0, |p| p.attempts);
    let agent = agent_for(phase_agent(&run.state, &run.plan, phase));
    let mut lines = Vec::new();
    let mut seen_sessions: Vec<String> = Vec::new();
    for attempt in 1..=attempts {
        let Ok(raw) = std::fs::read(run.paths.agent_stdout_log(phase, attempt)) else {
            continue;
        };
        let raw = String::from_utf8_lossy(&raw);
        let mut parser = agent.parser(run.git.root());
        let heading = lines.len();
        lines.push(LogLine::Attempt {
            number: attempt,
            resumed: false,
        });
        let mut sessions = Vec::new();
        for line in raw.lines() {
            for event in parser.feed(line) {
                if let AgentEvent::SessionStarted { session_id, .. } = &event {
                    sessions.push(session_id.clone());
                }
                if let Some(text) = agent_event_line(&event) {
                    lines.push(LogLine::Event(text));
                }
            }
        }
        let resumed = sessions.iter().any(|id| seen_sessions.contains(id));
        lines[heading] = LogLine::Attempt {
            number: attempt,
            resumed,
        };
        seen_sessions.extend(sessions);
    }
    lines
}

/// Content of the verification log of `phase`.
pub fn load_verify(run: &Run, phase: usize) -> String {
    match std::fs::read(run.paths.verify_log(phase)) {
        Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).into_owned(),
        _ => NO_VERIFY.to_string(),
    }
}

/// One phase of the plan shown on the home screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePhase {
    pub number: usize,
    pub title: String,
    /// Phase-specific checks (the `verify` blocks of the phase).
    pub checks: usize,
    /// Effective model and effort: the phase's block, else the plan's front matter.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The phase's own agent, when it is not the plan's.
    pub agent: Option<AgentKind>,
}

/// A valid plan, summarised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSummary {
    pub title: Option<String>,
    pub agent: AgentKind,
    /// `model` / `effort` of the plan's front matter.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub phases: Vec<HomePhase>,
}

/// A problem found in the plan file; `line` is 0 when it has no line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanProblem {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanState {
    Missing,
    Valid(PlanSummary),
    Invalid(Vec<PlanProblem>),
}

/// One phase of the current run, as the home screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPhaseSummary {
    pub number: usize,
    pub title: String,
    pub status: PhaseStatus,
    /// When the phase started (for the elapsed time of a running one).
    pub started_at: Option<Timestamp>,
    /// The phase's agent, when it is not the run's.
    pub agent: Option<AgentKind>,
}

/// The current run of the repository, of any status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub id: String,
    pub status: RunStatus,
    pub run_branch: String,
    pub original_branch: String,
    pub phases: Vec<RunPhaseSummary>,
}

impl RunSummary {
    pub fn active(&self) -> bool {
        self.status == RunStatus::Active
    }
}

/// What the home screen shows about the repository and its plan.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeData {
    /// Repository root.
    pub root: PathBuf,
    /// Name of the repository directory.
    pub repo_name: String,
    /// Current branch, or `detached HEAD`.
    pub branch: String,
    /// The plan file at the repository root.
    pub plan_path: PathBuf,
    pub plan: PlanState,
    /// The plan file has changes that are not committed.
    pub plan_uncommitted: bool,
    /// The plan file as written in the terminal phasewise was started from
    /// (relative to the current directory, e.g. `../phasewise.md`).
    pub plan_rel: String,
    /// The current run (any status), if the repository has one.
    pub run: Option<RunSummary>,
    /// The usage limits the agent last reported in this repository.
    pub limits: Option<RateLimits>,
}

/// Loads the home data for the repository that contains `cwd`.
pub fn load_home(cwd: &Path) -> Result<HomeData> {
    load_home_in(&Git::discover(cwd)?, cwd)
}

/// Like [`load_home`], with the repository handle given by the caller (the
/// directory is the repository root).
pub fn load_home_from(git: &Git) -> Result<HomeData> {
    load_home_in(git, git.root())
}

/// `path` as seen from the directory `base`: `src/a.md`, `../phasewise.md`.
/// Both are made absolute and resolved when they exist.
pub fn relative_path(base: &Path, path: &Path) -> PathBuf {
    let resolve = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (base, path) = (resolve(base), resolve(path));
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = path.components().collect();
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for part in &target[common..] {
        out.push(part.as_os_str());
    }
    out
}

fn load_home_in(git: &Git, cwd: &Path) -> Result<HomeData> {
    let root = git.root().to_path_buf();
    let repo_name = root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let branch = git
        .current_branch()?
        .unwrap_or_else(|| "detached HEAD".to_string());
    let plan_path = root.join(plan::DEFAULT_PLAN_FILE);
    let plan_rel = relative_path(cwd, &plan_path).display().to_string();
    let plan = load_plan_state(&plan_path);
    let plan_uncommitted = plan != PlanState::Missing
        && git
            .dirty_paths()?
            .iter()
            .any(|p| p == plan::DEFAULT_PLAN_FILE);
    // A run that cannot be read is left out: the home screen must still open.
    let run = crate::ops::load_from_git(git)
        .ok()
        .flatten()
        .map(|run| RunSummary {
            id: run.state.run_id.clone(),
            status: run.state.status,
            run_branch: run.state.run_branch.clone(),
            original_branch: run.state.original_branch.clone(),
            phases: run
                .state
                .phases
                .iter()
                .map(|p| RunPhaseSummary {
                    number: p.number,
                    title: p.title.clone(),
                    status: p.status,
                    started_at: p.started_at,
                    agent: Some(phase_agent(&run.state, &run.plan, p.number))
                        .filter(|a| *a != run.state.agent),
                })
                .collect(),
        });
    let limits = RateLimits::load(&root);
    Ok(HomeData {
        root,
        repo_name,
        branch,
        plan_path,
        plan,
        plan_uncommitted,
        plan_rel,
        run,
        limits,
    })
}

fn load_plan_state(path: &Path) -> PlanState {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PlanState::Missing,
        Err(e) => {
            return PlanState::Invalid(vec![PlanProblem {
                line: 0,
                message: format!(
                    "Cannot read {}: {e}. Check the file and its permissions.",
                    path.display()
                ),
            }]);
        }
    };
    match plan::parse(&text) {
        Ok(plan) => PlanState::Valid(PlanSummary {
            title: plan.title.clone(),
            agent: plan.config.agent,
            model: plan.config.model.clone(),
            effort: plan.config.effort.clone(),
            phases: plan
                .phases
                .iter()
                .map(|p| {
                    // The plan's model belongs to the plan's agent only.
                    let own = p.agent.filter(|a| *a != plan.config.agent);
                    let inherit = own.is_none();
                    HomePhase {
                        number: p.number,
                        title: p.title.clone(),
                        checks: p.verify.len(),
                        model: p
                            .model
                            .clone()
                            .or_else(|| plan.config.model.clone().filter(|_| inherit)),
                        effort: p
                            .effort
                            .clone()
                            .or_else(|| plan.config.effort.clone().filter(|_| inherit)),
                        agent: own,
                    }
                })
                .collect(),
        }),
        Err(errors) => PlanState::Invalid(
            errors
                .into_iter()
                .map(|e| PlanProblem {
                    line: e.line,
                    message: e.message,
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_goes_up_from_a_subdirectory() {
        let rel = |base: &str, path: &str| {
            relative_path(Path::new(base), Path::new(path))
                .display()
                .to_string()
        };
        assert_eq!(
            rel("/nonexistent/r", "/nonexistent/r/phasewise.md"),
            "phasewise.md"
        );
        assert_eq!(
            rel("/nonexistent/r/src", "/nonexistent/r/phasewise.md"),
            "../phasewise.md"
        );
        assert_eq!(
            rel("/nonexistent/r/src/a/b", "/nonexistent/r/phasewise.md"),
            "../../../phasewise.md"
        );
    }
}
