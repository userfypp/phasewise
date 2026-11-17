//! Terminal UI: data loading, state machine, rendering and the terminal loop.

pub mod app;
pub mod data;
pub mod home;
mod home_view;
#[cfg(test)]
mod screenshot;
pub mod shared;
pub mod view;
pub mod widgets;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Once, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, anyhow};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEvent, KeyEventKind,
};

use crate::agent::{ModelInfo, agent_for};
use crate::display::{failure_kind_label, first_line_short, plan_event_line, plan_header};
use crate::engine::EngineEvent;
use crate::ops::{
    self, AbortOutcome, MergeOutcome, NextOutcome, Overrides, PlanEvent, PlanOutcome, PlanRequest,
    Run,
};
use crate::plan::AgentKind;
use crate::process;
use crate::settings::{self, Settings};
use crate::state::{PhaseStatus, PhasewiseDir, RunPaths, RunState, RunStatus};
use crate::style::Styler;
use app::{App, Command, Tab};
use data::HomeData;
use home::{Home, HomeCommand, Mode as HomeMode, PlanLaunch, Probe};
use shared::{Notice, spinner_frame};

const NO_RUN: &str = "No run yet. Start one with `phasewise run`.";
const NEEDS_TERMINAL: &str = "phasewise review needs an interactive terminal. Run it from a terminal, or use `phasewise status` and `phasewise diff`.";
const NEEDS_TERMINAL_HOME: &str = "phasewise needs an interactive terminal for its home screen. Run it from a terminal, or use `phasewise --help` to see the commands.";
const POLL: Duration = Duration::from_millis(100);
const REFRESH_EVERY: Duration = Duration::from_millis(250);
/// How often the run screen looks for changes made by another process.
const OUTSIDE_CHECK_EVERY: Duration = Duration::from_secs(1);
/// How often the home screen reloads its data by itself.
const HOME_RELOAD_EVERY: Duration = Duration::from_secs(2);
/// How long the TUI loop may go without a turn, while a signal is pending or
/// the parent process has ended, before the watchdog ends phasewise.
const STUCK_AFTER: Duration = Duration::from_secs(30);
const WATCH_EVERY: Duration = Duration::from_millis(500);
/// Exit code of a forced quit (128 + SIGINT).
const FORCED_EXIT: u8 = 130;
/// Exit code when the watchdog ends phasewise (128 + SIGHUP).
const HANGUP_EXIT: i32 = 129;

/// When a TUI loop last went round, in ms since `CLOCK` started.
static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);
static CLOCK: OnceLock<Instant> = OnceLock::new();

fn clock_ms() -> u64 {
    let start = CLOCK.get_or_init(Instant::now);
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Called at every turn of a TUI loop, for the watchdog.
fn beat() {
    LAST_BEAT_MS.store(clock_ms(), Ordering::SeqCst);
}

/// Starts (once per process) the thread that ends phasewise when its terminal
/// is gone. crossterm 0.29 never returns from `event::poll` on a closed
/// terminal: it keeps reading end-of-file. The loop then cannot see a signal
/// or a missing parent, so after `STUCK_AFTER` without a turn the watchdog
/// does what a second signal does: it kills the agents' process groups and
/// exits. A phase left running is recovered as interrupted on the next start.
fn start_hangup_watchdog(signalled: Arc<AtomicBool>) {
    static STARTED: Once = Once::new();
    STARTED.call_once(move || {
        beat();
        let parent = process::ParentWatch::new();
        thread::spawn(move || {
            loop {
                thread::sleep(WATCH_EVERY);
                let silent = Duration::from_millis(
                    clock_ms().saturating_sub(LAST_BEAT_MS.load(Ordering::SeqCst)),
                );
                if is_stuck(
                    silent,
                    signalled.load(Ordering::SeqCst),
                    parent.parent_gone(),
                ) {
                    process::kill_all_groups();
                    std::process::exit(HANGUP_EXIT);
                }
            }
        });
    });
}

/// Waits for a worker that was told to stop, beating meanwhile: the wait is
/// progress, not a stuck loop, so the watchdog must not cut it short.
fn join_beating<T>(handle: JoinHandle<T>) -> thread::Result<T> {
    while !handle.is_finished() {
        beat();
        thread::sleep(POLL);
    }
    handle.join()
}

/// While stopping, Ctrl-C or `q` again forces the exit.
fn is_force_key(key: KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    match key.code {
        KeyCode::Char('c') => key.modifiers.contains(KeyModifiers::CONTROL),
        KeyCode::Char('q') => !key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

/// The loop has been silent too long while it has a reason to stop.
fn is_stuck(silent: Duration, signalled: bool, parent_gone: bool) -> bool {
    silent >= STUCK_AFTER && (signalled || parent_gone)
}

/// Restores the terminal on every exit path, including `?` and unwinding.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

/// What a run-screen worker produced.
enum Finished {
    /// A phase cycle (`next`, `retry`).
    Phase(NextOutcome),
    /// The run was integrated (`merge`).
    Merged(MergeOutcome),
}

/// An engine call running in its own thread; it owns its own `Run`.
struct Worker {
    handle: JoinHandle<Result<Finished>>,
    cancel: Arc<AtomicBool>,
    events: Receiver<EngineEvent>,
}

/// What the run screen can tell about the run on disk without loading it: the
/// id of the current run and the time and size of its `state.json`. Another
/// stamp than the one taken at the last load means another process (`next`,
/// `abort`, `merge`, a new run) changed the run since.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunStamp {
    run_id: Option<String>,
    state: Option<(SystemTime, u64)>,
}

impl RunStamp {
    fn read(repo_root: &Path) -> RunStamp {
        let run_id = PhasewiseDir::new(repo_root).read_current().ok().flatten();
        let state = run_id.as_ref().and_then(|id| {
            let meta = std::fs::metadata(RunPaths::new(repo_root, id).state_file()).ok()?;
            Some((meta.modified().ok()?, meta.len()))
        });
        RunStamp { run_id, state }
    }
}

/// The run screen: the review state, its worker, channel and caches. It
/// lives on while the home screen is shown.
struct Tui<'a> {
    cwd: &'a Path,
    app: App,
    /// Read-only copy used to load the views.
    run: Run,
    worker: Option<Worker>,
    last_refresh: Instant,
    refresh_pending: bool,
    /// The run on disk as of the last load.
    stamp: RunStamp,
    /// When the stamp was last compared; `None` means compare at once.
    last_outside_check: Option<Instant>,
    /// The last load found no run: its files were removed from outside.
    gone: bool,
    /// A phase or the run ended: ring the bell after the next draw (if the
    /// setting is on).
    bell: bool,
    /// Something changed since the last draw. Drawing only then keeps an idle
    /// TUI silent (no output every poll).
    dirty: bool,
}

/// Which screen is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Run,
}

/// Opens the review TUI. Refuses without a run or without a terminal.
pub fn run(cwd: &Path) -> Result<ExitCode> {
    let Some(run) = ops::load_current(cwd)? else {
        eprintln!("{NO_RUN}");
        return Ok(ExitCode::from(1));
    };
    if !std::io::stdout().is_terminal() {
        eprintln!("{NEEDS_TERMINAL}");
        return Ok(ExitCode::from(1));
    }
    let data = data::load_home(cwd)?;
    launch(cwd, data, Some(run), Screen::Run)
}

/// Opens the TUI inside a git repository: on the run screen when the current
/// run is active, else on the home screen.
pub fn run_home(cwd: &Path) -> Result<ExitCode> {
    if !std::io::stdout().is_terminal() {
        eprintln!("{NEEDS_TERMINAL_HOME}");
        return Ok(ExitCode::from(1));
    }
    let data = data::load_home(cwd)?;
    // A run that cannot be read must not keep the home screen from opening.
    let run = ops::load_current(cwd).ok().flatten();
    let screen = if run
        .as_ref()
        .is_some_and(|run| run.state.status == RunStatus::Active)
    {
        Screen::Run
    } else {
        Screen::Home
    };
    launch(cwd, data, run, screen)
}

/// Sets the terminal up and runs the one loop; on an error it stops every
/// worker and waits for them before reporting it.
fn launch(cwd: &Path, data: HomeData, run: Option<Run>, screen: Screen) -> Result<ExitCode> {
    let signalled = process::install_signal_handler(None)?;
    start_hangup_watchdog(Arc::clone(&signalled));
    let loaded = settings::load();
    let mut ui = Ui::new(
        cwd,
        Home::with_settings(data, loaded.settings.clone()),
        run,
        screen,
        signalled,
        loaded.settings,
    );
    // A broken settings file is shown, not fatal; the first key clears it.
    if !loaded.warnings.is_empty() {
        ui.show_notice(&Notice::warn(loaded.warnings.join(" ")));
    }
    ui.home.spawn_probes();
    let mut terminal = ratatui::try_init()?;
    let _guard = TerminalGuard;
    // Without this, a pasted line break arrives as Enter and submits the
    // input. A terminal that does not know the mode ignores it.
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    if let Err(err) = ui.event_loop(&mut terminal) {
        ui.stop_all();
        return Err(err);
    }
    if ui.forced {
        return Ok(ExitCode::from(FORCED_EXIT));
    }
    Ok(ExitCode::SUCCESS)
}

/// What the background threads of the home screen report.
enum Background {
    Version(AgentKind, Probe),
    Models(AgentKind, Result<Vec<ModelInfo>, String>),
    /// The plan commit ended: the short SHA, or git's error.
    Committed(Result<String, String>),
    /// The periodic reload of the home data ended (with the generation it
    /// started in).
    Reloaded(u64, Box<Result<HomeData, String>>),
}

/// The planning running in its own thread.
struct PlanWorker {
    handle: JoinHandle<Result<PlanOutcome>>,
    cancel: Arc<AtomicBool>,
    events: Receiver<PlanEvent>,
    started: Instant,
}

/// The home screen: its state, the planning worker and the background
/// channel. It always exists.
struct HomeTui<'a> {
    cwd: &'a Path,
    home: Home,
    worker: Option<PlanWorker>,
    background: Receiver<Background>,
    background_tx: mpsc::Sender<Background>,
    /// The commit thread, while it runs.
    commit: Option<JoinHandle<()>>,
    /// Start the run with these choices when the commit succeeds.
    then_start: Option<(AgentKind, Overrides)>,
    /// A periodic reload runs in its own thread.
    reloading: bool,
    /// Bumped by every explicit reload: an older periodic result is stale.
    reload_gen: u64,
    dirty: bool,
    /// Set when a run was started: the loop shows the run screen.
    started: Option<Run>,
}

impl<'a> HomeTui<'a> {
    fn new(cwd: &'a Path, home: Home) -> HomeTui<'a> {
        let (background_tx, background) = mpsc::channel();
        HomeTui {
            cwd,
            home,
            worker: None,
            background,
            background_tx,
            commit: None,
            then_start: None,
            reloading: false,
            reload_gen: 0,
            dirty: true,
            started: None,
        }
    }

    /// Looks for the version of both agents, each in its own thread.
    fn spawn_probes(&self) {
        for kind in [AgentKind::Claude, AgentKind::Codex] {
            let tx = self.background_tx.clone();
            thread::spawn(move || {
                let probe = match agent_for(kind).version() {
                    Ok(version) => Probe::Version(version),
                    Err(_) => Probe::NotInstalled,
                };
                // The screen may be gone already.
                let _ = tx.send(Background::Version(kind, probe));
            });
        }
    }

    fn spawn_models(&self, kind: AgentKind) {
        let tx = self.background_tx.clone();
        thread::spawn(move || {
            let result = agent_for(kind)
                .list_models()
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(Background::Models(kind, result));
        });
    }

    /// Commits the plan file in its own thread (hooks may be slow).
    fn spawn_commit(&mut self, then_start: Option<(AgentKind, Overrides)>) {
        if self.commit.is_some() {
            return;
        }
        let tx = self.background_tx.clone();
        let cwd: PathBuf = self.cwd.to_path_buf();
        let path = self.home.data.plan_path.clone();
        self.commit = Some(thread::spawn(move || {
            let result = ops::leave_finished_run(&cwd)
                .and_then(|()| ops::commit_plan(&cwd, &path))
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(Background::Committed(result));
        }));
        self.then_start = then_start;
        self.home.begin_commit();
    }

    /// Runs a command of the home screen; quitting and opening the run
    /// screen belong to the loop.
    fn execute(&mut self, command: HomeCommand) {
        match command {
            HomeCommand::Quit | HomeCommand::OpenRun | HomeCommand::SaveSettings(_) => {}
            HomeCommand::Start { agent, overrides } => self.start_run(agent, &overrides),
            HomeCommand::Commit => self.spawn_commit(None),
            HomeCommand::CommitAndStart { agent, overrides } => {
                self.spawn_commit(Some((agent, overrides)));
            }
            HomeCommand::Plan(launch) => self.start_planning(launch),
            HomeCommand::CancelPlan => {
                self.interrupt();
                self.home.message = Some(Notice::info("Stopping the planning…"));
            }
            HomeCommand::LoadModels(kind) => self.spawn_models(kind),
        }
        self.dirty = true;
    }

    fn start_run(&mut self, agent: AgentKind, overrides: &Overrides) {
        match ops::start(self.cwd, &self.home.data.plan_path, Some(agent), overrides) {
            Ok(run) => self.started = Some(run),
            Err(err) => self.home.show_start_error(&err),
        }
    }

    fn interrupt(&self) {
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn start_planning(&mut self, launch: PlanLaunch) {
        if self.worker.is_some() {
            return;
        }
        // The file did not exist when the screen was loaded: ask again rather
        // than fail with the CLI's `--force` advice.
        if !launch.force && self.home.data.plan_path.exists() {
            self.reload(false);
            self.home.message = Some(Notice::warn(format!(
                "{} was created meanwhile. Press p again to replace it.",
                self.home.plan_name()
            )));
            return;
        }
        let timeout = match ops::plan_timeout() {
            Ok(timeout) => timeout,
            Err(err) => {
                self.home
                    .show_error(err.to_string().lines().map(str::to_string).collect());
                return;
            }
        };
        let request = PlanRequest {
            goal: launch.goal,
            agent: launch.agent,
            options: launch.options,
            output: Some(self.home.data.plan_path.clone()),
            force: launch.force,
            timeout,
        };
        let cwd: PathBuf = self.cwd.to_path_buf();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (tx, events) = mpsc::channel();
        let handle = thread::spawn(move || ops::create_plan(&cwd, &request, &tx, &worker_cancel));
        self.worker = Some(PlanWorker {
            handle,
            cancel,
            events,
            started: Instant::now(),
        });
        self.home.begin_planning();
    }

    /// Handles what the background threads sent. A periodic reload is applied
    /// only when `apply_reload` (the home screen is shown and idle).
    fn drain_background(&mut self, quitting: bool, apply_reload: bool) {
        while let Ok(message) = self.background.try_recv() {
            match message {
                Background::Version(kind, probe) => self.home.set_probe(kind, probe),
                Background::Models(kind, result) => self.home.set_models(kind, result),
                Background::Committed(result) => self.commit_done(&result, quitting),
                Background::Reloaded(generation, result) => {
                    self.reloading = false;
                    let fresh = generation == self.reload_gen;
                    if let Ok(data) = *result
                        && apply_reload
                        && fresh
                        && self.commit.is_none()
                        && self.home.set_data(data)
                    {
                        self.dirty = true;
                    }
                    continue;
                }
            }
            self.dirty = true;
        }
    }

    /// A commit thread that ended without a result panicked: report it, or
    /// the spinner would never stop and quitting would never finish.
    fn check_commit(&mut self, quitting: bool) {
        if !self.commit.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        // A result sent just before the thread ended is in the channel now.
        self.drain_background(quitting, false);
        if self.commit.is_some() {
            let panicked =
                Err("The commit thread panicked. This is a bug; please report it.".to_string());
            self.commit_done(&panicked, quitting);
            self.dirty = true;
        }
    }

    /// Loads the home data in a thread (at most one at a time); the result
    /// comes back as `Background::Reloaded`.
    fn spawn_reload(&mut self) {
        if self.reloading {
            return;
        }
        self.reloading = true;
        let generation = self.reload_gen;
        let tx = self.background_tx.clone();
        let cwd: PathBuf = self.cwd.to_path_buf();
        thread::spawn(move || {
            let result = data::load_home(&cwd).map_err(|err| format!("{err:#}"));
            let _ = tx.send(Background::Reloaded(generation, Box::new(result)));
        });
    }

    fn commit_done(&mut self, result: &Result<String, String>, quitting: bool) {
        if let Some(handle) = self.commit.take() {
            // The thread has sent its result; it ends at once.
            let _ = handle.join();
        }
        let ok = self.home.commit_finished(result);
        self.reload(false);
        if let Some((agent, overrides)) = self.then_start.take()
            && ok
            && !quitting
        {
            self.start_run(agent, &overrides);
        }
    }

    fn drain_plan_events(&mut self) {
        let Some(worker) = &self.worker else { return };
        let styler = Styler { color: false };
        let mut changed = false;
        // Empty or disconnected: nothing more to show now.
        while let Ok(event) = worker.events.try_recv() {
            // Paths inside the repository are shown relative to its root.
            let event = match event {
                PlanEvent::Written { path } => PlanEvent::Written {
                    path: path
                        .strip_prefix(&self.home.data.root)
                        .map_or(path.clone(), Path::to_path_buf),
                },
                other => other,
            };
            let header = plan_header(&event, &styler);
            let line = plan_event_line(&event).map(|l| l.at(worker.started.elapsed().as_secs()));
            self.home.plan_event(header, line);
            changed = true;
        }
        if changed {
            self.dirty = true;
        }
    }

    /// When the planning has ended: joins it, reloads the plan and reports.
    fn finish_worker(&mut self, quitting: bool) {
        if !self.worker.as_ref().is_some_and(|w| w.handle.is_finished()) {
            return;
        }
        // Everything the worker sent is in the channel now.
        self.drain_plan_events();
        let Some(worker) = self.worker.take() else {
            return;
        };
        let result = worker.handle.join().unwrap_or_else(|_| {
            Err(anyhow!(
                "The planning thread panicked. This is a bug; please report it."
            ))
        });
        self.dirty = true;
        // A failed reload leaves an error message; the report of the planning
        // keeps priority when it has a message of its own.
        self.reload(false);
        if quitting {
            return;
        }
        let report = home::plan_report(&result, &self.home.data.root);
        self.home.plan_finished(report);
    }

    /// Loads the home data again, keeping the user's choices. A quiet reload
    /// (the periodic one) reports nothing on failure and redraws only when
    /// the data changed. False on error.
    fn reload(&mut self, quiet: bool) -> bool {
        self.reload_gen += 1;
        match data::load_home(self.cwd) {
            Ok(data) => {
                if self.home.set_data(data) || !quiet {
                    self.dirty = true;
                }
                true
            }
            Err(err) => {
                if !quiet {
                    self.home.message = Some(Notice::error(format!("Error: {err}")));
                    self.dirty = true;
                }
                false
            }
        }
    }
}

/// The two screens and the one loop that drives them.
struct Ui<'a> {
    cwd: &'a Path,
    screen: Screen,
    home: HomeTui<'a>,
    /// The state of the current run, when the repository has one.
    run: Option<Tui<'a>>,
    quitting: bool,
    /// The user settings in force (the screens hold copies for rendering).
    settings: Settings,
    /// Left by a second Ctrl-C or `q` while stopping: exit code 130.
    forced: bool,
    /// Set by SIGINT/SIGTERM/SIGHUP (the first one acts like a confirmed quit).
    signalled: Arc<AtomicBool>,
    /// A closed terminal acts like a signal (see `ParentWatch`).
    parent: process::ParentWatch,
    /// Clock of the spinner.
    clock: Instant,
    last_frame: usize,
    next_reload: Instant,
}

impl<'a> Ui<'a> {
    fn new(
        cwd: &'a Path,
        home: Home,
        run: Option<Run>,
        screen: Screen,
        signalled: Arc<AtomicBool>,
        settings: Settings,
    ) -> Ui<'a> {
        let run = run.map(|run| {
            let mut tui = Tui::new(cwd, run, &settings);
            tui.load_caches();
            tui
        });
        Ui {
            cwd,
            screen,
            home: HomeTui::new(cwd, home),
            run,
            settings,
            quitting: false,
            forced: false,
            signalled,
            parent: process::ParentWatch::new(),
            clock: Instant::now(),
            last_frame: 0,
            next_reload: Instant::now() + HOME_RELOAD_EVERY,
        }
    }

    fn any_worker(&self) -> bool {
        self.home.worker.is_some()
            || self.home.commit.is_some()
            || self.run.as_ref().is_some_and(|t| t.worker.is_some())
    }

    fn visible_dirty(&self) -> bool {
        match self.screen {
            Screen::Home => self.home.dirty,
            Screen::Run => self.run.as_ref().is_some_and(|t| t.dirty),
        }
    }

    fn mark_dirty(&mut self) {
        match self.screen {
            Screen::Home => self.home.dirty = true,
            Screen::Run => {
                if let Some(tui) = &mut self.run {
                    tui.dirty = true;
                }
            }
        }
    }

    fn animating(&self) -> bool {
        match self.screen {
            Screen::Home => self.home.home.animating(),
            Screen::Run => self.run.as_ref().is_some_and(|t| t.app.animating()),
        }
    }

    /// Tells each screen what the other one is doing and moves the spinner.
    fn tick(&mut self) {
        let busy = self.run.as_ref().is_some_and(|t| t.worker.is_some());
        let planning = self.home.worker.is_some();
        self.home.home.run_busy = busy;
        let frame = spinner_frame(self.clock.elapsed());
        self.home.home.frame = frame;
        if let Some(tui) = &mut self.run {
            tui.app.worker_busy = tui.worker.is_some();
            tui.app.planning_busy = planning;
            tui.app.frame = frame;
        }
        if frame != self.last_frame {
            self.last_frame = frame;
            if self.animating() {
                self.mark_dirty();
            }
        }
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            beat();
            self.tick();
            // Same rule as the command output: `NO_COLOR` (non-empty), a dumb
            // terminal or `color = never` means no colour. Checked every turn
            // because the settings menu can change it.
            let monochrome = !crate::style::color_enabled(
                true,
                std::env::var("NO_COLOR").ok().as_deref(),
                std::env::var("TERM").ok().as_deref(),
                self.settings.color_never(),
            );
            if self.visible_dirty() {
                match (self.screen, &mut self.run) {
                    (Screen::Run, Some(tui)) => {
                        terminal.draw(|frame| {
                            view::draw(frame, &mut tui.app);
                            if monochrome {
                                widgets::strip_color(frame.buffer_mut());
                            }
                        })?;
                        tui.dirty = false;
                    }
                    _ => {
                        terminal.draw(|frame| {
                            home_view::draw(frame, &self.home.home);
                            if monochrome {
                                widgets::strip_color(frame.buffer_mut());
                            }
                        })?;
                        self.home.dirty = false;
                    }
                }
            }
            if self.take_bell() {
                // Straight to the terminal, after the frame: the bell is not
                // part of what ratatui draws.
                let backend = terminal.backend_mut();
                let _ = backend.write_all(settings::BELL.as_bytes());
                let _ = backend.flush();
            }
            if !self.quitting
                && (self.signalled.load(Ordering::SeqCst) || self.parent.parent_gone())
            {
                self.quit_now();
                self.mark_dirty();
            }
            if self.quitting && !self.any_worker() {
                return Ok(());
            }
            if event::poll(POLL)? {
                // Any key or resize may change what is shown.
                self.mark_dirty();
                // A resize needs nothing more than the redraw of the next turn.
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if !self.quitting {
                            self.on_key(key);
                        } else if is_force_key(key) {
                            // A worker that never stops must not trap the user:
                            // do what a second signal does.
                            process::kill_all_groups();
                            self.forced = true;
                            return Ok(());
                        }
                    }
                    Event::Paste(text) if !self.quitting => self.on_paste(&text),
                    _ => {}
                }
            }
            self.pump();
        }
    }

    /// The background work of every turn, whatever the screen.
    fn pump(&mut self) {
        if self.screen == Screen::Run && self.run.is_none() {
            self.screen = Screen::Home;
            self.home.dirty = true;
        }
        let apply_reload = self.screen == Screen::Home && self.home.home.mode == HomeMode::Normal;
        self.home.drain_background(self.quitting, apply_reload);
        self.home.check_commit(self.quitting);
        self.home.drain_plan_events();
        self.home.finish_worker(self.quitting);
        if let Some(tui) = &mut self.run {
            tui.drain_events();
            tui.finish_worker();
            if tui.refresh_pending && tui.last_refresh.elapsed() >= REFRESH_EVERY {
                tui.refresh();
            }
            if self.screen == Screen::Run {
                tui.follow_outside_changes();
            }
        }
        // The run was removed from outside: leave its screen for the home one.
        if self.run.as_ref().is_some_and(|tui| tui.gone) {
            self.run = None;
            self.home.reload(true);
            if self.screen == Screen::Run {
                self.screen = Screen::Home;
                self.home.home.message = Some(Notice::info(
                    "The run no longer exists: its files were removed.",
                ));
            }
            self.home.dirty = true;
        }
        if let Some(run) = self.home.started.take() {
            self.begin_run(run);
        }
        self.auto_reload();
    }

    /// While the home screen is shown and idle, the data reloads by itself.
    fn auto_reload(&mut self) {
        if self.screen != Screen::Home
            || self.home.home.mode != HomeMode::Normal
            || self.home.commit.is_some()
            || Instant::now() < self.next_reload
        {
            return;
        }
        self.next_reload = Instant::now() + HOME_RELOAD_EVERY;
        self.home.spawn_reload();
    }

    fn on_paste(&mut self, text: &str) {
        match (self.screen, &mut self.run) {
            (Screen::Run, Some(tui)) => tui.app.handle_paste(text),
            _ => self.home.home.handle_paste(text),
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        match self.screen {
            Screen::Home => {
                if let Some(command) = self.home.home.handle_key(key) {
                    match command {
                        HomeCommand::Quit => self.quit_now(),
                        HomeCommand::OpenRun => self.open_run(),
                        HomeCommand::SaveSettings(settings) => self.save_settings(settings),
                        other => self.home.execute(other),
                    }
                }
            }
            Screen::Run => {
                let Some(tui) = &mut self.run else {
                    self.screen = Screen::Home;
                    return;
                };
                if let Some(command) = tui.app.handle_key(key) {
                    match command {
                        Command::Quit => self.quit_now(),
                        Command::Home => self.go_home(),
                        Command::SaveSettings(settings) => self.save_settings(settings),
                        other => tui.execute(other),
                    }
                }
            }
        }
    }

    /// Makes `settings` the ones in force on both screens.
    fn apply_settings(&mut self, settings: Settings) {
        self.home.home.settings = settings.clone();
        if let Some(tui) = &mut self.run {
            tui.app.settings = settings.clone();
        }
        self.settings = settings;
    }

    /// Applies the settings at once and saves them; a failure is shown and
    /// the change still holds for this session.
    fn save_settings(&mut self, settings: Settings) {
        self.apply_settings(settings.clone());
        if let Err(err) = settings::save(&settings) {
            self.show_notice(&Notice::error(format!("Error: {err}")));
        }
    }

    /// A message on both screens.
    fn show_notice(&mut self, notice: &Notice) {
        self.home.home.message = Some(notice.clone());
        if let Some(tui) = &mut self.run {
            tui.app.message = Some(notice.clone());
        }
    }

    /// True once per event that rings the bell and only when the setting is on.
    fn take_bell(&mut self) -> bool {
        let Some(tui) = &mut self.run else {
            return false;
        };
        let pending = std::mem::take(&mut tui.bell);
        pending && settings::bell_wanted(self.settings.bell(), true)
    }

    /// Interrupts every worker; the loop ends when they have stopped.
    fn quit_now(&mut self) {
        self.home.interrupt();
        if let Some(tui) = &self.run {
            tui.interrupt();
        }
        if self.any_worker() {
            let notice = Notice::info("Stopping… press Ctrl-C again to force");
            self.home.home.message = Some(notice.clone());
            if let Some(tui) = &mut self.run {
                tui.app.message = Some(notice);
            }
        }
        self.quitting = true;
    }

    /// After an error: stops every worker and waits for them.
    fn stop_all(&mut self) {
        self.home.interrupt();
        if let Some(worker) = self.home.worker.take() {
            let _ = join_beating(worker.handle);
        }
        if let Some(handle) = self.home.commit.take() {
            let _ = join_beating(handle);
        }
        if let Some(tui) = &mut self.run {
            tui.interrupt();
            if let Some(worker) = tui.worker.take() {
                let _ = join_beating(worker.handle);
            }
        }
    }

    /// Run screen to home screen: the data reloads on arrival.
    fn go_home(&mut self) {
        self.screen = Screen::Home;
        self.home.reload(false);
        self.home.dirty = true;
        self.next_reload = Instant::now() + HOME_RELOAD_EVERY;
    }

    /// Home screen to run screen for the current run (active or finished).
    fn open_run(&mut self) {
        let run = match ops::load_current(self.cwd) {
            Ok(Some(run)) => run,
            Ok(None) => {
                self.home.home.message = Some(Notice::error(NO_RUN));
                return;
            }
            Err(err) => {
                self.home.home.message = Some(Notice::error(format!("Error: {err}")));
                return;
            }
        };
        match &mut self.run {
            Some(tui) if tui.run.state.run_id == run.state.run_id => tui.refresh(),
            // A worker belongs to its run: only an idle state is replaced.
            Some(tui) if tui.worker.is_some() => {}
            _ => {
                let mut tui = Tui::new(self.cwd, run, &self.settings);
                tui.load_caches();
                self.run = Some(tui);
            }
        }
        self.show_run();
    }

    /// A run was started from the home screen: its phases start running.
    fn begin_run(&mut self, run: Run) {
        // A worker belongs to its Tui: never drop one that is still alive.
        if self.run.as_ref().is_some_and(|t| t.worker.is_some()) {
            self.home.home.message = Some(Notice::error(
                "A phase is still running, so the new run cannot be shown. Open it with enter when that phase ends.",
            ));
            return;
        }
        let mut tui = Tui::new(self.cwd, run, &self.settings);
        tui.spawn(|run, tx, cancel| {
            ops::execute(run, ops::Chain::One, tx, cancel).map(NextOutcome::Ran)
        });
        tui.load_caches();
        self.run = Some(tui);
        self.home.reload(true);
        self.show_run();
    }

    fn show_run(&mut self) {
        self.screen = Screen::Run;
        if let Some(tui) = &mut self.run {
            tui.dirty = true;
            // The run may have changed while the home screen was shown.
            tui.last_outside_check = None;
        }
    }
}

impl<'a> Tui<'a> {
    fn new(cwd: &'a Path, run: Run, settings: &Settings) -> Tui<'a> {
        let mut app = App::new(data::load_run_view(&run));
        app.settings = settings.clone();
        Tui {
            cwd,
            app,
            stamp: RunStamp::read(run.git.root()),
            run,
            bell: false,
            worker: None,
            last_refresh: Instant::now(),
            refresh_pending: false,
            last_outside_check: None,
            gone: false,
            dirty: true,
        }
    }
}

impl Tui<'_> {
    /// Runs a command of the run screen; quitting and going home belong to
    /// the loop.
    fn execute(&mut self, command: Command) {
        match command {
            Command::Quit | Command::Home | Command::SaveSettings(_) => {}
            Command::Reload => self.refresh(),
            Command::Interrupt => {
                self.interrupt();
                self.app.message = Some(Notice::info(match self.app.running {
                    Some(n) => format!("Interrupting phase {n}…"),
                    None => "Interrupting…".to_string(),
                }));
            }
            Command::Next => self.spawn(|run, tx, cancel| {
                ops::next(run, ops::Chain::One, &ops::Overrides::default(), tx, cancel)
            }),
            Command::Retry(note) => self.spawn(move |run, tx, cancel| {
                ops::retry(run, note.as_deref(), &ops::Overrides::default(), tx, cancel)
                    .map(NextOutcome::Ran)
            }),
            Command::Merge => {
                self.spawn_finished(|run, _tx, _cancel| ops::merge(run).map(Finished::Merged));
                if self.worker.is_some() {
                    self.app.message = Some(Notice::info("Merging…"));
                }
            }
            Command::Rollback(k) => self.rollback(k),
            Command::Abandon => self.abandon(),
        }
        self.dirty = true;
    }

    fn interrupt(&self) {
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn spawn<F>(&mut self, work: F)
    where
        F: FnOnce(&mut Run, &mpsc::Sender<EngineEvent>, &AtomicBool) -> Result<NextOutcome>
            + Send
            + 'static,
    {
        self.spawn_finished(move |run, tx, cancel| work(run, tx, cancel).map(Finished::Phase));
    }

    fn spawn_finished<F>(&mut self, work: F)
    where
        F: FnOnce(&mut Run, &mpsc::Sender<EngineEvent>, &AtomicBool) -> Result<Finished>
            + Send
            + 'static,
    {
        if self.worker.is_some() {
            return;
        }
        let mut run = match ops::load_current(self.cwd) {
            Ok(Some(run)) => run,
            Ok(None) => {
                self.app.message = Some(Notice::error(format!("Error: {NO_RUN}")));
                return;
            }
            Err(err) => {
                self.app.message = Some(Notice::error(format!("Error: {err}")));
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (tx, events) = mpsc::channel();
        let handle = thread::spawn(move || work(&mut run, &tx, &worker_cancel));
        self.worker = Some(Worker {
            handle,
            cancel,
            events,
        });
    }

    fn rollback(&mut self, k: usize) {
        if self.worker.is_some() {
            self.app.message = Some(Notice::warn(
                "A phase is running; wait for it to finish or press x to interrupt it.",
            ));
            return;
        }
        let result = ops::load_current(self.cwd)
            .and_then(|run| run.ok_or_else(|| anyhow!(NO_RUN)))
            .and_then(|mut run| ops::rollback(&mut run, k, None));
        self.refresh();
        self.app.message = Some(match result {
            Ok(()) if k == 0 => Notice::ok("Rolled back to the initial state."),
            Ok(()) => Notice::ok(format!("Rolled back to the checkpoint of phase {k}.")),
            Err(err) => Notice::error(format!("Error: {err}")),
        });
    }

    /// `ops::abort` is quick, so it runs right here.
    fn abandon(&mut self) {
        if self.worker.is_some() {
            self.app.message = Some(Notice::warn("Interrupt the running phase first (x)."));
            return;
        }
        let result = ops::load_current(self.cwd)
            .and_then(|run| run.ok_or_else(|| anyhow!(NO_RUN)))
            .and_then(|mut run| ops::abort(&mut run).map(|outcome| (outcome, run)));
        self.refresh();
        self.app.message = Some(match result {
            Ok((outcome, run)) => abandon_notice(&outcome, &run.state),
            Err(err) => Notice::error(format!("Error: {err}")),
        });
    }

    fn drain_events(&mut self) {
        let Some(worker) = &self.worker else { return };
        let mut changed = false;
        loop {
            match worker.events.try_recv() {
                Ok(EngineEvent::PhaseStarted { phase, .. }) => {
                    // A new phase is selected; a retry of the running one
                    // leaves the user's selection alone.
                    if self.app.running != Some(phase) {
                        self.app.follow(phase);
                    }
                    self.app.running = Some(phase);
                    changed = true;
                }
                Ok(EngineEvent::PhaseFinished { .. }) => {
                    self.app.running = None;
                    changed = true;
                }
                Ok(_) => changed = true,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if changed {
            self.refresh_pending = true;
            self.dirty = true;
        }
    }

    /// When the worker has ended: joins it, reloads and reports the outcome.
    fn finish_worker(&mut self) {
        if !self.worker.as_ref().is_some_and(|w| w.handle.is_finished()) {
            return;
        }
        let Some(worker) = self.worker.take() else {
            return;
        };
        let result = worker.handle.join().unwrap_or_else(|_| {
            Err(anyhow!(
                "The engine thread panicked. This is a bug; please report it."
            ))
        });
        self.app.running = None;
        self.refresh();
        // A phase that needs the user, or the end of the run.
        self.bell = matches!(result, Ok(Finished::Phase(_)));
        self.app.message = Some(match result {
            Ok(Finished::Merged(outcome)) => merge_notice(&outcome),
            Ok(Finished::Phase(outcome)) => outcome_notice(&Ok(outcome), &self.run.state),
            Err(err) => outcome_notice(&Err(err), &self.run.state),
        });
    }

    /// Reloads the run when another process changed it, so the screen
    /// follows `next`, `abort` or `merge` run in another terminal without a
    /// key press. A phase of this screen's own worker is left alone: its
    /// events refresh the screen.
    fn follow_outside_changes(&mut self) {
        if self.worker.is_some()
            || self
                .last_outside_check
                .is_some_and(|at| at.elapsed() < OUTSIDE_CHECK_EVERY)
        {
            return;
        }
        self.last_outside_check = Some(Instant::now());
        if RunStamp::read(self.run.git.root()) != self.stamp {
            self.refresh();
        }
    }

    /// Reloads the run and the caches of the selected phase.
    fn refresh(&mut self) {
        self.dirty = true;
        self.refresh_pending = false;
        self.last_refresh = Instant::now();
        // Taken before the load: a change made while loading is seen at the
        // next check, never lost.
        self.stamp = RunStamp::read(self.run.git.root());
        match ops::load_current(self.cwd) {
            Ok(Some(run)) => {
                self.run = run;
                self.app.set_view(data::load_run_view(&self.run));
            }
            Ok(None) => self.gone = true,
            Err(err) => self.app.message = Some(Notice::error(format!("Error: {err}"))),
        }
        self.load_caches();
    }

    fn load_caches(&mut self) {
        let Some(number) = self.app.selected_number() else {
            return;
        };
        let diff = data::load_diff(&self.run, number).unwrap_or_else(|err| data::DiffView {
            note: Some(format!("Cannot load the diff: {err}")),
            ..data::DiffView::default()
        });
        let follow = self.app.tab == Tab::Log
            && self.app.running == Some(number)
            && at_log_end(self.app.scroll, self.app.log.len(), self.app.page);
        let log = data::load_log(&self.run, number);
        let verify = data::load_verify(&self.run, number);
        self.app.set_caches(diff, log, verify);
        if follow {
            // The view clamps the scroll to the end of the content.
            self.app.scroll = usize::MAX;
        }
    }
}

/// True when the view shows the end of a log of `len` lines (so it follows it).
fn at_log_end(scroll: usize, len: usize, page: u16) -> bool {
    scroll >= len.saturating_sub(usize::from(page))
}

/// The message shown when the run was abandoned.
fn abandon_notice(outcome: &AbortOutcome, state: &RunState) -> Notice {
    let place = if outcome.switched {
        format!("Back on {}", outcome.current_branch)
    } else {
        format!("Still on {}", outcome.current_branch)
    };
    let saved = outcome
        .leftovers_sha
        .as_ref()
        .map(|sha| format!("; uncommitted changes saved in {sha}"))
        .unwrap_or_default();
    Notice::ok(format!(
        "Run abandoned. {place}; branch {} kept{saved}.",
        state.run_branch
    ))
}

/// The message shown when a merge ends.
fn merge_notice(outcome: &MergeOutcome) -> Notice {
    match &outcome.commit {
        Some(sha) => Notice::ok(format!(
            "Merged: committed {sha} on {}. The run branch {} is kept.",
            outcome.branch, outcome.run_branch
        )),
        None => Notice::info(format!(
            "Nothing to merge: the work of {} is already in {}.",
            outcome.run_branch, outcome.branch
        )),
    }
}

/// The message shown when a worker ends.
fn outcome_notice(result: &Result<NextOutcome>, state: &RunState) -> Notice {
    match result {
        Err(err) => Notice::error(format!("Error: {err}")),
        Ok(NextOutcome::Completed) => Notice::ok(format!(
            "All phases done. Press m to merge, or: git switch {} && git merge --squash {} && git commit",
            state.original_branch, state.run_branch
        )),
        Ok(NextOutcome::Ran(PhaseStatus::Failed)) => {
            match state
                .phases
                .iter()
                .find(|p| p.status == PhaseStatus::Failed)
            {
                Some(phase) => match &phase.failure {
                    Some(failure) => Notice::error(format!(
                        "Phase {} failed ({}): {}",
                        phase.number,
                        failure_kind_label(failure.kind),
                        first_line_short(&failure.detail)
                    )),
                    None => Notice::error(format!("Phase {} failed.", phase.number)),
                },
                None => Notice::error("The phase failed."),
            }
        }
        Ok(NextOutcome::Ran(_)) => {
            let number = state
                .phases
                .iter()
                .rev()
                .find(|p| p.status == PhaseStatus::AwaitingReview)
                .map_or(0, |p| p.number);
            Notice::ok(format!("Phase {number} is ready for review."))
        }
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::plan::AgentKind;
    use crate::state::{Failure, FailureKind, PhaseState, RunStatus};

    fn phase(number: usize, status: PhaseStatus, failure: Option<Failure>) -> PhaseState {
        PhaseState {
            number,
            title: format!("Phase {number}"),
            status,
            attempts: 1,
            session_id: None,
            checkpoint_sha: None,
            start_sha: None,
            summary: None,
            cost_usd: None,
            started_at: None,
            finished_at: None,
            failure,
            model: None,
            effort: None,
            agent_model: None,
        }
    }

    fn state(phases: Vec<PhaseState>) -> RunState {
        RunState {
            schema_version: 1,
            run_id: "r".into(),
            plan_path: "phasewise.md".into(),
            plan_title: None,
            agent: AgentKind::Claude,
            original_branch: "main".into(),
            run_branch: "phasewise/r".into(),
            base_sha: "abc".into(),
            status: RunStatus::Active,
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
            phases,
            model_override: None,
            effort_override: None,
        }
    }

    #[test]
    fn ready_for_review_message_names_the_phase() {
        let s = state(vec![
            phase(1, PhaseStatus::Done, None),
            phase(2, PhaseStatus::AwaitingReview, None),
        ]);
        let msg = outcome_notice(&Ok(NextOutcome::Ran(PhaseStatus::AwaitingReview)), &s);
        assert!(
            msg.text.starts_with("Phase 2 is ready for review"),
            "{msg:?}"
        );
        assert_eq!(msg.level, shared::Level::Ok);
    }

    #[test]
    fn failure_message_has_kind_and_detail() {
        let failure = Failure {
            kind: FailureKind::VerifyFailed,
            detail: "cargo test failed\nmore".into(),
        };
        let s = state(vec![phase(1, PhaseStatus::Failed, Some(failure))]);
        let msg = outcome_notice(&Ok(NextOutcome::Ran(PhaseStatus::Failed)), &s);
        assert_eq!(
            msg.text,
            "Phase 1 failed (verify_failed): cargo test failed"
        );
        assert_eq!(msg.level, shared::Level::Error);
    }

    #[test]
    fn completed_message_has_the_merge_hint() {
        let s = state(vec![phase(1, PhaseStatus::Done, None)]);
        let msg = outcome_notice(&Ok(NextOutcome::Completed), &s);
        assert!(msg.text.contains("All phases done. Press m to merge"));
        assert!(
            msg.text
                .contains("git switch main && git merge --squash phasewise/r && git commit")
        );
    }

    #[test]
    fn merge_message_says_what_was_committed_or_that_nothing_was() {
        let mut outcome = MergeOutcome {
            branch: "main".into(),
            run_branch: "phasewise/r".into(),
            commit: Some("abc1234".into()),
        };
        let done = merge_notice(&outcome);
        assert_eq!(done.level, shared::Level::Ok);
        assert!(done.text.contains("abc1234 on main"), "{}", done.text);
        assert!(done.text.contains("phasewise/r is kept"), "{}", done.text);
        outcome.commit = None;
        let nothing = merge_notice(&outcome);
        assert_eq!(nothing.level, shared::Level::Info);
        assert!(nothing.text.starts_with("Nothing to merge"));
    }

    #[test]
    fn error_message_is_prefixed() {
        let s = state(vec![]);
        let msg = outcome_notice(&Err(anyhow!("boom")), &s);
        assert_eq!(msg, Notice::error("Error: boom"));
    }

    #[test]
    fn the_watchdog_acts_only_on_a_silent_loop_with_a_reason_to_stop() {
        let long = STUCK_AFTER;
        let short = STUCK_AFTER - Duration::from_millis(1);
        assert!(is_stuck(long, true, false));
        assert!(is_stuck(long, false, true));
        assert!(!is_stuck(long, false, false));
        assert!(!is_stuck(short, true, true));
    }

    #[test]
    fn abandon_notice_says_where_the_repository_is() {
        let s = state(vec![]);
        let outcome = AbortOutcome {
            current_branch: "main".into(),
            switched: true,
            leftovers_sha: Some("abc1234".into()),
        };
        assert_eq!(
            abandon_notice(&outcome, &s).text,
            "Run abandoned. Back on main; branch phasewise/r kept; uncommitted changes saved in abc1234."
        );
        let stayed = AbortOutcome {
            current_branch: "other".into(),
            switched: false,
            leftovers_sha: None,
        };
        assert_eq!(
            abandon_notice(&stayed, &s).text,
            "Run abandoned. Still on other; branch phasewise/r kept."
        );
    }

    #[test]
    fn ctrl_c_or_q_while_stopping_force_the_exit() {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        let key = |c, m| KeyEvent::new(KeyCode::Char(c), m);
        assert!(is_force_key(key('c', KeyModifiers::CONTROL)));
        assert!(is_force_key(key('q', KeyModifiers::NONE)));
        assert!(!is_force_key(key('c', KeyModifiers::NONE)));
        assert!(!is_force_key(key('x', KeyModifiers::NONE)));
    }

    /// A repository with a plan and a started run, made without any agent. The
    /// identity is the repository's own: the global configuration is not used.
    fn repo_with_run() -> (tempfile::TempDir, Run) {
        let dir = tempfile::tempdir().expect("temp dir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .envs([
                    ("GIT_CONFIG_GLOBAL", "/dev/null"),
                    ("GIT_CONFIG_NOSYSTEM", "1"),
                ])
                .output()
                .expect("run git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "user.email", "t@example.com"]);
        let plan = dir.path().join("phasewise.md");
        std::fs::write(
            &plan,
            "# Plan\n\n## Phase 1: One\n\nDo one.\n\n## Phase 2: Two\n\nDo two.\n",
        )
        .expect("write plan");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let run = ops::start(dir.path(), &plan, None, &Overrides::default()).expect("start");
        (dir, run)
    }

    /// Another process marks phase 1 done, as `phasewise next` would.
    fn finish_phase_one_from_outside(paths: &RunPaths) {
        let mut state = RunState::load(paths).expect("load state");
        state.phases[0].status = PhaseStatus::Done;
        state.save(paths).expect("save state");
        // Coarse file system clocks can give two writes the same time.
        let file = std::fs::File::options()
            .write(true)
            .open(paths.state_file())
            .expect("open state");
        file.set_modified(SystemTime::now() + Duration::from_secs(60))
            .expect("set time");
    }

    #[test]
    fn the_run_screen_reloads_a_run_changed_by_another_process() {
        let (dir, run) = repo_with_run();
        let outside = run.paths.clone();
        let mut tui = Tui::new(dir.path(), run, &Settings::default());
        assert_eq!(tui.run.state.phases[0].status, PhaseStatus::Pending);

        // Nothing changed: no reload, no redraw.
        tui.dirty = false;
        tui.follow_outside_changes();
        assert!(!tui.dirty);

        finish_phase_one_from_outside(&outside);
        // The check is rate limited, so the screen does not look yet...
        tui.follow_outside_changes();
        assert_eq!(tui.run.state.phases[0].status, PhaseStatus::Pending);
        // ...and follows on the next one.
        tui.last_outside_check = None;
        tui.follow_outside_changes();
        assert_eq!(tui.run.state.phases[0].status, PhaseStatus::Done);
        assert!(tui.dirty);

        // Seen once: the same state is not reloaded again.
        tui.dirty = false;
        tui.last_outside_check = None;
        tui.follow_outside_changes();
        assert!(!tui.dirty);
    }

    #[test]
    fn the_run_screen_leaves_a_running_worker_alone() {
        let (dir, run) = repo_with_run();
        let outside = run.paths.clone();
        let mut tui = Tui::new(dir.path(), run, &Settings::default());
        tui.worker = Some(Worker {
            handle: thread::spawn(|| Ok(Finished::Phase(NextOutcome::Completed))),
            cancel: Arc::new(AtomicBool::new(false)),
            events: mpsc::channel().1,
        });
        finish_phase_one_from_outside(&outside);
        tui.follow_outside_changes();
        assert_eq!(tui.run.state.phases[0].status, PhaseStatus::Pending);
    }

    #[test]
    fn a_run_removed_from_outside_closes_the_run_screen() {
        let (dir, run) = repo_with_run();
        let run_dir = run.paths.dir().to_path_buf();
        let data = data::load_home(dir.path()).expect("home data");
        let mut ui = Ui::new(
            dir.path(),
            Home::new(data),
            Some(run),
            Screen::Run,
            Arc::new(AtomicBool::new(false)),
            Settings::default(),
        );
        ui.pump();
        assert!(ui.run.is_some(), "nothing changed yet");
        std::fs::remove_dir_all(run_dir).expect("remove the run");
        ui.run.as_mut().expect("the run").last_outside_check = None;
        ui.pump();
        assert!(ui.run.is_none());
        assert_eq!(ui.screen, Screen::Home);
        let message = ui.home.home.message.as_ref().expect("a message");
        assert!(
            message.text.contains("no longer exists"),
            "{}",
            message.text
        );
    }

    #[test]
    fn the_stamp_changes_with_the_state_file_and_the_current_run() {
        let (dir, run) = repo_with_run();
        let before = RunStamp::read(dir.path());
        assert_eq!(before, RunStamp::read(dir.path()));
        assert_eq!(before.run_id.as_deref(), Some(run.state.run_id.as_str()));

        finish_phase_one_from_outside(&run.paths);
        let after_write = RunStamp::read(dir.path());
        assert_ne!(before, after_write);

        PhasewiseDir::new(dir.path())
            .write_current("another-run")
            .expect("write current");
        let after_switch = RunStamp::read(dir.path());
        assert_ne!(after_write, after_switch);
        assert_eq!(after_switch.state, None);
    }

    fn home_tui() -> HomeTui<'static> {
        let data = home_view::tests::data(data::PlanState::Missing);
        HomeTui::new(Path::new("/nonexistent"), Home::new(data))
    }

    #[test]
    fn a_panicked_commit_thread_is_reported_and_cleared() {
        let mut tui = home_tui();
        tui.commit = Some(thread::spawn(|| panic!("hook exploded")));
        tui.home.begin_commit();
        while !tui.commit.as_ref().is_some_and(|h| h.is_finished()) {
            thread::sleep(Duration::from_millis(5));
        }
        tui.check_commit(false);
        assert!(tui.commit.is_none());
        assert!(tui.home.committing.is_none());
        let HomeMode::Error(lines) = &tui.home.mode else {
            panic!("expected an error popup");
        };
        assert!(
            lines.iter().any(|l| l.contains("commit thread panicked")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_commit_result_sent_before_the_thread_ended_is_not_a_panic() {
        let mut tui = home_tui();
        let tx = tui.background_tx.clone();
        tui.commit = Some(thread::spawn(move || {
            let _ = tx.send(Background::Committed(Ok("abc1234".into())));
        }));
        tui.home.begin_commit();
        while !tui.commit.as_ref().is_some_and(|h| h.is_finished()) {
            thread::sleep(Duration::from_millis(5));
        }
        tui.check_commit(false);
        assert!(tui.commit.is_none());
        assert_eq!(tui.home.mode, HomeMode::Normal);
    }

    #[test]
    fn a_periodic_reload_is_applied_only_when_fresh_and_idle() {
        let changed = |branch: &str| {
            let mut d = home_view::tests::data(data::PlanState::Missing);
            d.branch = branch.into();
            d
        };
        let mut tui = home_tui();
        tui.reloading = true;
        tui.background_tx
            .send(Background::Reloaded(0, Box::new(Ok(changed("dev")))))
            .expect("send");
        // The home screen is not shown (or a popup is open): dropped.
        tui.drain_background(false, false);
        assert_eq!(
            (tui.reloading, tui.home.data.branch.as_str()),
            (false, "main")
        );
        // Stale: an explicit reload happened meanwhile.
        tui.reload_gen = 1;
        tui.background_tx
            .send(Background::Reloaded(0, Box::new(Ok(changed("dev")))))
            .expect("send");
        tui.drain_background(false, true);
        assert_eq!(tui.home.data.branch, "main");
        // Fresh and idle: applied.
        tui.background_tx
            .send(Background::Reloaded(1, Box::new(Ok(changed("dev")))))
            .expect("send");
        tui.drain_background(false, true);
        assert_eq!(tui.home.data.branch, "dev");
    }

    #[test]
    fn follows_only_when_at_the_end() {
        assert!(at_log_end(0, 5, 10));
        assert!(at_log_end(20, 30, 10));
        assert!(at_log_end(25, 30, 10));
        assert!(!at_log_end(5, 30, 10));
    }
}
