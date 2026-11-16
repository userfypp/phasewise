//! Run-level operations shared by the CLI and the TUI. Never prints; progress
//! goes through `EngineEvent`s.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;

use crate::agent::{AgentEvent, LaunchOptions, agent_for};
use crate::engine::{
    AgentSession, EngineEvent, PhaseContext, PhaseSettings, phase_agent, run_agent_session,
    run_phase,
};
use crate::git::{Git, SquashResult};
use crate::plan::{self, AgentKind, Plan};
use crate::prompt;
use crate::state::{
    LockGuard, PhaseStatus, PhasewiseDir, RunPaths, RunState, RunStatus, load_locked,
    load_with_recovery, lock_holder, new_run_id, run_branch_name, write_atomic,
};

const KILL_GRACE: Duration = Duration::from_secs(5);
const MAX_DIRTY_LISTED: usize = 10;
const PHASE_TIMEOUT_ENV: &str = "PHASEWISE_PHASE_TIMEOUT_SECONDS";
const MAX_COST_ENV: &str = "PHASEWISE_MAX_COST_USD";
const PLAN_TIMEOUT_ENV: &str = "PHASEWISE_PLAN_TIMEOUT_SECONDS";
const PLAN_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// Repair rounds after the first answer of `phasewise plan`.
const MAX_PLAN_REPAIRS: u32 = 2;

/// A loaded run: repository, files, state and the plan copy it was started with.
pub struct Run {
    pub git: Git,
    pub paths: RunPaths,
    pub state: RunState,
    pub plan: Plan,
    /// Phases that were left `running`/`verifying` by a dead process and were
    /// marked `failed` (interrupted) while loading. The CLI reports them once.
    pub recovered: Vec<usize>,
}

/// What `abort` did, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortOutcome {
    /// The branch checked out now.
    pub current_branch: String,
    /// True when `abort` switched to the original branch.
    pub switched: bool,
    /// Commit holding uncommitted changes found on the run branch, if any.
    pub leftovers_sha: Option<String>,
}

/// What `merge` did, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// The branch the work was committed on (the run's original branch).
    pub branch: String,
    /// The run branch, which is kept.
    pub run_branch: String,
    /// Short SHA of the new commit; `None` when there was nothing to merge
    /// (the work is already in the branch).
    pub commit: Option<String>,
}

/// How `next` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextOutcome {
    /// A phase ran; this is how it ended (the last one of an `--auto` chain).
    Ran(PhaseStatus),
    /// The last phase was approved and the run is complete.
    Completed,
}

/// What `--model` / `--effort` ask for one setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Override {
    Set(String),
    /// `default`: remove the stored override.
    Clear,
}

/// The `--model` / `--effort` flags of `run`, `next` and `retry`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub model: Option<Override>,
    pub effort: Option<Override>,
}

impl Overrides {
    /// Validates the flag values; `default` means "clear the override".
    pub fn parse(model: Option<&str>, effort: Option<&str>) -> Result<Overrides> {
        let one = |value: Option<&str>, check: fn(&str) -> Result<(), String>, flag: &str| {
            value
                .map(|v| {
                    if v == "default" {
                        return Ok(Override::Clear);
                    }
                    check(v)
                        .map(|()| Override::Set(v.to_string()))
                        .map_err(|e| anyhow!("Invalid --{flag}: {e}."))
                })
                .transpose()
        };
        Ok(Overrides {
            model: one(model, plan::validate_model, "model")?,
            effort: one(effort, plan::validate_effort, "effort")?,
        })
    }

    /// Stores the given values in the run state; the caller saves it.
    pub fn apply(&self, state: &mut RunState) {
        let set = |slot: &mut Option<String>, value: &Option<Override>| match value {
            Some(Override::Set(v)) => *slot = Some(v.clone()),
            Some(Override::Clear) => *slot = None,
            None => {}
        };
        set(&mut state.model_override, &self.model);
        set(&mut state.effort_override, &self.effort);
    }
}

/// Starts a run: validates the plan and the repository, creates the run
/// branch and writes the run files. Nothing is written if a check fails.
pub fn start(
    cwd: &Path,
    plan_path: &Path,
    agent_override: Option<AgentKind>,
    overrides: &Overrides,
) -> Result<Run> {
    let plan = plan::load(plan_path)?;
    // Same rule as `next` and `retry`: a model belongs to one agent.
    let run_agent = agent_override.unwrap_or(plan.config.agent);
    let first_agent = match plan.phases.first() {
        Some(first) if run_agent == plan.config.agent => first.agent.unwrap_or(run_agent),
        _ => run_agent,
    };
    if matches!(overrides.model, Some(Override::Set(_))) && first_agent != run_agent {
        bail!(
            "Phase 1 runs with {first_agent}; --model applies only to the run's agent ({run_agent}) phases. Run without --model."
        );
    }
    let plan_text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("Cannot read {}", plan_path.display()))?;
    let git = Git::discover(cwd)?;
    refuse_if_active(&git)?;
    check_repository(&git)?;
    let original_branch = git
        .current_branch()?
        .ok_or_else(|| {
            anyhow!(
                "HEAD is detached. Switch to a branch (`git switch <branch>`), then run phasewise again."
            )
        })?;
    refuse_run_branch(&git, &original_branch, "start a run")?;

    git.check_identity()?;
    git.ensure_excluded(".phasewise/")?;
    let run_id = new_run_id(Timestamp::now());
    let run_branch = run_branch_name(&run_id);
    let base_sha = git.head_sha()?;
    git.create_branch(&run_branch, &base_sha)?;

    // From here on the repository is on the run branch: undo that if a later step fails.
    match write_run_files(
        &git,
        plan_path,
        &plan_text,
        &plan,
        agent_override,
        overrides,
        &run_id,
        &original_branch,
        &base_sha,
    ) {
        Ok(run) => Ok(run),
        Err(err) => {
            let _ = git.switch(&original_branch);
            Err(err.context(format!(
                "Branch {run_branch} was created and can be removed with `git branch -D {run_branch}` \
(phasewise never deletes branches)"
            )))
        }
    }
}

/// The steps of `start` after the branch exists, split out so `start` can undo
/// the branch switch when one of them fails.
// Same independent run fields as `RunState::new`; a parameter struct would only
// move the list to the single call site.
#[allow(clippy::too_many_arguments)]
fn write_run_files(
    git: &Git,
    plan_path: &Path,
    plan_text: &str,
    plan: &Plan,
    agent_override: Option<AgentKind>,
    overrides: &Overrides,
    run_id: &str,
    original_branch: &str,
    base_sha: &str,
) -> Result<Run> {
    let run_branch = run_branch_name(run_id);
    let paths = RunPaths::new(git.root(), run_id);
    write_atomic(&paths.plan_copy(), plan_text.as_bytes())?;
    let phases: Vec<(usize, String)> = plan
        .phases
        .iter()
        .map(|p| (p.number, p.title.clone()))
        .collect();
    let plan_path = plan_path
        .canonicalize()
        .unwrap_or_else(|_| plan_path.to_path_buf());
    let mut state = RunState::new(
        run_id,
        &plan_path,
        plan.title.clone(),
        agent_override.unwrap_or(plan.config.agent),
        original_branch,
        &run_branch,
        base_sha,
        &phases,
    );
    overrides.apply(&mut state);
    state.save(&paths)?;
    PhasewiseDir::new(git.root()).write_current(run_id)?;
    Ok(Run {
        git: git.clone(),
        paths,
        state,
        plan: plan.clone(),
        recovered: Vec::new(),
    })
}

fn refuse_if_active(git: &Git) -> Result<()> {
    let Some(run) = load_from_git(git)? else {
        return Ok(());
    };
    if run.state.status == RunStatus::Active {
        bail!(
            "A run is already active ({} on branch {}). Continue it with `phasewise next`, \
inspect it with `phasewise review`, or give it up with `phasewise abort`.",
            run.state.run_id,
            run.state.run_branch
        );
    }
    Ok(())
}

fn check_repository(git: &Git) -> Result<()> {
    if !git.has_commits()? {
        bail!(
            "The repository has no commits yet. Make an initial commit, then run phasewise again."
        );
    }
    if git.current_branch()?.is_none() {
        bail!(
            "HEAD is detached. Switch to a branch (`git switch <branch>`), then run phasewise again."
        );
    }
    let dirty = dirty_paths(git)?;
    if !dirty.is_empty() {
        let mut listed: Vec<String> = dirty
            .iter()
            .take(MAX_DIRTY_LISTED)
            .map(|p| format!("  {p}"))
            .collect();
        if dirty.len() > MAX_DIRTY_LISTED {
            listed.push(format!("  ... and {} more", dirty.len() - MAX_DIRTY_LISTED));
        }
        bail!(
            "The working tree has uncommitted changes:\n{}\nCommit or stash them, then run phasewise again.",
            listed.join("\n")
        );
    }
    Ok(())
}

/// Dirty paths, ignoring phasewise's own directory.
fn dirty_paths(git: &Git) -> Result<Vec<String>> {
    Ok(git
        .dirty_paths()?
        .into_iter()
        .filter(|p| !p.starts_with(".phasewise/"))
        .collect())
}

/// Commits the plan file alone (the user's hooks and signing apply) and
/// returns the short SHA of the new commit.
pub fn commit_plan(cwd: &Path, plan_path: &Path) -> Result<String> {
    let git = Git::discover(cwd)?;
    commit_plan_in(&git, cwd, plan_path)
}

/// `commit_plan` on a repository already found (tests give it an isolated
/// git environment).
pub fn commit_plan_in(git: &Git, cwd: &Path, plan_path: &Path) -> Result<String> {
    let absolute = if plan_path.is_absolute() {
        plan_path.to_path_buf()
    } else {
        cwd.join(plan_path)
    };
    if !absolute.exists() {
        bail!(
            "{} does not exist: write a plan first.",
            plan_path.display()
        );
    }
    // Resolve symlinks (for example /tmp on macOS) so the paths compare.
    let absolute = absolute.canonicalize().unwrap_or(absolute);
    let root = git
        .root()
        .canonicalize()
        .unwrap_or_else(|_| git.root().to_path_buf());
    let relative = absolute.strip_prefix(&root).map_err(|_| {
        anyhow!(
            "{} is outside the repository at {}. Move the plan inside the repository.",
            plan_path.display(),
            root.display()
        )
    })?;
    let relative = relative.to_string_lossy().into_owned();
    match git.current_branch()? {
        None => bail!(
            "HEAD is detached. Switch to a branch (`git switch <branch>`) before you commit the plan."
        ),
        Some(branch) => refuse_run_branch(git, &branch, "commit the plan")?,
    }
    if !git.dirty_paths()?.contains(&relative) {
        if git.is_ignored(&relative)? {
            bail!(
                "{} is ignored by git. Remove it from .gitignore (or .git/info/exclude), \
or commit it yourself with `git add -f {}`.",
                plan_path.display(),
                plan_path.display()
            );
        }
        bail!(
            "{} is already committed: there is nothing to commit.",
            plan_path.display()
        );
    }
    git.commit_file(&relative, "Add phasewise plan")
}

/// Fails when `branch` is the branch of a phasewise run: plans are committed
/// and runs started on the user's own branch. `action` completes "before you".
fn refuse_run_branch(git: &Git, branch: &str, action: &str) -> Result<()> {
    if !branch.starts_with("phasewise/") {
        return Ok(());
    }
    let original = load_from_git(git)
        .ok()
        .flatten()
        .filter(|run| run.state.run_branch == branch)
        .map_or_else(
            || "<branch>".to_string(),
            |run| run.state.original_branch.clone(),
        );
    bail!(
        "You are on {branch}, the branch of a phasewise run. Switch to your own branch \
(`git switch {original}`) before you {action}."
    )
}

/// When the repository is on the branch of a run that is over (completed or
/// aborted), switches back to the run's original branch. Does nothing in any
/// other case. The run branch is kept.
pub fn leave_finished_run(cwd: &Path) -> Result<()> {
    let git = Git::discover(cwd)?;
    let Some(run) = load_from_git(&git)? else {
        return Ok(());
    };
    if run.state.status == RunStatus::Active
        || git.current_branch()?.as_deref() != Some(run.state.run_branch.as_str())
    {
        return Ok(());
    }
    git.switch(&run.state.original_branch).with_context(|| {
        format!(
            "Cannot go back to {} (the run's original branch). Run `git switch {}` yourself",
            run.state.original_branch, run.state.original_branch
        )
    })
}

/// The latest run, whatever its status, with crash recovery applied.
pub fn load_current(cwd: &Path) -> Result<Option<Run>> {
    let git = Git::discover(cwd)?;
    load_from_git(&git)
}

/// Says what to do when the files of the current run cannot be read: without
/// this, every command fails and the only way out is a file the user was never
/// told about.
fn damaged_run(run_id: &str, error: anyhow::Error) -> anyhow::Error {
    anyhow!(
        "The files of run {run_id} cannot be read: {error:#}. If you do not need this run, \
delete `.phasewise/current` to forget it (its files stay in `.phasewise/runs/{run_id}/`) and \
start a new one."
    )
}

pub(crate) fn load_from_git(git: &Git) -> Result<Option<Run>> {
    let Some(run_id) = PhasewiseDir::new(git.root()).read_current()? else {
        return Ok(None);
    };
    let paths = RunPaths::new(git.root(), &run_id);
    if !paths.state_file().exists() {
        return Ok(None);
    }
    let before = RunState::load(&paths).map_err(|e| damaged_run(&run_id, e))?;
    let state = load_with_recovery(&paths)?;
    let recovered = recovered_phases(&before, &state);
    let plan = plan::load(&paths.plan_copy()).map_err(|e| damaged_run(&run_id, e))?;
    Ok(Some(Run {
        git: git.clone(),
        paths,
        state,
        plan,
        recovered,
    }))
}

/// The latest run, only if it is active.
pub fn load_active(cwd: &Path) -> Result<Option<Run>> {
    Ok(load_current(cwd)?.filter(|run| run.state.status == RunStatus::Active))
}

/// The run's spending limit: `PHASEWISE_MAX_COST_USD` over the plan's `max_cost_usd`.
pub fn spending_limit(run: &Run) -> Result<Option<f64>> {
    match std::env::var(MAX_COST_ENV) {
        Ok(value) => match value.trim().parse::<f64>() {
            Ok(limit) if limit.is_finite() && limit > 0.0 => Ok(Some(limit)),
            _ => bail!(
                "{MAX_COST_ENV} must be an amount in US dollars greater than 0, got `{value}`. \
Unset it or fix the value."
            ),
        },
        Err(_) => Ok(run.plan.config.max_cost_usd),
    }
}

/// Reads a timeout given in whole seconds by an environment variable. Zero is
/// refused, as it is for the plan's own timeouts: it would stop the agent at once.
fn parse_timeout_seconds(name: &str, value: &str) -> Result<Duration> {
    let secs: u64 = value.trim().parse().map_err(|_| {
        anyhow!(
            "{name} must be a whole number of seconds, got `{value}`. Unset it or fix the value."
        )
    })?;
    if secs == 0 {
        bail!("{name} must be at least 1 second, got `{value}`. Unset it or use a larger value.");
    }
    Ok(Duration::from_secs(secs))
}

/// Limits from the plan; `PHASEWISE_PHASE_TIMEOUT_SECONDS` overrides the phase timeout.
pub fn settings(run: &Run) -> Result<PhaseSettings> {
    let config = &run.plan.config;
    let phase_timeout = match std::env::var(PHASE_TIMEOUT_ENV) {
        Ok(value) => parse_timeout_seconds(PHASE_TIMEOUT_ENV, &value)?,
        Err(_) => Duration::from_secs(config.phase_timeout_minutes * 60),
    };
    let max_cost_usd = spending_limit(run)?;
    Ok(PhaseSettings {
        max_retries: config.max_retries,
        phase_timeout,
        verify_timeout: Duration::from_secs(config.verify_timeout_seconds),
        max_cost_usd,
        kill_grace: KILL_GRACE,
        agent_args: config.agent_args.clone(),
    })
}

/// Moves the phase awaiting review to `done`; completes the run when no phase
/// is pending. Only call it while holding the run lock.
pub fn approve(run: &mut Run) -> Result<()> {
    let phase = run
        .state
        .phases
        .iter_mut()
        .find(|p| p.status == PhaseStatus::AwaitingReview)
        .ok_or_else(|| {
            anyhow!("No phase is awaiting review. Run `phasewise status` to see the run.")
        })?;
    phase.status = PhaseStatus::Done;
    if !run
        .state
        .phases
        .iter()
        .any(|p| p.status == PhaseStatus::Pending)
    {
        run.state.status = RunStatus::Completed;
    }
    run.state.save(&run.paths)
}

/// Takes the run lock and reloads the state under it. Phases left
/// `running`/`verifying` are orphaned (we hold the lock) and are marked failed;
/// they are added to `run.recovered`.
fn lock_and_reload(run: &mut Run) -> Result<LockGuard> {
    let lock = LockGuard::acquire(&run.paths)?;
    let before = RunState::load(&run.paths)?;
    let state = load_locked(&run.paths, &lock)?;
    let recovered = recovered_phases(&before, &state);
    run.state = state;
    for number in recovered {
        if !run.recovered.contains(&number) {
            run.recovered.push(number);
        }
    }
    Ok(lock)
}

/// Numbers of phases that `old` had running/verifying and `new` has failed.
fn recovered_phases(old: &RunState, new: &RunState) -> Vec<usize> {
    old.phases
        .iter()
        .zip(&new.phases)
        .filter(|(old, new)| old.status != new.status && new.status == PhaseStatus::Failed)
        .map(|(old, _)| old.number)
        .collect()
}

/// Refuses `--model` for a phase that runs with an agent other than the run's:
/// a model name belongs to one agent. Nothing is stored.
fn check_model_override(run: &Run, overrides: &Overrides, number: usize) -> Result<()> {
    let agent = phase_agent(&run.state, &run.plan, number);
    if matches!(overrides.model, Some(Override::Set(_))) && agent != run.state.agent {
        bail!(
            "Phase {number} runs with {agent}; --model applies only to the run's agent ({}) phases. Run without --model.",
            run.state.agent
        );
    }
    Ok(())
}

/// Approves the phase awaiting review (if any) and runs the next pending one.
/// Holds the run lock for the whole operation.
pub fn next(
    run: &mut Run,
    chain: Chain,
    overrides: &Overrides,
    events: &Sender<EngineEvent>,
    cancel: &AtomicBool,
) -> Result<NextOutcome> {
    let _lock = lock_and_reload(run)?;
    check_can_advance(run)?;
    if let Some(pending) = run
        .state
        .phases
        .iter()
        .find(|p| p.status == PhaseStatus::Pending)
        .map(|p| p.number)
    {
        check_model_override(run, overrides, pending)?;
        // Before approving, so a bad `--until` leaves the review open.
        if let Chain::Until(stop) = chain {
            check_until(stop, pending, run.state.phases.len())?;
        }
    }
    overrides.apply(&mut run.state);
    // Only needed when a phase will run (completing the run commits nothing).
    // Checked before approving, so a missing identity or an exhausted budget
    // leaves the review open.
    if let Some(pending) = run
        .state
        .phases
        .iter()
        .find(|p| p.status == PhaseStatus::Pending)
        .map(|p| p.number)
    {
        run.git.check_identity()?;
        check_budget(run, &settings(run)?, pending)?;
    }
    if run
        .state
        .phases
        .iter()
        .any(|p| p.status == PhaseStatus::AwaitingReview)
    {
        approve(run)?;
        if run.state.status == RunStatus::Completed {
            return Ok(NextOutcome::Completed);
        }
    }
    execute_locked(run, chain, events, cancel).map(NextOutcome::Ran)
}

/// The run must be active. For callers that hold the run lock (a live lock is
/// then our own).
fn check_active(run: &Run) -> Result<()> {
    if run.state.status != RunStatus::Active {
        bail!(
            "Run {} is not active. Start a new one with `phasewise run`.",
            run.state.run_id
        );
    }
    Ok(())
}

/// The run must be active and no phasewise process may be running a phase.
/// Only for callers that do not hold the lock.
fn check_active_unlocked(run: &Run) -> Result<()> {
    check_active(run)?;
    if let Some(pid) = lock_holder(&run.paths) {
        let number = run
            .state
            .phases
            .iter()
            .find(|p| matches!(p.status, PhaseStatus::Running | PhaseStatus::Verifying))
            .map_or(0, |p| p.number);
        bail!("Phase {number} is running in process {pid}. Wait for it to finish.");
    }
    Ok(())
}

fn on_run_branch(run: &Run) -> Result<bool> {
    Ok(run.git.current_branch()?.as_deref() == Some(run.state.run_branch.as_str()))
}

fn check_on_run_branch(run: &Run) -> Result<()> {
    if !on_run_branch(run)? {
        bail!(
            "You are not on the run branch. Run `git switch {}` first.",
            run.state.run_branch
        );
    }
    Ok(())
}

fn check_can_advance(run: &Run) -> Result<()> {
    check_active(run)?;
    if let Some(failed) = run
        .state
        .phases
        .iter()
        .find(|p| p.status == PhaseStatus::Failed)
    {
        bail!(
            "Phase {} failed. Run `phasewise retry` or `phasewise rollback {}`.",
            failed.number,
            failed.number - 1
        );
    }
    check_on_run_branch(run)?;
    let dirty = dirty_paths(&run.git)?;
    if !dirty.is_empty() {
        bail!(
            "The working tree has uncommitted changes (e.g. {}). Commit or stash them, \
or run `phasewise rollback` to discard them.",
            dirty[0]
        );
    }
    Ok(())
}

/// Puts a phase back to `pending`, clearing everything but `attempts` and the
/// cost: what an attempt spent stays spent, and counts against `max_cost_usd`.
fn reset_phase(run: &mut Run, number: usize) {
    if let Some(phase) = run.state.phase_mut(number) {
        phase.status = PhaseStatus::Pending;
        phase.session_id = None;
        phase.checkpoint_sha = None;
        phase.summary = None;
        phase.failure = None;
        phase.model = None;
        phase.agent_model = None;
        phase.effort = None;
        phase.started_at = None;
        phase.finished_at = None;
    }
}

/// Reruns the highest-numbered `failed` or `awaiting_review` phase from the
/// checkpoint before it, in a fresh session, with `note` added to the prompt.
pub fn retry(
    run: &mut Run,
    note: Option<&str>,
    overrides: &Overrides,
    events: &Sender<EngineEvent>,
    cancel: &AtomicBool,
) -> Result<PhaseStatus> {
    let _lock = lock_and_reload(run)?;
    check_active(run)?;
    let number = run
        .state
        .phases
        .iter()
        .rev()
        .find(|p| matches!(p.status, PhaseStatus::Failed | PhaseStatus::AwaitingReview))
        .map(|p| p.number)
        .ok_or_else(|| {
            anyhow!(
                "Nothing to retry: no phase has failed or is awaiting review. \
Run `phasewise status` to see the run, or `phasewise next` to continue."
            )
        })?;
    check_on_run_branch(run)?;
    run.git.check_identity()?;
    let previous = run
        .state
        .checkpoint(number - 1)
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "Phase {} has no checkpoint to go back to. Run `phasewise status`, \
then `phasewise rollback 0` to start over.",
                number - 1
            )
        })?;
    // Before discarding anything: a refused retry must leave the work as it is.
    check_model_override(run, overrides, number)?;
    check_budget(run, &settings(run)?, number)?;
    run.git.reset_hard(&previous)?;
    run.git.clean_untracked()?;
    reset_phase(run, number);
    overrides.apply(&mut run.state);
    run.state.save(&run.paths)?;
    run_one(run, number, note, events, cancel)
}

/// Checks that `rollback k` can run and returns the SHA it goes back to. Does
/// not take the lock (the CLI calls it before its confirmation prompt);
/// `rollback` checks again under the lock.
pub fn prepare_rollback(run: &Run, k: usize) -> Result<String> {
    check_active_unlocked(run)?;
    rollback_target(run, k)
}

fn rollback_target(run: &Run, k: usize) -> Result<String> {
    check_on_run_branch(run)?;
    let valid = |n: usize| run.state.checkpoint(n).map(str::to_string);
    valid(k).ok_or_else(|| {
        let mut targets = vec!["0 (initial state)".to_string()];
        targets.extend(
            run.state
                .phases
                .iter()
                .filter(|p| p.checkpoint_sha.is_some())
                .map(|p| p.number.to_string()),
        );
        anyhow!(
            "Phase {k} has no checkpoint to go back to. Valid targets: {}.",
            targets.join(", ")
        )
    })
}

/// Goes back to the checkpoint of phase `k` (`0` = initial state); later
/// phases become `pending` again. `expected_sha` is the target the user
/// confirmed (from `prepare_rollback`); if the run changed since, nothing is done.
pub fn rollback(run: &mut Run, k: usize, expected_sha: Option<&str>) -> Result<()> {
    let _lock = lock_and_reload(run)?;
    check_active(run)?;
    let sha = rollback_target(run, k)?;
    if let Some(expected) = expected_sha
        && expected != sha
    {
        bail!(
            "The run changed while waiting for confirmation: the target of `rollback {k}` is \
now {}. Nothing was changed; run `phasewise rollback {k}` again.",
            sha.chars().take(7).collect::<String>()
        );
    }
    run.git.reset_hard(&sha)?;
    run.git.clean_untracked()?;
    let numbers: Vec<usize> = run.state.phases.iter().map(|p| p.number).collect();
    for number in numbers.into_iter().filter(|n| *n > k) {
        reset_phase(run, number);
    }
    run.state.save(&run.paths)
}

/// Gives the run up: keeps uncommitted work in a commit on the run branch,
/// returns to the original branch and marks the run `aborted`. Never deletes
/// the run branch.
pub fn abort(run: &mut Run) -> Result<AbortOutcome> {
    let _lock = lock_and_reload(run)?;
    check_active(run)?;
    let mut leftovers_sha = None;
    let mut switched = false;
    if on_run_branch(run)? {
        if !dirty_paths(&run.git)?.is_empty() {
            run.git.add_all()?;
            // A dirty path can be something `git add` cannot stage (changes
            // inside a submodule): then there is nothing to commit.
            if run.git.has_staged_changes()? {
                let message = format!(
                    "phasewise: abort — uncommitted changes\n\nPhasewise-Run: {}",
                    run.state.run_id
                );
                leftovers_sha = Some(run.git.commit(&message, false)?);
            }
        }
        run.git.switch(&run.state.original_branch).map_err(|e| {
            anyhow!(
                "Cannot go back to `{}`: {e:#}. Nothing is lost: uncommitted work is saved on \
the run branch. Switch to any branch yourself with `git switch`, then run `phasewise abort` again.",
                run.state.original_branch
            )
        })?;
        switched = true;
    }
    run.state.status = RunStatus::Aborted;
    run.state.save(&run.paths)?;
    let current_branch = run
        .git
        .current_branch()?
        .unwrap_or_else(|| "(detached HEAD)".to_string());
    Ok(AbortOutcome {
        current_branch,
        switched,
        leftovers_sha,
    })
}

/// Checks that `merge` can run: the run is completed and the working tree is
/// clean outside `.phasewise/`. Does not take the lock (the CLI calls it before
/// its confirmation prompt); `merge` checks again under the lock.
pub fn prepare_merge(run: &Run) -> Result<()> {
    if run.state.status != RunStatus::Completed {
        bail!(
            "Run {} is not completed, so there is nothing to merge yet. Finish its phases with \
`phasewise next`; `phasewise status` shows where the run is.",
            run.state.run_id
        );
    }
    let dirty = dirty_paths(&run.git)?;
    if !dirty.is_empty() {
        let listed: Vec<&str> = dirty
            .iter()
            .take(MAX_DIRTY_LISTED)
            .map(String::as_str)
            .collect();
        let more = dirty.len().saturating_sub(MAX_DIRTY_LISTED);
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        bail!(
            "The working tree has uncommitted changes ({}{more}). Commit or stash them \
(`git stash -u`), then run the merge again.",
            listed.join(", ")
        );
    }
    Ok(())
}

/// Integrates a completed run: switches to the original branch, squashes the
/// run branch into it and commits the result (the user's hooks and signing
/// apply). When the squash conflicts, the tree is restored, the original
/// branch is left as it was and the error names the files. Never deletes the
/// run branch and never pushes.
pub fn merge(run: &mut Run) -> Result<MergeOutcome> {
    let _lock = lock_and_reload(run)?;
    prepare_merge(run)?;
    run.git.check_identity()?;
    let original = run.state.original_branch.clone();
    let run_branch = run.state.run_branch.clone();
    if run.git.current_branch()?.as_deref() != Some(original.as_str()) {
        run.git.switch(&original).map_err(|e| {
            anyhow!(
                "Cannot switch to `{original}`: {e:#}. Switch to it yourself with \
`git switch {original}`, then run the merge again."
            )
        })?;
    }
    let manual = format!(
        "git switch {original} && git merge --squash {run_branch}, fix the conflicts, \
then `git add` the files and `git commit`"
    );
    match run.git.merge_squash(&run_branch)? {
        SquashResult::Clean => {}
        SquashResult::Conflict(files) => {
            // The tree was clean before the merge, so nothing of the user's is lost.
            run.git.reset_hard("HEAD")?;
            bail!(
                "Merging {run_branch} into {original} conflicts in: {}. Nothing was changed: \
you are on {original} with a clean working tree and {run_branch} is kept. To resolve it by \
hand, run {manual}.",
                files.join(", ")
            );
        }
    }
    let commit = if run.git.has_staged_changes()? {
        let title = run
            .state
            .plan_title
            .clone()
            .or_else(|| run.plan.title.clone())
            .and_then(|t| t.lines().next().map(|l| l.trim().to_string()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| run.state.run_id.clone());
        let message = format!(
            "phasewise: {title}\n\nPhasewise-Run: {}\n",
            run.state.run_id
        );
        let sha = run.git.commit_staged(&message).map_err(|e| {
            anyhow!(
                "{e:#} The changes of {run_branch} are staged on {original}. Fix the problem and \
run `git commit`, or discard them with `git reset --hard`."
            )
        })?;
        Some(sha)
    } else {
        None
    };
    Ok(MergeOutcome {
        branch: original,
        run_branch,
        commit,
    })
}

/// Refuses to start `number` once the run has spent its limit.
fn check_budget(run: &Run, settings: &PhaseSettings, number: usize) -> Result<()> {
    let Some(limit) = settings.max_cost_usd else {
        return Ok(());
    };
    let spent = run.state.cost_usd();
    if spent >= limit {
        bail!(
            "The run has spent ${spent:.2} of its ${limit:.2} limit (`max_cost_usd`), so \
phase {number} was not started. Set {MAX_COST_ENV} to a higher amount and run it again, or \
finish the remaining phases yourself."
        );
    }
    Ok(())
}

fn run_one(
    run: &mut Run,
    number: usize,
    note: Option<&str>,
    events: &Sender<EngineEvent>,
    cancel: &AtomicBool,
) -> Result<PhaseStatus> {
    let agent = agent_for(phase_agent(&run.state, &run.plan, number));
    let settings = settings(run)?;
    check_budget(run, &settings, number)?;
    let mut ctx = PhaseContext {
        git: &run.git,
        paths: &run.paths,
        state: &mut run.state,
        plan: &run.plan,
        agent: agent.as_ref(),
        settings: &settings,
        events,
        cancel,
    };
    run_phase(&mut ctx, number, note)
}

/// How many phases `execute` and `next` run before stopping for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    /// One phase.
    One,
    /// Every phase (`--auto`): each one that reaches `awaiting_review` is
    /// approved, up to the last.
    All,
    /// Up to this phase (`--until N`), which stays `awaiting_review`.
    Until(usize),
}

impl Chain {
    /// The chain the flags ask for; `--until` wins over `--auto`.
    pub fn from_flags(auto: bool, until: Option<usize>) -> Chain {
        match (until, auto) {
            (Some(stop), _) => Chain::Until(stop),
            (None, true) => Chain::All,
            (None, false) => Chain::One,
        }
    }
}

/// `--until` must name a phase from the one about to run to the last one.
pub fn check_until(stop: usize, first: usize, last: usize) -> Result<()> {
    if (first..=last).contains(&stop) {
        return Ok(());
    }
    if first == last {
        bail!(
            "--until {stop} is out of range: only phase {last} is left to run. Use --until {last}."
        )
    }
    bail!(
        "--until {stop} is out of range: it must be a phase from {first} (the next one to run) to {last} (the last). Use a number in that range."
    )
}

/// Runs the first pending phase. With a longer `chain`, approves each phase
/// that reaches `awaiting_review` and continues while phases remain and the
/// stop phase has not run; the last phase stays `awaiting_review`. Stops at the
/// first failure or on cancellation. Holds the run lock for the whole chain.
pub fn execute(
    run: &mut Run,
    chain: Chain,
    events: &Sender<EngineEvent>,
    cancel: &AtomicBool,
) -> Result<PhaseStatus> {
    let _lock = lock_and_reload(run)?;
    check_active(run)?;
    run.git.check_identity()?;
    if let (Chain::Until(stop), Some(first)) = (
        chain,
        run.state
            .phases
            .iter()
            .find(|p| p.status == PhaseStatus::Pending)
            .map(|p| p.number),
    ) {
        check_until(stop, first, run.state.phases.len())?;
    }
    execute_locked(run, chain, events, cancel)
}

fn execute_locked(
    run: &mut Run,
    chain: Chain,
    events: &Sender<EngineEvent>,
    cancel: &AtomicBool,
) -> Result<PhaseStatus> {
    loop {
        let number = run
            .state
            .phases
            .iter()
            .find(|p| p.status == PhaseStatus::Pending)
            .map(|p| p.number)
            .ok_or_else(|| {
                anyhow!("No phase is pending. Run `phasewise status` to see the run.")
            })?;
        let status = run_one(run, number, None, events, cancel)?;
        let more = run
            .state
            .phases
            .iter()
            .any(|p| p.status == PhaseStatus::Pending);
        let stopped = match chain {
            Chain::One => true,
            Chain::All => false,
            Chain::Until(stop) => number >= stop,
        };
        if stopped
            || status != PhaseStatus::AwaitingReview
            || !more
            || cancel.load(Ordering::SeqCst)
        {
            return Ok(status);
        }
        // Before approving, so the phase stays under review when the run stops.
        let next = run
            .state
            .phases
            .iter()
            .find(|p| p.status == PhaseStatus::Pending)
            .map_or(0, |p| p.number);
        check_budget(run, &settings(run)?, next)?;
        approve(run)?;
    }
}

/// What `phasewise plan` asks for.
#[derive(Debug, Clone)]
pub struct PlanRequest {
    pub goal: String,
    pub agent: AgentKind,
    pub options: LaunchOptions,
    /// Where to write the plan; `None` is the default plan path.
    pub output: Option<PathBuf>,
    /// Overwrite an existing output file.
    pub force: bool,
    /// Limit for each agent session.
    pub timeout: Duration,
}

/// What `create_plan` reports while it works.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanEvent {
    Started {
        plan_id: String,
        agent: AgentKind,
        model: Option<String>,
        effort: Option<String>,
    },
    Agent {
        event: AgentEvent,
    },
    /// One agent session ended (`attempt` starts at 1).
    AttemptFinished {
        attempt: u32,
        exit_code: Option<i32>,
    },
    /// The answer was unusable; the agent is asked again (`round` starts at 1).
    Repairing {
        round: u32,
        problems: Vec<String>,
    },
    Written {
        path: PathBuf,
    },
}

/// How `create_plan` ended.
#[derive(Debug)]
pub enum PlanResult {
    Written {
        path: PathBuf,
        plan: Plan,
    },
    Failed {
        problems: Vec<String>,
        /// Last draft the agent produced, if any.
        last_draft: Option<PathBuf>,
        cancelled: bool,
    },
}

#[derive(Debug)]
pub struct PlanOutcome {
    pub plan_id: String,
    /// Where the agent's output and the drafts are.
    pub dir: PathBuf,
    /// Sum of the costs the agent reported, when it reported any.
    pub cost_usd: Option<f64>,
    pub result: PlanResult,
}

/// Repository and output path of a `phasewise plan` call.
#[derive(Debug)]
pub struct PlanTarget {
    pub git: Git,
    pub output: PathBuf,
}

/// Checks that can fail before the agent starts: the repository and the
/// output path (relative paths are relative to `cwd`; the default is the
/// default plan path). An existing output is refused without `force`.
pub fn prepare_plan(cwd: &Path, output: Option<&Path>, force: bool) -> Result<PlanTarget> {
    let git = Git::discover(cwd)?;
    let output = match output {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => cwd.join(path),
        None => git.root().join(plan::DEFAULT_PLAN_FILE),
    };
    if output.exists() && !force {
        bail!(
            "{} already exists. Choose another path with `--output`, or pass `--force` to overwrite it.",
            output.display()
        );
    }
    if output.is_dir() {
        bail!(
            "{} is a directory. Choose a file path with `--output`.",
            output.display()
        );
    }
    Ok(PlanTarget { git, output })
}

/// Timeout of one planning session: 20 minutes, or `PHASEWISE_PLAN_TIMEOUT_SECONDS`.
pub fn plan_timeout() -> Result<Duration> {
    match std::env::var(PLAN_TIMEOUT_ENV) {
        Ok(value) => parse_timeout_seconds(PLAN_TIMEOUT_ENV, &value),
        Err(_) => Ok(PLAN_TIMEOUT),
    }
}

/// Asks the agent (read-only) to write a plan for the goal, checks it and, if
/// it is valid, writes it to the output path. Unusable answers are sent back
/// to the same session, at most twice. Nothing is written to the output path
/// on failure. Never prints; progress goes through `events`.
pub fn create_plan(
    cwd: &Path,
    request: &PlanRequest,
    events: &Sender<PlanEvent>,
    cancel: &AtomicBool,
) -> Result<PlanOutcome> {
    let target = prepare_plan(cwd, request.output.as_deref(), request.force)?;
    let git = &target.git;
    git.ensure_excluded(".phasewise/")?;
    let plan_id = new_run_id(Timestamp::now());
    let dir = PhasewiseDir::new(git.root())
        .dir()
        .join("plans")
        .join(&plan_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("Cannot create {}", dir.display()))?;

    let agent = agent_for(request.agent);
    let models = agent.list_models().ok();
    let base_prompt = prompt::plan_prompt(&request.goal, request.agent, models.as_deref());
    let emit = |event: PlanEvent| {
        let _ = events.send(event);
    };
    emit(PlanEvent::Started {
        plan_id: plan_id.clone(),
        agent: request.agent,
        model: request.options.model.clone(),
        effort: request.options.effort.clone(),
    });

    let mut cost_usd: Option<f64> = None;
    let mut last_draft: Option<PathBuf> = None;
    let mut session: Option<String> = None;
    let mut next_prompt = base_prompt.clone();
    let mut attempt = 0;
    loop {
        attempt += 1;
        let command = agent.plan_command(git.root(), &request.options, session.as_deref());
        let stdout_log = dir.join(format!("attempt-{attempt}.jsonl"));
        let stderr_log = dir.join(format!("attempt-{attempt}.stderr.log"));
        let run = run_agent_session(
            AgentSession {
                command,
                prompt: next_prompt.clone(),
                stdout_log: &stdout_log,
                stderr_log: &stderr_log,
                parser: agent.parser(git.root()),
                timeout: request.timeout,
                kill_grace: KILL_GRACE,
                cancel,
            },
            &mut |event| {
                emit(PlanEvent::Agent { event });
                Ok(())
            },
        )?;
        if run.spawn_error.is_none() {
            emit(PlanEvent::AttemptFinished {
                attempt,
                exit_code: run.exit_code,
            });
        }
        if let Some(cost) = run.cost_usd {
            cost_usd = Some(cost_usd.unwrap_or(0.0) + cost);
        }
        if run.session_id.is_some() {
            session = run.session_id.clone();
        }
        let failed = |problems: Vec<String>, last_draft: Option<PathBuf>, cancelled: bool| {
            Ok(PlanOutcome {
                plan_id: plan_id.clone(),
                dir: dir.clone(),
                cost_usd,
                result: PlanResult::Failed {
                    problems,
                    last_draft,
                    cancelled,
                },
            })
        };

        // Timeouts, cancellation and a missing binary are not worth another round.
        let name = request.agent;
        if let Some(error) = &run.spawn_error {
            return failed(
                vec![format!(
                    "Cannot start {name}: {error}. Install it or run `phasewise doctor`."
                )],
                last_draft,
                false,
            );
        }
        if run.cancelled {
            return failed(
                vec!["interrupted by the user (Ctrl-C)".into()],
                last_draft,
                true,
            );
        }
        if run.timed_out {
            return failed(
                vec![format!(
                    "{name} did not finish within {} s and was stopped. Try again, or raise \
PHASEWISE_PLAN_TIMEOUT_SECONDS",
                    request.timeout.as_secs()
                )],
                last_draft,
                false,
            );
        }

        let agent_failed = run.exit_code != Some(0) || run.finished_ok == Some(false);
        let problems = match check_plan_answer(&run, request.agent) {
            Ok((text, plan)) => {
                let draft = save_draft(&dir, attempt, &text);
                // The output was checked before the agent started; a file that
                // appeared since then belongs to the user and is kept.
                if !request.force && target.output.exists() {
                    return failed(
                        vec![format!(
                            "{} appeared while the agent was working and was left as it is. \
The new plan is the last draft: compare the two and keep what you want.",
                            target.output.display()
                        )],
                        draft.or(last_draft),
                        false,
                    );
                }
                write_atomic(&target.output, text.as_bytes())?;
                emit(PlanEvent::Written {
                    path: target.output.clone(),
                });
                return Ok(PlanOutcome {
                    plan_id,
                    dir,
                    cost_usd,
                    result: PlanResult::Written {
                        path: target.output.clone(),
                        plan,
                    },
                });
            }
            Err((draft, problems)) => {
                if let Some(text) = draft
                    && let Some(path) = save_draft(&dir, attempt, &text)
                {
                    last_draft = Some(path);
                }
                problems
            }
        };
        let round = attempt;
        // An agent that failed before it started a session (a rejected option,
        // a missing login) would only fail the same way again.
        let never_started = agent_failed && run.session_id.is_none() && run.final_text.is_none();
        if round > MAX_PLAN_REPAIRS || never_started {
            let mut problems = problems;
            if agent_failed {
                problems.push(format!(
                    "The agent's own messages are in attempt-{attempt}.stderr.log, in the agent \
output directory."
                ));
            }
            return failed(problems, last_draft, false);
        }
        emit(PlanEvent::Repairing {
            round,
            problems: problems.clone(),
        });
        next_prompt = if session.is_some() {
            prompt::plan_repair_prompt(&problems)
        } else {
            prompt::plan_retry_prompt(&base_prompt, &problems)
        };
    }
}

/// Writes `draft-K.md`; a failure only loses the draft, so it is ignored.
fn save_draft(dir: &Path, attempt: u32, text: &str) -> Option<PathBuf> {
    let path = dir.join(format!("draft-{attempt}.md"));
    write_atomic(&path, text.as_bytes()).ok().map(|()| path)
}

type Draft = Option<String>;

/// The valid plan text of an agent answer, or the draft found (if any) and the
/// problems to send back.
fn check_plan_answer(
    run: &crate::engine::AgentRun,
    agent: AgentKind,
) -> Result<(String, Plan), (Draft, Vec<String>)> {
    if run.exit_code != Some(0) || run.finished_ok == Some(false) {
        let code = run
            .exit_code
            .map_or_else(|| "no exit code".to_string(), |c| format!("exit code {c}"));
        let mut problem = format!("{agent} failed ({code})");
        if let Some(text) = &run.error_text {
            problem.push_str(&format!(": {text}"));
        }
        return Err((None, vec![problem]));
    }
    let Some(text) = run.final_text.as_deref().and_then(prompt::extract_plan) else {
        return Err((
            None,
            vec![format!(
                "the answer has no plan between a line `{}` and a line `{}`",
                prompt::PLAN_START,
                prompt::PLAN_END
            )],
        ));
    };
    // A fence around the whole plan is removed. It must not be parsed as part
    // of the plan: an inner ```verify fence does not close it, but the inner
    // closing fence does, so the wrapped text can parse as a shorter plan.
    let text = prompt::unwrap_fence(&text).unwrap_or(text);
    match plan::parse(&text) {
        Ok(plan) if plan.config.agent == agent => {
            match plan
                .phases
                .iter()
                .find(|p| p.agent.is_some_and(|a| a != agent))
            {
                Some(phase) => Err((
                    Some(text),
                    vec![format!(
                        "phase {} sets agent = \"{}\"; leave `agent` out of phase blocks",
                        phase.number,
                        phase.agent.map_or_else(String::new, |a| a.to_string())
                    )],
                )),
                None => Ok((text, plan)),
            }
        }
        Ok(plan) => Err((
            Some(text),
            vec![format!(
                "the front matter sets agent = \"{}\" but it must set agent = \"{agent}\"",
                plan.config.agent
            )],
        )),
        Err(errors) => Err((Some(text), errors.iter().map(ToString::to_string).collect())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn until_wins_over_auto_and_must_be_in_range() {
        assert_eq!(Chain::from_flags(false, None), Chain::One);
        assert_eq!(Chain::from_flags(true, None), Chain::All);
        assert_eq!(Chain::from_flags(true, Some(2)), Chain::Until(2));
        assert_eq!(Chain::from_flags(false, Some(2)), Chain::Until(2));
        assert!(check_until(2, 2, 4).is_ok());
        assert!(check_until(4, 2, 4).is_ok());
        let low = check_until(1, 2, 4).unwrap_err().to_string();
        assert!(low.contains("from 2") && low.contains("to 4"), "{low}");
        assert!(check_until(5, 2, 4).is_err());
        assert!(check_until(0, 1, 3).is_err());
        let only = check_until(2, 3, 3).unwrap_err().to_string();
        assert!(only.contains("--until 3"), "{only}");
    }

    #[test]
    fn flags_are_validated_and_default_clears() {
        let o = Overrides::parse(Some("opus"), Some("default")).expect("valid");
        assert_eq!(o.model, Some(Override::Set("opus".into())));
        assert_eq!(o.effort, Some(Override::Clear));
        assert_eq!(
            Overrides::parse(None, None).expect("none"),
            Overrides::default()
        );
        let err = Overrides::parse(Some("-x"), None)
            .expect_err("flag-like")
            .to_string();
        assert!(err.starts_with("Invalid --model"), "{err}");
        let err = Overrides::parse(None, Some("huge"))
            .expect_err("bad")
            .to_string();
        assert!(
            err.starts_with("Invalid --effort") && err.contains("xhigh"),
            "{err}"
        );
    }

    #[test]
    fn apply_sets_clears_and_leaves_alone() {
        let mut state = RunState::new(
            "r",
            Path::new("p.md"),
            None,
            AgentKind::Claude,
            "main",
            "phasewise/r",
            "abc",
            &[(1, "A".into())],
        );
        Overrides::parse(Some("m"), Some("high"))
            .expect("ok")
            .apply(&mut state);
        assert_eq!(state.model_override.as_deref(), Some("m"));
        assert_eq!(state.effort_override.as_deref(), Some("high"));
        Overrides::parse(None, Some("default"))
            .expect("ok")
            .apply(&mut state);
        assert_eq!(state.model_override.as_deref(), Some("m"));
        assert_eq!(state.effort_override, None);
    }

    #[test]
    fn a_draft_with_a_phase_agent_of_the_other_agent_is_sent_back() {
        let answer = |agent: &str| crate::engine::AgentRun {
            exit_code: Some(0),
            final_text: Some(format!(
                "{}\n+++\nagent = \"claude\"\n+++\n\n## A\n\nx\n\n```phasewise\nagent = \"{agent}\"\n```\n{}",
                prompt::PLAN_START,
                prompt::PLAN_END
            )),
            ..Default::default()
        };
        assert!(check_plan_answer(&answer("claude"), AgentKind::Claude).is_ok());
        let (_, problems) =
            check_plan_answer(&answer("codex"), AgentKind::Claude).expect_err("rejected");
        assert_eq!(
            problems,
            ["phase 1 sets agent = \"codex\"; leave `agent` out of phase blocks"]
        );
    }
}
