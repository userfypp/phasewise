//! One phase cycle: agent, verification, retries and checkpoint. Reports
//! progress through `EngineEvent`s; never prints.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use jiff::Timestamp;

use crate::agent::{Agent, AgentEvent, AgentParser, LaunchOptions};
use crate::git::{Git, checkpoint_message};
use crate::plan::{AgentKind, Plan};
use crate::process::{GroupChild, spawn_in_group};
use crate::prompt;
use crate::state::{
    Failure, FailureKind, PhaseStatus, RateLimits, RunPaths, RunState, record_group,
    truncate_summary,
};
use crate::verify::{self, FailedCommand, VerifyEvent, VerifyOutcome};

const POLL: Duration = Duration::from_millis(50);
const EOF_WAIT: Duration = Duration::from_secs(2);
const LEFTOVER_GRACE: Duration = Duration::from_millis(200);

/// What the engine reports while a phase runs.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    PhaseStarted {
        phase: usize,
        total: usize,
        title: String,
        attempt: u32,
        resumed: bool,
        /// Agent that runs this phase.
        agent: AgentKind,
        /// Effective model and effort of this attempt, when set.
        model: Option<String>,
        effort: Option<String>,
    },
    Agent {
        phase: usize,
        event: AgentEvent,
    },
    AgentExited {
        phase: usize,
        exit_code: Option<i32>,
    },
    VerifyStarted {
        phase: usize,
        command: String,
    },
    VerifyFinished {
        phase: usize,
        command: String,
        ok: bool,
        exit_code: Option<i32>,
        timed_out: bool,
    },
    Retrying {
        phase: usize,
        reason: String,
    },
    CheckpointCreated {
        phase: usize,
        sha: String,
    },
    PhaseFinished {
        phase: usize,
        status: PhaseStatus,
        failure: Option<Failure>,
        /// Files changed by the phase (successful phases only).
        files: Option<usize>,
        /// Cost of the phase when the agent reported one.
        cost_usd: Option<f64>,
    },
}

/// Limits and extra arguments for one phase.
#[derive(Debug, Clone)]
pub struct PhaseSettings {
    pub max_retries: u32,
    pub phase_timeout: Duration,
    pub verify_timeout: Duration,
    /// Spending limit for the whole run, in US dollars.
    pub max_cost_usd: Option<f64>,
    /// Grace period between `TERM` and `KILL` when stopping the agent.
    pub kill_grace: Duration,
    pub agent_args: Vec<String>,
}

/// Everything `run_phase` works on.
pub struct PhaseContext<'a> {
    pub git: &'a Git,
    pub paths: &'a RunPaths,
    pub state: &'a mut RunState,
    pub plan: &'a Plan,
    pub agent: &'a dyn Agent,
    pub settings: &'a PhaseSettings,
    pub events: &'a Sender<EngineEvent>,
    pub cancel: &'a AtomicBool,
}

impl PhaseContext<'_> {
    /// Sends an event; a closed receiver is ignored.
    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }

    fn save(&mut self) -> Result<()> {
        self.state.save(self.paths)
    }

    fn phase_state(&mut self, phase: usize) -> Result<&mut crate::state::PhaseState> {
        self.state
            .phase_mut(phase)
            .ok_or_else(|| anyhow!(
                "Phase {phase} does not exist in the run state. Run `phasewise status`; state.json may be damaged"
            ))
    }

    /// Marks the phase `failed`, saves and reports it.
    fn fail(&mut self, phase: usize, kind: FailureKind, detail: String) -> Result<PhaseStatus> {
        let failure = Failure { kind, detail };
        let state = self.phase_state(phase)?;
        state.status = PhaseStatus::Failed;
        state.finished_at = Some(Timestamp::now());
        state.failure = Some(failure.clone());
        self.save()?;
        self.emit(EngineEvent::PhaseFinished {
            phase,
            status: PhaseStatus::Failed,
            failure: Some(failure),
            files: None,
            cost_usd: None,
        });
        Ok(PhaseStatus::Failed)
    }
}

/// Agent that runs a phase: the phase block's `agent`, else the plan's. When
/// `run --agent` switched the run to another agent, that agent runs every phase.
pub fn phase_agent(state: &RunState, plan: &Plan, phase: usize) -> AgentKind {
    if state.agent != plan.config.agent {
        return state.agent;
    }
    plan.phases
        .iter()
        .find(|p| p.number == phase)
        .and_then(|p| p.agent)
        .unwrap_or(plan.config.agent)
}

/// Effective model and effort of a phase, highest precedence first: the run
/// override, the phase's settings block, the plan's front matter, nothing.
/// A model name belongs to one agent, so each source applies only to the agent
/// that owns it: the phase block to the phase's own agent (its `agent`, else the
/// plan's), the front matter to the plan's agent and `model_override` to the
/// run's agent. `effort_override` applies to every phase.
pub fn resolve_options(state: &RunState, plan: &Plan, phase: usize) -> LaunchOptions {
    let effective = phase_agent(state, plan, phase);
    let phase_settings = plan.phases.iter().find(|p| p.number == phase);
    let block_applies = effective
        == phase_settings
            .and_then(|p| p.agent)
            .unwrap_or(plan.config.agent);
    let plan_applies = effective == plan.config.agent;
    let pick = |over: Option<&String>,
                over_applies: bool,
                in_phase: Option<&Option<String>>,
                in_plan: &Option<String>| {
        over.filter(|_| over_applies).cloned().or_else(|| {
            in_phase
                .filter(|_| block_applies)
                .and_then(Clone::clone)
                .or_else(|| in_plan.clone().filter(|_| plan_applies))
        })
    };
    LaunchOptions {
        model: pick(
            state.model_override.as_ref(),
            effective == state.agent,
            phase_settings.map(|p| &p.model),
            &plan.config.model,
        ),
        effort: pick(
            state.effort_override.as_ref(),
            true,
            phase_settings.map(|p| &p.effort),
            &plan.config.effort,
        ),
    }
}

/// What the next agent launch sends.
struct Launch {
    prompt: String,
    resume_session: Option<String>,
}

/// Result of one agent process.
#[derive(Debug, Default)]
pub struct AgentRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    /// `ok` flag of the agent's final event, if it sent one.
    pub finished_ok: Option<bool>,
    /// Error text reported by the agent (final error preferred).
    pub error_text: Option<String>,
    pub spawn_error: Option<String>,
    /// Session (thread) id the agent announced.
    pub session_id: Option<String>,
    /// Complete final text of the agent (`Finished.summary`), untruncated.
    pub final_text: Option<String>,
    /// Sum of the costs the agent reported, when it reported any.
    pub cost_usd: Option<f64>,
}

/// Messages from the stdout reader thread.
enum Reader {
    Event(AgentEvent),
    Eof,
}

/// Everything `run_agent_session` needs to launch and follow one agent process.
pub struct AgentSession<'a> {
    /// Command line, with the working directory set; stdio is configured here.
    pub command: Command,
    /// Where to record the agent's process group while it runs, so that a
    /// later phasewise can stop it if this one is killed (see
    /// `state::record_group`).
    pub group_file: Option<&'a Path>,
    /// Written to the agent's stdin.
    pub prompt: String,
    /// Raw stdout lines go here.
    pub stdout_log: &'a Path,
    pub stderr_log: &'a Path,
    pub parser: Box<dyn AgentParser>,
    pub timeout: Duration,
    /// Grace period between `TERM` and `KILL` when stopping the agent.
    pub kill_grace: Duration,
    pub cancel: &'a AtomicBool,
}

/// Title of a phase and the checks that gate it: the plan's, then the phase's.
fn phase_checks(plan: &Plan, phase: usize) -> Result<(String, Vec<String>)> {
    let plan_phase = plan
        .phases
        .iter()
        .find(|p| p.number == phase)
        .ok_or_else(|| anyhow!(
                "Phase {phase} does not exist in the plan copy of this run. Start a new run with `phasewise run`"
            ))?;
    let mut commands = plan.config.verify.clone();
    commands.extend(plan_phase.verify.iter().cloned());
    Ok((plan_phase.title.clone(), commands))
}

fn failure_what(failed: &FailedCommand) -> String {
    if failed.timed_out {
        format!("timed out after {} s", failed.timeout_secs)
    } else {
        match failed.exit_code {
            Some(code) => format!("exit code {code}"),
            None => "terminated by a signal".to_string(),
        }
    }
}

/// The failure text of a check that failed, with the path of its log.
fn verify_failure_detail(failed: &FailedCommand, log: &Path) -> String {
    format!(
        "`{}` failed ({}). Output: {}",
        failed.command,
        failure_what(failed),
        log.display()
    )
}

/// Runs the checks of a failed phase on the working tree as the user left it
/// and, if they pass, checkpoints it. No agent is launched. The caller must
/// hold the run lock.
pub fn accept_phase(ctx: &mut PhaseContext, phase: usize) -> Result<PhaseStatus> {
    let (title, commands) = phase_checks(ctx.plan, phase)?;
    let state = ctx.phase_state(phase)?;
    state.status = PhaseStatus::Verifying;
    state.failure = None;
    state.finished_at = None;
    let attempts = state.attempts;
    ctx.save()?;
    match run_verification(ctx, phase, attempts, &commands)? {
        VerifyOutcome::Passed => checkpoint(ctx, phase, &title),
        VerifyOutcome::Cancelled => ctx.fail(
            phase,
            FailureKind::Interrupted,
            "interrupted by the user during verification".to_string(),
        ),
        VerifyOutcome::Failed(failed) => {
            let detail = verify_failure_detail(&failed, &ctx.paths.verify_log(phase));
            ctx.fail(phase, FailureKind::VerifyFailed, detail)
        }
    }
}

/// Runs one phase until it is `awaiting_review` or `failed`. The caller must
/// hold the run lock (`LockGuard`).
pub fn run_phase(
    ctx: &mut PhaseContext,
    phase_number: usize,
    note: Option<&str>,
) -> Result<PhaseStatus> {
    let phase = phase_number;
    let (title, verify_commands) = phase_checks(ctx.plan, phase)?;

    let completed: Vec<(usize, String, String)> = ctx
        .state
        .phases
        .iter()
        .filter(|p| p.status == PhaseStatus::Done)
        .map(|p| {
            (
                p.number,
                p.title.clone(),
                prompt::one_line_summary(p.summary.as_deref()),
            )
        })
        .collect();
    let base_prompt = prompt::phase_prompt(ctx.plan, phase, &completed, &verify_commands, note);

    // Before the agent can commit anything: where this phase starts.
    let start_sha = ctx.git.head_sha().ok();
    let state = ctx.phase_state(phase)?;
    state.start_sha = start_sha;
    state.started_at = Some(Timestamp::now());
    state.finished_at = None;
    state.failure = None;
    state.session_id = None;
    state.agent_model = None;

    let options = resolve_options(ctx.state, ctx.plan, phase);
    let mut launch = Launch {
        prompt: base_prompt.clone(),
        resume_session: None,
    };
    let mut retries_used = 0;
    loop {
        let attempt = {
            let state = ctx.phase_state(phase)?;
            state.attempts += 1;
            state.model = options.model.clone();
            state.effort = options.effort.clone();
            state.status = PhaseStatus::Running;
            state.attempts
        };
        ctx.save()?;
        ctx.emit(EngineEvent::PhaseStarted {
            phase,
            total: ctx.state.phases.len(),
            title: title.clone(),
            attempt,
            resumed: launch.resume_session.is_some(),
            agent: phase_agent(ctx.state, ctx.plan, phase),
            model: options.model.clone(),
            effort: options.effort.clone(),
        });

        let run = run_agent(ctx, phase, attempt, &launch, &options)?;
        if let Some(status) = check_agent_run(ctx, phase, attempt, &run)? {
            return Ok(status);
        }

        ctx.phase_state(phase)?.status = PhaseStatus::Verifying;
        ctx.save()?;
        let outcome = run_verification(ctx, phase, attempt, &verify_commands)?;
        let failed = match outcome {
            VerifyOutcome::Passed => return checkpoint(ctx, phase, &title),
            VerifyOutcome::Cancelled => {
                return ctx.fail(
                    phase,
                    FailureKind::Interrupted,
                    "interrupted by the user during verification".to_string(),
                );
            }
            VerifyOutcome::Failed(failed) => failed,
        };

        if retries_used >= ctx.settings.max_retries {
            let detail = verify_failure_detail(&failed, &ctx.paths.verify_log(phase));
            return ctx.fail(phase, FailureKind::VerifyFailed, detail);
        }
        let what = failure_what(&failed);
        if let Some(limit) = ctx.settings.max_cost_usd
            && let Some((spent, limit)) = cost_limit_reached(ctx.state.cost_usd(), limit)
        {
            let detail = format!(
                "`{}` failed ({what}). Not retrying: the run has spent ${spent} of its \
${limit} limit. Output: {}",
                failed.command,
                ctx.paths.verify_log(phase).display()
            );
            return ctx.fail(phase, FailureKind::VerifyFailed, detail);
        }
        retries_used += 1;
        ctx.emit(EngineEvent::Retrying {
            phase,
            reason: format!("`{}` failed ({what})", failed.command),
        });
        let report = prompt::verify_failure_report(
            &failed.command,
            failed.exit_code,
            failed.timed_out,
            failed.timeout_secs,
            &failed.output_tail,
        );
        launch = match ctx.state.phase(phase).and_then(|p| p.session_id.clone()) {
            Some(session) => Launch {
                prompt: prompt::resume_prompt(&report),
                resume_session: Some(session),
            },
            None => Launch {
                prompt: prompt::fresh_retry_prompt(&base_prompt, &report),
                resume_session: None,
            },
        };
    }
}

/// Turns a bad agent run into a failed phase; `None` means the agent succeeded.
fn check_agent_run(
    ctx: &mut PhaseContext,
    phase: usize,
    attempt: u32,
    run: &AgentRun,
) -> Result<Option<PhaseStatus>> {
    let agent_name = phase_agent(ctx.state, ctx.plan, phase).to_string();
    let stderr_log = ctx.paths.agent_stderr_log(phase, attempt);
    if let Some(error) = &run.spawn_error {
        let detail = format!("Cannot start {agent_name}: {error}");
        return ctx.fail(phase, FailureKind::AgentError, detail).map(Some);
    }
    if run.cancelled {
        let detail = "interrupted by the user (Ctrl-C)".to_string();
        return ctx.fail(phase, FailureKind::Interrupted, detail).map(Some);
    }
    if run.timed_out {
        let detail = format!(
            "{agent_name} did not finish within {} s and was stopped",
            ctx.settings.phase_timeout.as_secs()
        );
        return ctx.fail(phase, FailureKind::Timeout, detail).map(Some);
    }
    let exit_ok = run.exit_code == Some(0);
    if !exit_ok || run.finished_ok == Some(false) {
        let code = run
            .exit_code
            .map_or_else(|| "no exit code".to_string(), |c| format!("exit code {c}"));
        let mut detail = format!("{agent_name} failed ({code})");
        if let Some(text) = &run.error_text {
            detail.push_str(&format!(": {text}"));
        }
        detail.push_str(&format!(". See {}", stderr_log.display()));
        return ctx.fail(phase, FailureKind::AgentError, detail).map(Some);
    }
    let run_branch = ctx.state.run_branch.clone();
    let current = ctx.git.current_branch()?;
    if current.as_deref() != Some(run_branch.as_str()) {
        let now_on = current.unwrap_or_else(|| "a detached HEAD".to_string());
        let detail = format!(
            "{agent_name} left the repository on {now_on} instead of {run_branch}. \
Run `git switch {run_branch}`, then `phasewise retry` or `phasewise rollback {}`",
            phase - 1
        );
        return ctx.fail(phase, FailureKind::AgentError, detail).map(Some);
    }
    Ok(None)
}

/// Launches the phase's agent and reports what it does as engine events.
fn run_agent(
    ctx: &mut PhaseContext,
    phase: usize,
    attempt: u32,
    launch: &Launch,
    options: &LaunchOptions,
) -> Result<AgentRun> {
    let root = ctx.git.root().to_path_buf();
    // `agent_args` are written for the run's agent; another agent's phase gets none.
    let extra: &[String] = if phase_agent(ctx.state, ctx.plan, phase) == ctx.state.agent {
        &ctx.settings.agent_args
    } else {
        &[]
    };
    let command = match &launch.resume_session {
        Some(session) => ctx.agent.resume_command(&root, session, options, extra),
        None => ctx.agent.new_session_command(&root, options, extra),
    };
    let stdout_log = ctx.paths.agent_stdout_log(phase, attempt);
    let stderr_log = ctx.paths.agent_stderr_log(phase, attempt);
    let group_file = ctx.paths.group_file();
    let session = AgentSession {
        command,
        group_file: Some(&group_file),
        prompt: launch.prompt.clone(),
        stdout_log: &stdout_log,
        stderr_log: &stderr_log,
        parser: ctx.agent.parser(&root),
        timeout: ctx.settings.phase_timeout,
        kill_grace: ctx.settings.kill_grace,
        cancel: ctx.cancel,
    };
    let run = run_agent_session(session, &mut |event| handle_agent_event(ctx, phase, event))?;
    if run.spawn_error.is_none() {
        ctx.emit(EngineEvent::AgentExited {
            phase,
            exit_code: run.exit_code,
        });
    }
    Ok(run)
}

/// Spawns the agent in its own process group, feeds it the prompt, follows it
/// until it ends (or the timeout or the cancel flag stops it) and returns what
/// happened. Every parsed event goes to `on_event`, in order, before the next
/// one is read. Shared by phases and by `phasewise plan`.
pub fn run_agent_session(
    session: AgentSession,
    on_event: &mut dyn FnMut(AgentEvent) -> Result<()>,
) -> Result<AgentRun> {
    let AgentSession {
        mut command,
        group_file,
        prompt,
        stdout_log,
        stderr_log,
        parser,
        timeout,
        kill_grace,
        cancel,
    } = session;
    let stderr_file = File::create(stderr_log)
        .with_context(|| format!("Cannot create {}", stderr_log.display()))?;
    let stdout_file = File::create(stdout_log)
        .with_context(|| format!("Cannot create {}", stdout_log.display()))?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_file));
    let mut child = match spawn_in_group(&mut command) {
        Ok(child) => child,
        Err(e) => {
            return Ok(AgentRun {
                spawn_error: Some(e.to_string()),
                ..AgentRun::default()
            });
        }
    };

    if let Some(path) = group_file {
        record_group(path, child.id());
    }
    if let Some(mut stdin) = child.take_stdin() {
        // A broken pipe only means the agent stopped reading; its exit status tells the rest.
        thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
        });
    }
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.take_stdout() {
        thread::spawn(move || read_stdout(stdout, stdout_file, parser, tx));
    } else {
        let _ = tx.send(Reader::Eof);
    }
    let run = follow_agent(&mut child, &rx, timeout, kill_grace, cancel, on_event);
    // Dropping the child kills whatever `follow_agent` left; after that there
    // is nothing for recovery to stop.
    drop(child);
    if let Some(path) = group_file {
        let _ = std::fs::remove_file(path);
    }
    run
}

/// Reader thread: raw lines to the log, parsed events to the engine loop.
fn read_stdout(
    stdout: impl std::io::Read,
    mut log: File,
    mut parser: Box<dyn AgentParser>,
    tx: Sender<Reader>,
) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let _ = log.write_all(&buf);
        if !buf.ends_with(b"\n") {
            let _ = log.write_all(b"\n");
        }
        let _ = log.flush();
        let line = String::from_utf8_lossy(&buf);
        for event in parser.feed(line.trim_end_matches(['\r', '\n'])) {
            if tx.send(Reader::Event(event)).is_err() {
                return;
            }
        }
    }
    let _ = tx.send(Reader::Eof);
}

/// Loop of one agent process: forwards events and watches timeout, cancel and exit.
fn follow_agent(
    child: &mut GroupChild,
    rx: &Receiver<Reader>,
    timeout: Duration,
    kill_grace: Duration,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(AgentEvent) -> Result<()>,
) -> Result<AgentRun> {
    let mut run = AgentRun::default();
    let started = Instant::now();
    let mut eof = false;
    let mut stopped = false;
    let mut exited = false;
    while !exited {
        if eof {
            thread::sleep(POLL);
        } else {
            match rx.recv_timeout(POLL) {
                Ok(Reader::Event(event)) => track_event(&mut run, event, on_event)?,
                Ok(Reader::Eof) | Err(RecvTimeoutError::Disconnected) => eof = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        if !stopped {
            if cancel.load(Ordering::SeqCst) {
                run.cancelled = true;
                stopped = true;
            } else if started.elapsed() >= timeout {
                run.timed_out = true;
                stopped = true;
            }
            if stopped {
                let status = child.terminate(kill_grace)?;
                run.exit_code = status.code();
                exited = true;
            }
        }
        if !exited && let Some(status) = child.try_wait()? {
            run.exit_code = status.code();
            exited = true;
        }
    }
    if !stopped {
        // The phase is over: stop anything the agent left running in its group
        // (it could keep changing files or hold stdout open). Usually there is
        // nothing left and this returns at once.
        child.finish_leftovers(LEFTOVER_GRACE);
    }
    // The process is gone; give the reader a moment to deliver what is left.
    let deadline = Instant::now() + EOF_WAIT;
    while !eof {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(Reader::Event(event)) => track_event(&mut run, event, on_event)?,
            Ok(Reader::Eof) | Err(_) => eof = true,
        }
    }
    Ok(run)
}

/// Records what the run needs from an event, then hands the event on.
fn track_event(
    run: &mut AgentRun,
    event: AgentEvent,
    on_event: &mut dyn FnMut(AgentEvent) -> Result<()>,
) -> Result<()> {
    match &event {
        AgentEvent::SessionStarted { session_id, .. } => run.session_id = Some(session_id.clone()),
        AgentEvent::Error { detail } => {
            if run.finished_ok != Some(false) {
                run.error_text = first_line(detail);
            }
        }
        AgentEvent::Finished {
            ok,
            summary,
            cost_usd,
        } => {
            run.finished_ok = Some(*ok);
            if !ok && let Some(line) = first_line(summary) {
                run.error_text = Some(line);
            }
            run.final_text = Some(summary.clone());
            if let Some(cost) = cost_usd {
                run.cost_usd = Some(run.cost_usd.unwrap_or(0.0) + cost);
            }
        }
        _ => {}
    }
    on_event(event)
}

/// Phase bookkeeping for one agent event, then the event for the consumers.
fn handle_agent_event(ctx: &mut PhaseContext, phase: usize, event: AgentEvent) -> Result<()> {
    match &event {
        AgentEvent::SessionStarted { session_id, model } => {
            let state = ctx.phase_state(phase)?;
            state.session_id = Some(session_id.clone());
            if model.is_some() {
                state.agent_model = model.clone();
            }
            ctx.save()?;
        }
        AgentEvent::Finished {
            summary, cost_usd, ..
        } => {
            let state = ctx.phase_state(phase)?;
            state.summary = Some(truncate_summary(summary));
            if let Some(cost) = cost_usd {
                state.cost_usd = Some(state.cost_usd.unwrap_or(0.0) + cost);
            }
            ctx.save()?;
        }
        AgentEvent::RateLimits(windows) => {
            // Informational: a failed write never fails the phase.
            let _ = RateLimits::save_if_changed(
                ctx.git.root(),
                ctx.agent.kind(),
                windows,
                Timestamp::now().as_second(),
            );
        }
        _ => {}
    }
    ctx.emit(EngineEvent::Agent { phase, event });
    Ok(())
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn run_verification(
    ctx: &mut PhaseContext,
    phase: usize,
    attempt: u32,
    commands: &[String],
) -> Result<VerifyOutcome> {
    let root = ctx.git.root().to_path_buf();
    let log_path = ctx.paths.verify_log(phase);
    let events = ctx.events.clone();
    let mut forward = |event: VerifyEvent| {
        let _ = events.send(match event {
            VerifyEvent::Started { command } => EngineEvent::VerifyStarted { phase, command },
            VerifyEvent::Finished {
                command,
                ok,
                exit_code,
                timed_out,
            } => EngineEvent::VerifyFinished {
                phase,
                command,
                ok,
                exit_code,
                timed_out,
            },
        });
    };
    verify::run_all(
        commands,
        &root,
        ctx.settings.verify_timeout,
        &log_path,
        attempt,
        ctx.cancel,
        &mut forward,
    )
}

/// Commits the phase (empty commits allowed) and moves it to `awaiting_review`.
/// A git failure fails the phase (`checkpoint_failed`) instead of aborting.
fn checkpoint(ctx: &mut PhaseContext, phase: usize, title: &str) -> Result<PhaseStatus> {
    let run_branch = ctx.state.run_branch.clone();
    // Verification commands can switch branches.
    let current = ctx.git.current_branch();
    if let Ok(current) = current
        && current.as_deref() != Some(run_branch.as_str())
    {
        let now_on = current.unwrap_or_else(|| "a detached HEAD".to_string());
        let detail = format!(
            "The verification commands left the repository on {now_on} instead of {run_branch}, \
so no checkpoint was created. Run `git switch {run_branch}`, then `phasewise accept` \
(or `phasewise retry` to run the phase again)"
        );
        return ctx.fail(phase, FailureKind::CheckpointFailed, detail);
    }
    let message = checkpoint_message(&ctx.state.run_id, phase, title);
    let created = ctx
        .git
        .add_all()
        .and_then(|()| ctx.git.commit(&message, true))
        .and_then(|_| ctx.git.head_sha());
    let sha = match created {
        Ok(sha) => sha,
        Err(err) => {
            let detail = format!(
                "phasewise could not create the checkpoint commit: {err:#}. \
The phase's changes are still in the working tree. Fix the cause, then run \
`phasewise accept` to checkpoint them (or `phasewise retry` to run the phase again).{}",
                ctx.git.embedded_repo_advice()
            );
            return ctx.fail(phase, FailureKind::CheckpointFailed, detail);
        }
    };
    let state = ctx.phase_state(phase)?;
    state.checkpoint_sha = Some(sha.clone());
    state.status = PhaseStatus::AwaitingReview;
    state.finished_at = Some(Timestamp::now());
    ctx.save()?;
    let cost_usd = ctx.state.phase(phase).and_then(|p| p.cost_usd);
    let files = ctx
        .state
        .checkpoint(phase - 1)
        .and_then(|from| ctx.git.changed_file_count(from, &sha).ok());
    ctx.emit(EngineEvent::CheckpointCreated { phase, sha });
    ctx.emit(EngineEvent::PhaseFinished {
        phase,
        status: PhaseStatus::AwaitingReview,
        failure: None,
        files,
        cost_usd,
    });
    Ok(PhaseStatus::AwaitingReview)
}

/// Whether `spent` has reached `limit` (both in US dollars), compared in whole
/// micro-dollars so float sums such as 0.1 + 0.2 against 0.30 do not slip by a
/// step. Returns the two amounts to print, with more decimals only when two
/// decimals would show them as equal while the comparison says otherwise.
pub fn cost_limit_reached(spent: f64, limit: f64) -> Option<(String, String)> {
    let micros = |usd: f64| (usd * 1_000_000.0).round() as i64;
    if micros(spent) < micros(limit) {
        return None;
    }
    let (spent_2, limit_2) = (format!("{spent:.2}"), format!("{limit:.2}"));
    if spent_2 == limit_2 && micros(spent) != micros(limit) {
        return Some((format!("{spent:.6}"), format!("{limit:.6}")));
    }
    Some((spent_2, limit_2))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_cost_limit_is_compared_in_micro_dollars() {
        let same = |a: &str| Some((a.to_string(), a.to_string()));
        // Under the limit by a micro-dollar: still allowed.
        assert_eq!(cost_limit_reached(0.999_999, 1.0), None);
        // Sums of cents that land one float step off the limit count as equal.
        assert_eq!(cost_limit_reached(0.1 + 0.2, 0.30), same("0.30"));
        assert_eq!(cost_limit_reached(0.7 + 0.1 + 0.1, 0.90), same("0.90"));
        // Exact equality reaches the limit.
        assert_eq!(cost_limit_reached(1.0, 1.0), same("1.00"));
        // Normal values keep the two-decimal output.
        assert_eq!(cost_limit_reached(0.5, 1.0), None);
        assert_eq!(
            cost_limit_reached(1.5, 1.0),
            Some(("1.50".to_string(), "1.00".to_string()))
        );
        // Over the limit but equal at two decimals: show the precision.
        assert_eq!(
            cost_limit_reached(1.000_004, 1.0),
            Some(("1.000004".to_string(), "1.000000".to_string()))
        );
    }

    use super::*;
    use crate::plan::{self, AgentKind};

    const PLAN: &str = "+++\nmodel = \"plan-m\"\neffort = \"low\"\n+++\n## A\n\nx\n\n```phasewise\nmodel = \"phase-m\"\n```\n\n## B\n\ny\n";

    fn state(agent: AgentKind) -> RunState {
        RunState::new(
            "r",
            std::path::Path::new("p.md"),
            None,
            agent,
            "main",
            "phasewise/r",
            "abc",
            &[(1, "A".into()), (2, "B".into())],
        )
    }

    fn opts(model: Option<&str>, effort: Option<&str>) -> LaunchOptions {
        LaunchOptions {
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
        }
    }

    #[test]
    fn precedence_is_override_then_phase_then_plan_then_nothing() {
        let plan = plan::parse(PLAN).expect("plan");
        let mut s = state(AgentKind::Claude);
        assert_eq!(
            resolve_options(&s, &plan, 1),
            opts(Some("phase-m"), Some("low"))
        );
        assert_eq!(
            resolve_options(&s, &plan, 2),
            opts(Some("plan-m"), Some("low"))
        );
        s.model_override = Some("cli-m".into());
        s.effort_override = Some("max".into());
        assert_eq!(
            resolve_options(&s, &plan, 1),
            opts(Some("cli-m"), Some("max"))
        );
        assert_eq!(
            resolve_options(&s, &plan, 2),
            opts(Some("cli-m"), Some("max"))
        );
        let bare = plan::parse("## A\n\nx\n").expect("plan");
        assert_eq!(
            resolve_options(&state(AgentKind::Claude), &bare, 1),
            opts(None, None)
        );
    }

    #[test]
    fn plan_values_apply_only_to_the_plans_agent() {
        let plan = plan::parse(PLAN).expect("plan");
        let mut s = state(AgentKind::Codex);
        assert_eq!(resolve_options(&s, &plan, 1), opts(None, None));
        s.effort_override = Some("high".into());
        assert_eq!(resolve_options(&s, &plan, 1), opts(None, Some("high")));
    }

    const MIXED: &str = "+++\nagent = \"claude\"\nmodel = \"plan-m\"\n+++\n## A\n\nx\n\n## B\n\ny\n\n```phasewise\nagent = \"codex\"\nmodel = \"codex-m\"\neffort = \"low\"\n```\n";

    #[test]
    fn phase_block_agent_is_honoured_unless_the_run_switched_agents() {
        let plan = plan::parse(MIXED).expect("plan");
        let s = state(AgentKind::Claude);
        assert_eq!(phase_agent(&s, &plan, 1), AgentKind::Claude);
        assert_eq!(phase_agent(&s, &plan, 2), AgentKind::Codex);
        let s = state(AgentKind::Codex);
        assert_eq!(phase_agent(&s, &plan, 1), AgentKind::Codex);
        assert_eq!(phase_agent(&s, &plan, 2), AgentKind::Codex);
    }

    #[test]
    fn phase_model_belongs_to_the_phases_agent() {
        let plan = plan::parse(MIXED).expect("plan");
        let s = state(AgentKind::Claude);
        assert_eq!(resolve_options(&s, &plan, 1), opts(Some("plan-m"), None));
        // Phase 2 runs with codex: the plan's claude model does not leak in.
        assert_eq!(
            resolve_options(&s, &plan, 2),
            opts(Some("codex-m"), Some("low"))
        );
        // Run switched to codex: the front matter model is dropped, the block stays.
        let s = state(AgentKind::Codex);
        assert_eq!(resolve_options(&s, &plan, 1), opts(None, None));
        assert_eq!(
            resolve_options(&s, &plan, 2),
            opts(Some("codex-m"), Some("low"))
        );
    }

    #[test]
    fn model_override_skips_phases_of_the_other_agent_but_effort_applies_to_all() {
        let plan = plan::parse(MIXED).expect("plan");
        let mut s = state(AgentKind::Claude);
        s.model_override = Some("cli-m".into());
        s.effort_override = Some("max".into());
        assert_eq!(
            resolve_options(&s, &plan, 1),
            opts(Some("cli-m"), Some("max"))
        );
        assert_eq!(
            resolve_options(&s, &plan, 2),
            opts(Some("codex-m"), Some("max"))
        );
    }
}
