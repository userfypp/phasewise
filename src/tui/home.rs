//! Home screen state machine: keys in, commands out. No terminal, no I/O.

use std::path::Path;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::data::{HomeData, PlanState, RunSummary};
use super::shared::{
    Choice, FooterItem, Notice, ShortcutGroups, confirm_key, digit_row, edit_text, item,
    picker_footer,
};
use crate::agent::{LaunchOptions, ModelInfo};
use crate::display::{EventLine, money, phase_count};
use crate::ops::{Override, Overrides, PlanOutcome, PlanResult};
use crate::plan::{self, AgentKind};

/// Shown for a key refused while the agent writes the plan.
const PLANNING_BUSY: &str = "Wait for the plan or stop it first (esc).";
const RUN_ACTIVE: &str = "A run is active. Open it (enter) to finish or abandon it.";
const COMMITTING: &str = "Wait for the commit to finish.";
const NO_PLAN: &str =
    "There is no plan: press p to write one with the agent, or run `phasewise init`.";
const INVALID_PLAN: &str = "The plan is invalid: fix it in your editor (this screen updates on its own), press p to write one with the agent, or run `phasewise init`.";

/// Result of looking for an agent's version in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Checking,
    Version(String),
    NotInstalled,
}

/// The model list of one agent, loaded on demand and cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelsState {
    NotLoaded,
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

/// A row of the model popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelItem {
    Default,
    /// Id and label of a listed model.
    Model(String, String),
    Custom,
}

/// Confirmation modes hold the highlighted row: 0 is the action, 1 cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Help,
    /// Quit asked while something runs.
    ConfirmQuit(usize),
    /// Stop the plan being written?
    ConfirmStopPlan(usize),
    /// Index into the model popup rows.
    ModelPopup(usize),
    /// Custom model name, being typed.
    ModelInput(String),
    /// Index into the effort popup rows.
    EffortPopup(usize),
    /// The goal for the plan, being typed.
    GoalInput(String),
    /// Replace the existing plan file? Holds the goal.
    ConfirmReplace(String, usize),
    /// The plan is not committed: commit it and start the run?
    ConfirmCommitStart(usize),
    /// Lines of an error, shown until closed.
    Error(Vec<String>),
}

/// What `p` asks the worker to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLaunch {
    pub goal: String,
    pub agent: AgentKind,
    pub options: LaunchOptions,
    /// The plan file existed and the user agreed to replace it.
    pub force: bool,
}

/// What the event loop must do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeCommand {
    Quit,
    /// Start a run with the plan, with the agent shown on the screen (always
    /// passed, so a plan edited on disk meanwhile cannot change it).
    Start {
        agent: AgentKind,
        overrides: Overrides,
    },
    /// Commit the plan file in the background.
    Commit,
    /// Commit the plan, then start the run if the commit succeeded.
    CommitAndStart {
        agent: AgentKind,
        overrides: Overrides,
    },
    /// Show the run screen for the current run.
    OpenRun,
    /// Write a plan with the agent in a worker.
    Plan(PlanLaunch),
    CancelPlan,
    /// Load the model list of the agent in the background.
    LoadModels(AgentKind),
}

/// The planning in progress: its heading and the live event lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Planning {
    pub header: Option<String>,
    pub lines: Vec<EventLine>,
}

/// How a finished planning is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReport {
    /// One line for the status line.
    Success(String),
    /// Lines for the error popup.
    Failure(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct Home {
    pub data: HomeData,
    pub agent: AgentKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    claude: Probe,
    codex: Probe,
    claude_models: ModelsState,
    codex_models: ModelsState,
    pub mode: Mode,
    pub message: Option<Notice>,
    pub planning: Option<Planning>,
    /// When the planning started.
    pub planning_started: Option<Instant>,
    /// When the plan commit started, while it runs.
    pub committing: Option<Instant>,
    /// A phase of the current run is running (set by the loop every turn).
    pub run_busy: bool,
    /// Spinner frame, set by the loop from the elapsed time.
    pub frame: usize,
}

impl Home {
    /// The agent starts as the plan's when the plan is valid, else Claude.
    pub fn new(data: HomeData) -> Home {
        let agent = match &data.plan {
            PlanState::Valid(plan) => plan.agent,
            _ => AgentKind::Claude,
        };
        Home {
            data,
            agent,
            model: None,
            effort: None,
            claude: Probe::Checking,
            codex: Probe::Checking,
            claude_models: ModelsState::NotLoaded,
            codex_models: ModelsState::NotLoaded,
            mode: Mode::Normal,
            message: None,
            planning: None,
            planning_started: None,
            committing: None,
            run_busy: false,
            frame: 0,
        }
    }

    /// Replaces the repository data, keeping the user's choices. True when
    /// the data was different.
    pub fn set_data(&mut self, data: HomeData) -> bool {
        let changed = self.data != data;
        self.data = data;
        changed
    }

    /// The current run, when it is active.
    pub fn active_run(&self) -> Option<&RunSummary> {
        self.data.run.as_ref().filter(|run| run.active())
    }

    /// The id of the run whose branch the repository is on (`phasewise/<id>`).
    pub fn run_branch_id(&self) -> Option<&str> {
        self.data.branch.strip_prefix("phasewise/")
    }

    /// The branch to go back to from a run branch: the original branch of the
    /// run when it is the current one.
    pub fn original_branch(&self) -> &str {
        self.data
            .run
            .as_ref()
            .filter(|run| run.run_branch == self.data.branch)
            .map_or("<your main branch>", |run| run.original_branch.as_str())
    }

    /// On the branch of the current run, and that run is over.
    fn on_finished_run_branch(&self) -> bool {
        self.run_branch_id().is_some()
            && self
                .data
                .run
                .as_ref()
                .is_some_and(|run| run.run_branch == self.data.branch && !run.active())
    }

    /// Refuses a command that must not run on the branch of a run.
    fn refuse_on_run_branch(&mut self) -> Option<HomeCommand> {
        let why = format!(
            "You are on the branch of run {}. Go back with: git switch {}",
            self.run_branch_id().unwrap_or_default(),
            self.original_branch()
        );
        self.refuse(&why)
    }

    /// The plan file as shown: relative to the directory phasewise started in.
    pub fn plan_name(&self) -> String {
        self.data.plan_rel.clone()
    }

    /// The copyable command that commits an uncommitted plan; it works when
    /// pasted in the terminal phasewise was started from.
    pub fn commit_hint(&self) -> String {
        format!(
            "git add {} && git commit -m \"Add phasewise plan\"",
            crate::cli::shell_arg(Path::new(&self.data.plan_rel))
        )
    }

    pub fn probe(&self, kind: AgentKind) -> &Probe {
        match kind {
            AgentKind::Claude => &self.claude,
            AgentKind::Codex => &self.codex,
        }
    }

    pub fn set_probe(&mut self, kind: AgentKind, probe: Probe) {
        match kind {
            AgentKind::Claude => self.claude = probe,
            AgentKind::Codex => self.codex = probe,
        }
    }

    pub fn models(&self, kind: AgentKind) -> &ModelsState {
        match kind {
            AgentKind::Claude => &self.claude_models,
            AgentKind::Codex => &self.codex_models,
        }
    }

    /// Stores a loaded model list (or the error). An open model popup keeps
    /// the row it had selected when that row still exists.
    pub fn set_models(&mut self, kind: AgentKind, result: Result<Vec<ModelInfo>, String>) {
        let keep_row = kind == self.agent && matches!(self.mode, Mode::ModelPopup(_));
        let current = match self.mode {
            Mode::ModelPopup(selected) if keep_row => self.model_items().get(selected).cloned(),
            _ => None,
        };
        let state = match result {
            Ok(models) => ModelsState::Ready(models),
            Err(error) => ModelsState::Failed(error),
        };
        match kind {
            AgentKind::Claude => self.claude_models = state,
            AgentKind::Codex => self.codex_models = state,
        }
        if keep_row {
            let items = self.model_items();
            let selected = current
                .and_then(|item| items.iter().position(|i| *i == item))
                .unwrap_or(0);
            self.mode = Mode::ModelPopup(selected);
        }
    }

    /// Rows of the model popup for the selected agent.
    pub fn model_items(&self) -> Vec<ModelItem> {
        let mut items = vec![ModelItem::Default];
        if let ModelsState::Ready(models) = self.models(self.agent) {
            items.extend(
                models
                    .iter()
                    .map(|m| ModelItem::Model(m.id.clone(), m.name.clone())),
            );
        }
        items.push(ModelItem::Custom);
        items
    }

    /// Note shown in the model popup while the list loads or after it failed.
    pub fn model_status(&self) -> Option<String> {
        match self.models(self.agent) {
            ModelsState::Loading => Some("loading…".to_string()),
            ModelsState::Failed(error) => Some(format!(
                "Cannot list the models: {error} You can still use default or custom…"
            )),
            ModelsState::NotLoaded | ModelsState::Ready(_) => None,
        }
    }

    /// Rows of the effort popup: `default` and the levels.
    pub fn effort_items() -> Vec<Option<&'static str>> {
        std::iter::once(None)
            .chain(plan::EFFORTS.iter().map(|e| Some(*e)))
            .collect()
    }

    /// The agent worker started: from now on the plan panel shows its events.
    pub fn begin_planning(&mut self) {
        self.planning = Some(Planning::default());
        self.planning_started = Some(Instant::now());
    }

    /// Adds what a planning event shows: the heading and/or a line.
    pub fn plan_event(&mut self, header: Option<String>, line: Option<EventLine>) {
        if let Some(planning) = &mut self.planning {
            if header.is_some() {
                planning.header = header;
            }
            planning.lines.extend(line);
        }
    }

    /// The planning could not start (no worker was created).
    pub fn show_error(&mut self, lines: Vec<String>) {
        self.planning = None;
        self.mode = Mode::Error(lines);
    }

    /// The planning ended: reports its outcome.
    pub fn plan_finished(&mut self, report: PlanReport) {
        self.planning = None;
        self.planning_started = None;
        // The quit question stays open: the report becomes a notice, so the
        // user's next key still answers the question.
        let asking = matches!(self.mode, Mode::ConfirmQuit(_));
        match report {
            PlanReport::Success(text) => {
                if !asking {
                    self.mode = Mode::Normal;
                }
                self.message = Some(Notice::ok(text));
            }
            PlanReport::Failure(lines) if asking => {
                let first = lines.first().cloned().unwrap_or_default();
                self.message = Some(Notice::error(first));
            }
            PlanReport::Failure(lines) => self.mode = Mode::Error(lines),
        }
    }

    /// The commit thread started.
    pub fn begin_commit(&mut self) {
        self.committing = Some(Instant::now());
    }

    /// The commit ended: reports it. True when it succeeded.
    pub fn commit_finished(&mut self, result: &Result<String, String>) -> bool {
        self.committing = None;
        match result {
            Ok(sha) => {
                self.message = Some(Notice::ok(format!(
                    "Committed {} ({sha})",
                    self.plan_name()
                )));
                true
            }
            Err(error) => {
                let mut lines = vec![format!("Could not commit {}", self.plan_name())];
                lines.extend(error.lines().map(str::to_string));
                lines.push(String::new());
                lines.push("Commit it yourself:".to_string());
                lines.push(format!("  {}", self.commit_hint()));
                self.mode = Mode::Error(lines);
                false
            }
        }
    }

    /// `ops::start` failed: shows its full text, led by the commit hint when
    /// the plan is not committed.
    pub fn show_start_error(&mut self, err: &anyhow::Error) {
        let mut lines = Vec::new();
        if self.data.plan_uncommitted {
            lines.push(format!(
                "{} is not committed. Commit it first:",
                self.plan_name()
            ));
            lines.push(format!("  {}", self.commit_hint()));
            lines.push(String::new());
        }
        lines.extend(error_lines(err));
        self.mode = Mode::Error(lines);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<HomeCommand> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        self.message = None;
        // Ctrl-C works from every popup and input: it closes it and then acts
        // as in the main view (quit, or ask first while work runs). While the
        // quit question is open it confirms.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if matches!(self.mode, Mode::ConfirmQuit(_)) {
                self.mode = Mode::Normal;
                return Some(HomeCommand::Quit);
            }
            self.mode = Mode::Normal;
        }
        match self.mode.clone() {
            Mode::Normal => self.key_normal(key),
            Mode::Help => {
                // Any key closes the help; `?` is the one that opened it.
                self.mode = Mode::Normal;
                None
            }
            Mode::ConfirmQuit(row) => self.confirm(key, row, Mode::ConfirmQuit, HomeCommand::Quit),
            Mode::ConfirmStopPlan(row) => {
                self.confirm(key, row, Mode::ConfirmStopPlan, HomeCommand::CancelPlan)
            }
            Mode::ModelPopup(selected) => self.key_model_popup(key, selected),
            Mode::ModelInput(buffer) => self.key_model_input(key, buffer),
            Mode::EffortPopup(selected) => self.key_effort_popup(key, selected),
            Mode::GoalInput(buffer) => self.key_goal(key, buffer),
            Mode::ConfirmReplace(goal, row) => {
                let command = HomeCommand::Plan(self.launch(goal.clone(), true));
                self.confirm(key, row, |row| Mode::ConfirmReplace(goal, row), command)
            }
            Mode::ConfirmCommitStart(row) => {
                let command = HomeCommand::CommitAndStart {
                    agent: self.agent,
                    overrides: self.overrides(),
                };
                self.confirm(key, row, Mode::ConfirmCommitStart, command)
            }
            Mode::Error(_) => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'))
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.mode = Mode::Normal;
                }
                None
            }
        }
    }

    /// A two-row confirmation: the key acts, cancels or moves the highlight.
    fn confirm(
        &mut self,
        key: KeyEvent,
        row: usize,
        again: impl FnOnce(usize) -> Mode,
        command: HomeCommand,
    ) -> Option<HomeCommand> {
        match confirm_key(key, row) {
            Choice::Act => {
                self.mode = Mode::Normal;
                Some(command)
            }
            Choice::Cancel => {
                self.mode = Mode::Normal;
                None
            }
            Choice::Row(row) => {
                self.mode = again(row);
                None
            }
        }
    }

    fn launch(&self, goal: String, force: bool) -> PlanLaunch {
        PlanLaunch {
            goal,
            agent: self.agent,
            options: LaunchOptions {
                model: self.model.clone(),
                effort: self.effort.clone(),
            },
            force,
        }
    }

    fn overrides(&self) -> Overrides {
        Overrides {
            model: self.model.clone().map(Override::Set),
            effort: self.effort.clone().map(Override::Set),
        }
    }

    fn refuse(&mut self, why: &str) -> Option<HomeCommand> {
        self.message = Some(Notice::warn(why));
        None
    }

    fn key_normal(&mut self, key: KeyEvent) -> Option<HomeCommand> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            return (key.code == KeyCode::Char('c'))
                .then(|| self.quit())
                .flatten();
        }
        let planning = self.planning.is_some();
        let run_active = self.active_run().is_some();
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                None
            }
            KeyCode::Esc => {
                if planning {
                    self.mode = Mode::ConfirmStopPlan(0);
                }
                None
            }
            KeyCode::Char('x') => {
                if planning {
                    self.mode = Mode::ConfirmStopPlan(0);
                    None
                } else {
                    self.refuse("No plan is being written.")
                }
            }
            KeyCode::Char('o' | 'a' | 'm' | 'e' | 'p' | 'c') | KeyCode::Enter if planning => {
                self.refuse(PLANNING_BUSY)
            }
            KeyCode::Enter if run_active => Some(HomeCommand::OpenRun),
            KeyCode::Char('o') => {
                if self.data.run.is_some() {
                    Some(HomeCommand::OpenRun)
                } else {
                    self.refuse("There is no run to open.")
                }
            }
            KeyCode::Char('a' | 'm' | 'e' | 'p' | 'c') if run_active => self.refuse(RUN_ACTIVE),
            KeyCode::Char('a') => {
                self.agent = match self.agent {
                    AgentKind::Claude => AgentKind::Codex,
                    AgentKind::Codex => AgentKind::Claude,
                };
                // Model names belong to one agent.
                self.model = None;
                None
            }
            KeyCode::Char('m') => self.open_models(),
            KeyCode::Char('e') => {
                let selected = self
                    .effort
                    .as_deref()
                    .and_then(|e| Self::effort_items().iter().position(|i| *i == Some(e)))
                    .unwrap_or(0);
                self.mode = Mode::EffortPopup(selected);
                None
            }
            KeyCode::Char('p') => {
                if self.committing.is_some() {
                    return self.refuse(COMMITTING);
                }
                self.mode = Mode::GoalInput(String::new());
                None
            }
            // Committing the plan from the branch of a finished run first goes back
            // to the run's original branch (see `ops::leave_finished_run`).
            KeyCode::Char('c') if self.on_finished_run_branch() => self.commit(),
            KeyCode::Char('c') | KeyCode::Enter if self.run_branch_id().is_some() => {
                self.refuse_on_run_branch()
            }
            KeyCode::Char('c') => self.commit(),
            KeyCode::Enter => self.start(),
            _ => None,
        }
    }

    /// Whether anything runs that quitting would interrupt.
    fn busy(&self) -> bool {
        self.planning.is_some() || self.run_busy
    }

    fn quit(&mut self) -> Option<HomeCommand> {
        if self.busy() {
            self.mode = Mode::ConfirmQuit(0);
            None
        } else {
            Some(HomeCommand::Quit)
        }
    }

    fn open_models(&mut self) -> Option<HomeCommand> {
        let selected = self
            .model
            .as_ref()
            .and_then(|m| {
                self.model_items()
                    .iter()
                    .position(|i| matches!(i, ModelItem::Model(id, _) if id == m))
            })
            .unwrap_or(0);
        self.mode = Mode::ModelPopup(selected);
        // A failed load is tried again each time the popup opens.
        match self.models(self.agent) {
            ModelsState::NotLoaded | ModelsState::Failed(_) => {
                let agent = self.agent;
                match agent {
                    AgentKind::Claude => self.claude_models = ModelsState::Loading,
                    AgentKind::Codex => self.codex_models = ModelsState::Loading,
                }
                Some(HomeCommand::LoadModels(agent))
            }
            ModelsState::Loading | ModelsState::Ready(_) => None,
        }
    }

    fn commit(&mut self) -> Option<HomeCommand> {
        if self.committing.is_some() {
            return self.refuse(COMMITTING);
        }
        if self.data.plan_uncommitted {
            return Some(HomeCommand::Commit);
        }
        let why = if self.data.plan == PlanState::Missing {
            "There is no plan to commit.".to_string()
        } else {
            format!("{} is already committed.", self.plan_name())
        };
        self.refuse(&why)
    }

    fn start(&mut self) -> Option<HomeCommand> {
        match &self.data.plan {
            PlanState::Valid(_) => {}
            PlanState::Missing => return self.refuse(NO_PLAN),
            PlanState::Invalid(_) => return self.refuse(INVALID_PLAN),
        }
        if self.committing.is_some() {
            return self.refuse(COMMITTING);
        }
        if self.data.plan_uncommitted {
            self.mode = Mode::ConfirmCommitStart(0);
            return None;
        }
        Some(HomeCommand::Start {
            agent: self.agent,
            overrides: self.overrides(),
        })
    }

    fn key_model_popup(&mut self, key: KeyEvent, selected: usize) -> Option<HomeCommand> {
        let items = self.model_items();
        let last = items.len().saturating_sub(1);
        if let Some(row) = digit_row(key) {
            if row <= last {
                self.choose_model(&items, row);
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                self.mode = Mode::ModelPopup((selected + 1).min(last));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.mode = Mode::ModelPopup(selected.saturating_sub(1));
            }
            KeyCode::Enter => self.choose_model(&items, selected.min(last)),
            _ => {}
        }
        None
    }

    fn choose_model(&mut self, items: &[ModelItem], row: usize) {
        match items.get(row) {
            Some(ModelItem::Default) => {
                self.model = None;
                self.mode = Mode::Normal;
            }
            Some(ModelItem::Model(id, _)) => {
                self.model = Some(id.clone());
                self.mode = Mode::Normal;
            }
            Some(ModelItem::Custom) => self.mode = Mode::ModelInput(String::new()),
            None => self.mode = Mode::Normal,
        }
    }

    fn key_model_input(&mut self, key: KeyEvent, mut buffer: String) -> Option<HomeCommand> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let name = buffer.trim();
                if name == "default" {
                    self.model = None;
                    self.mode = Mode::Normal;
                } else {
                    match plan::validate_model(name) {
                        Ok(()) => {
                            self.model = Some(name.to_string());
                            self.mode = Mode::Normal;
                        }
                        Err(why) => {
                            self.message = Some(Notice::warn(format!("Invalid model: {why}.")));
                        }
                    }
                }
            }
            _ => {
                if edit_text(&mut buffer, key) {
                    self.mode = Mode::ModelInput(buffer);
                }
            }
        }
        None
    }

    fn key_effort_popup(&mut self, key: KeyEvent, selected: usize) -> Option<HomeCommand> {
        let items = Self::effort_items();
        let last = items.len() - 1;
        if let Some(row) = digit_row(key) {
            if row <= last {
                self.effort = items[row].map(str::to_string);
                self.mode = Mode::Normal;
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                self.mode = Mode::EffortPopup((selected + 1).min(last));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.mode = Mode::EffortPopup(selected.saturating_sub(1));
            }
            KeyCode::Enter => {
                self.effort = items[selected.min(last)].map(str::to_string);
                self.mode = Mode::Normal;
            }
            _ => {}
        }
        None
    }

    fn key_goal(&mut self, key: KeyEvent, mut buffer: String) -> Option<HomeCommand> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let goal = buffer.trim().to_string();
                if goal.is_empty() {
                    self.message = Some(Notice::warn(
                        "Say what you want to do first, or press Esc to cancel.",
                    ));
                } else if self.data.plan == PlanState::Missing {
                    self.mode = Mode::Normal;
                    return Some(HomeCommand::Plan(self.launch(goal, false)));
                } else {
                    self.mode = Mode::ConfirmReplace(goal, 0);
                }
            }
            _ => {
                if edit_text(&mut buffer, key) {
                    self.mode = Mode::GoalInput(buffer);
                }
            }
        }
        None
    }

    /// True while something on this screen moves: planning, the commit, a
    /// phase of the run, or the model list of an open popup loading.
    pub fn animating(&self) -> bool {
        self.planning.is_some()
            || self.committing.is_some()
            || self.run_busy
            || (matches!(self.mode, Mode::ModelPopup(_))
                && matches!(self.models(self.agent), ModelsState::Loading))
    }

    /// Footer items for the current state, highest priority first. The `?`
    /// item is not in the list: the renderer appends it.
    pub fn footer(&self) -> Vec<FooterItem> {
        match &self.mode {
            Mode::ConfirmQuit(_)
            | Mode::ConfirmStopPlan(_)
            | Mode::ConfirmReplace(..)
            | Mode::ConfirmCommitStart(_) => return picker_footer(2),
            Mode::ModelPopup(_) => return picker_footer(self.model_items().len()),
            Mode::EffortPopup(_) => return picker_footer(Self::effort_items().len()),
            Mode::ModelInput(_) => return vec![item("enter", "use it"), item("esc", "cancel")],
            Mode::GoalInput(_) => {
                return vec![item("enter", "write the plan"), item("esc", "cancel")];
            }
            Mode::Help => return vec![item("any key", "close")],
            Mode::Error(_) => return vec![item("enter", "close")],
            Mode::Normal => {}
        }
        if self.planning.is_some() {
            return vec![item("esc", "stop"), item("q", "quit")];
        }
        if self.active_run().is_some() {
            return vec![item("enter", "open run"), item("q", "quit")];
        }
        let (agent, model, effort) = (item("a", "agent"), item("m", "model"), item("e", "effort"));
        let mut keys = Vec::new();
        match &self.data.plan {
            PlanState::Valid(_) => {
                // Starting does not work on the branch of a run; committing does
                // once the run is over (it goes back to the original branch).
                if self.run_branch_id().is_none() {
                    keys.push(item("enter", "start"));
                }
                if self.data.plan_uncommitted
                    && (self.run_branch_id().is_none() || self.on_finished_run_branch())
                {
                    keys.push(item("c", "commit"));
                }
                keys.extend([item("p", "new plan"), model, agent, effort]);
            }
            PlanState::Missing => keys.extend([item("p", "write a plan"), agent, model, effort]),
            PlanState::Invalid(_) => keys.extend([item("p", "new plan"), agent, model, effort]),
        }
        keys.push(item("q", "quit"));
        keys
    }

    /// Every key of the screen in groups, for the `?` popup.
    pub fn shortcuts() -> ShortcutGroups {
        vec![
            (
                "Run",
                vec![
                    ("enter", "start the run, or open the active one"),
                    ("o", "open the last run"),
                ],
            ),
            (
                "Plan",
                vec![
                    ("p", "write a new plan with the agent"),
                    ("c", "commit the plan"),
                ],
            ),
            (
                "Settings",
                vec![
                    ("a", "switch the agent (resets the model)"),
                    ("m", "choose the model"),
                    ("e", "choose the effort"),
                ],
            ),
            (
                "General",
                vec![
                    ("esc", "stop the plan being written"),
                    ("?", "show or hide the shortcuts"),
                    ("q / Ctrl-C", "quit"),
                ],
            ),
        ]
    }
}

/// The text of an error, one entry per line, with its causes after it.
fn error_lines(err: &anyhow::Error) -> Vec<String> {
    let mut lines: Vec<String> = err.to_string().lines().map(str::to_string).collect();
    for cause in err.chain().skip(1) {
        for (i, line) in cause.to_string().lines().enumerate() {
            lines.push(if i == 0 {
                format!("caused by: {line}")
            } else {
                line.to_string()
            });
        }
    }
    lines
}

/// A path shown relative to the repository root when it is inside it.
fn shown(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Turns what the planning worker returned into what the screen shows.
pub fn plan_report(result: &anyhow::Result<PlanOutcome>, root: &Path) -> PlanReport {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(err) => {
            let mut lines = vec!["phasewise could not write the plan.".to_string()];
            lines.extend(error_lines(err));
            lines.push("Press p to try again.".to_string());
            return PlanReport::Failure(lines);
        }
    };
    match &outcome.result {
        PlanResult::Written { path, plan } => {
            let phases = phase_count(plan.phases.len());
            let cost = outcome
                .cost_usd
                .map(|c| format!(", {}", money(c)))
                .unwrap_or_default();
            PlanReport::Success(format!(
                "Plan written to {} ({phases}{cost}). Review it, commit it, then press Enter.",
                shown(path, root)
            ))
        }
        PlanResult::Failed {
            problems,
            last_draft,
            cancelled,
        } => {
            let mut lines = vec![
                if *cancelled {
                    "Planning was interrupted. No plan was written."
                } else {
                    "phasewise could not get a valid plan. Nothing was written."
                }
                .to_string(),
            ];
            lines.extend(problems.iter().map(|p| format!("- {p}")));
            if let Some(draft) = last_draft {
                lines.push(format!(
                    "Last draft: {} (fix it by hand and check it with `phasewise check`).",
                    shown(draft, root)
                ));
            }
            lines.push(format!(
                "Agent output and drafts: {}. Press p to try again.",
                shown(&outcome.dir, root)
            ));
            if let Some(cost) = outcome.cost_usd {
                lines.push(format!("Cost: {}", money(cost)));
            }
            PlanReport::Failure(lines)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::state::{PhaseStatus, RunStatus};
    use crate::tui::data::{HomePhase, PlanProblem, PlanSummary, RunPhaseSummary};
    use crate::tui::shared::Level;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(home: &mut Home, text: &str) {
        for c in text.chars() {
            home.handle_key(key(c));
        }
    }

    fn summary(agent: AgentKind) -> PlanSummary {
        PlanSummary {
            title: Some("Demo".into()),
            agent,
            model: Some("opus".into()),
            effort: Some("high".into()),
            phases: vec![HomePhase {
                agent: None,
                number: 1,
                title: "First".into(),
                checks: 1,
                model: Some("opus".into()),
                effort: Some("high".into()),
            }],
        }
    }

    fn data(plan: PlanState) -> HomeData {
        HomeData {
            root: PathBuf::from("/work/demo"),
            repo_name: "demo".into(),
            branch: "main".into(),
            plan_path: PathBuf::from("/work/demo/phasewise.md"),
            plan,
            plan_uncommitted: false,
            plan_rel: "phasewise.md".into(),
            run: None,
        }
    }

    fn text(h: &Home) -> Option<&str> {
        h.message.as_ref().map(|n| n.text.as_str())
    }

    fn home_with(plan: PlanState) -> Home {
        Home::new(data(plan))
    }

    fn valid() -> Home {
        home_with(PlanState::Valid(summary(AgentKind::Claude)))
    }

    fn models() -> Vec<ModelInfo> {
        ["opus", "sonnet"]
            .iter()
            .map(|id| ModelInfo {
                id: (*id).into(),
                name: id.to_uppercase(),
                description: String::new(),
                efforts: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn initial_agent_is_the_plans_or_claude() {
        assert_eq!(
            home_with(PlanState::Valid(summary(AgentKind::Codex))).agent,
            AgentKind::Codex
        );
        assert_eq!(valid().agent, AgentKind::Claude);
        assert_eq!(home_with(PlanState::Missing).agent, AgentKind::Claude);
        let invalid = home_with(PlanState::Invalid(vec![PlanProblem {
            line: 1,
            message: "x".into(),
        }]));
        assert_eq!(invalid.agent, AgentKind::Claude);
    }

    #[test]
    fn reload_keeps_the_choices() {
        let mut h = home_with(PlanState::Missing);
        h.handle_key(key('a'));
        h.effort = Some("low".into());
        h.set_data(data(PlanState::Valid(summary(AgentKind::Claude))));
        assert_eq!(h.agent, AgentKind::Codex);
        assert_eq!(h.effort.as_deref(), Some("low"));
    }

    #[test]
    fn agent_toggle_resets_the_model_and_keeps_the_effort() {
        let mut h = valid();
        h.model = Some("opus".into());
        h.effort = Some("high".into());
        assert_eq!(h.handle_key(key('a')), None);
        assert_eq!((h.agent, h.model.clone()), (AgentKind::Codex, None));
        assert_eq!(h.effort.as_deref(), Some("high"));
        h.handle_key(key('a'));
        assert_eq!(h.agent, AgentKind::Claude);
    }

    #[test]
    fn model_popup_requests_the_list_once_and_caches_it() {
        let mut h = valid();
        assert_eq!(
            h.handle_key(key('m')),
            Some(HomeCommand::LoadModels(AgentKind::Claude))
        );
        assert_eq!(h.mode, Mode::ModelPopup(0));
        assert_eq!(h.model_status().as_deref(), Some("loading…"));
        // Only default and custom while loading.
        assert_eq!(h.model_items(), [ModelItem::Default, ModelItem::Custom]);
        h.set_models(AgentKind::Claude, Ok(models()));
        assert_eq!(h.model_items().len(), 4);
        assert_eq!(h.model_status(), None);
        h.handle_key(code(KeyCode::Esc));
        assert_eq!(h.mode, Mode::Normal);
        // Cached: no new request.
        assert_eq!(h.handle_key(key('m')), None);
        // The other agent has its own cache.
        h.handle_key(code(KeyCode::Esc));
        h.handle_key(key('a'));
        assert_eq!(
            h.handle_key(key('m')),
            Some(HomeCommand::LoadModels(AgentKind::Codex))
        );
    }

    #[test]
    fn model_popup_picks_a_listed_model_default_or_navigates_with_clamping() {
        let mut h = valid();
        h.handle_key(key('m'));
        h.set_models(AgentKind::Claude, Ok(models()));
        h.handle_key(key('j'));
        h.handle_key(code(KeyCode::Down));
        assert_eq!(h.mode, Mode::ModelPopup(2));
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(h.model.as_deref(), Some("sonnet"));
        assert_eq!(h.mode, Mode::Normal);

        // The popup opens on the current model.
        h.handle_key(key('m'));
        assert_eq!(h.mode, Mode::ModelPopup(2));
        h.handle_key(key('k'));
        h.handle_key(code(KeyCode::Up));
        h.handle_key(code(KeyCode::Up));
        assert_eq!(h.mode, Mode::ModelPopup(0));
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.model, None);
        h.handle_key(key('m'));
        for _ in 0..9 {
            h.handle_key(key('j'));
        }
        assert_eq!(h.mode, Mode::ModelPopup(3));
    }

    #[test]
    fn a_list_arriving_keeps_the_selected_row() {
        let mut h = valid();
        h.handle_key(key('m'));
        h.handle_key(key('j'));
        assert_eq!(h.mode, Mode::ModelPopup(1));
        h.set_models(AgentKind::Claude, Ok(models()));
        // `custom…` moved from row 1 to row 3 and stays selected.
        assert_eq!(h.mode, Mode::ModelPopup(3));
    }

    #[test]
    fn a_failed_list_still_offers_default_and_custom_and_is_retried() {
        let mut h = valid();
        h.handle_key(key('m'));
        h.set_models(AgentKind::Claude, Err("boom".into()));
        assert_eq!(h.model_items(), [ModelItem::Default, ModelItem::Custom]);
        assert!(h.model_status().is_some_and(|s| s.contains("boom")));
        h.handle_key(code(KeyCode::Esc));
        assert_eq!(
            h.handle_key(key('m')),
            Some(HomeCommand::LoadModels(AgentKind::Claude))
        );
    }

    #[test]
    fn custom_model_is_validated_and_default_clears_it() {
        let mut h = valid();
        h.handle_key(key('m'));
        h.handle_key(key('j'));
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.mode, Mode::ModelInput(String::new()));
        // Empty: refused, input stays.
        h.handle_key(code(KeyCode::Enter));
        assert!(matches!(h.mode, Mode::ModelInput(_)));
        assert!(text(&h).is_some_and(|m| m.starts_with("Invalid model")));
        type_text(&mut h, "bad name");
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.mode, Mode::ModelInput("bad name".into()));
        assert!(h.message.is_some());
        for _ in 0..8 {
            h.handle_key(code(KeyCode::Backspace));
        }
        type_text(&mut h, "-x");
        h.handle_key(code(KeyCode::Enter));
        assert!(matches!(h.mode, Mode::ModelInput(_)));
        h.handle_key(code(KeyCode::Backspace));
        h.handle_key(code(KeyCode::Backspace));
        type_text(&mut h, "my-model");
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(h.model.as_deref(), Some("my-model"));
        assert_eq!(h.mode, Mode::Normal);

        h.handle_key(key('m'));
        h.handle_key(key('j'));
        h.handle_key(code(KeyCode::Enter));
        type_text(&mut h, "default");
        h.handle_key(code(KeyCode::Enter));
        assert_eq!((h.model.clone(), h.mode.clone()), (None, Mode::Normal));
    }

    #[test]
    fn esc_closes_popups_without_changes() {
        let mut h = valid();
        h.model = Some("opus".into());
        h.effort = Some("low".into());
        h.handle_key(key('m'));
        h.handle_key(key('j'));
        h.handle_key(code(KeyCode::Esc));
        assert_eq!(h.mode, Mode::Normal);
        h.handle_key(key('m'));
        h.handle_key(key('j'));
        h.handle_key(code(KeyCode::Enter));
        type_text(&mut h, "zzz");
        h.handle_key(code(KeyCode::Esc));
        assert_eq!(h.mode, Mode::Normal);
        h.handle_key(key('e'));
        h.handle_key(key('j'));
        h.handle_key(code(KeyCode::Esc));
        assert_eq!(h.mode, Mode::Normal);
        assert_eq!(h.model.as_deref(), Some("opus"));
        assert_eq!(h.effort.as_deref(), Some("low"));
    }

    #[test]
    fn effort_popup_offers_default_and_the_levels() {
        let mut h = valid();
        assert_eq!(Home::effort_items().len(), 6);
        h.handle_key(key('e'));
        assert_eq!(h.mode, Mode::EffortPopup(0));
        h.handle_key(code(KeyCode::Down));
        h.handle_key(code(KeyCode::Down));
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.effort.as_deref(), Some("medium"));
        // Opens on the current level; default clears it.
        h.handle_key(key('e'));
        assert_eq!(h.mode, Mode::EffortPopup(2));
        h.handle_key(key('k'));
        h.handle_key(key('k'));
        h.handle_key(key('k'));
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.effort, None);
        h.handle_key(key('e'));
        for _ in 0..10 {
            h.handle_key(key('j'));
        }
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.effort.as_deref(), Some("max"));
    }

    #[test]
    fn plan_input_edits_and_refuses_an_empty_goal() {
        let mut h = home_with(PlanState::Missing);
        h.handle_key(key('p'));
        assert_eq!(h.mode, Mode::GoalInput(String::new()));
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert!(h.message.is_some());
        assert!(matches!(h.mode, Mode::GoalInput(_)));
        type_text(&mut h, "   ");
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert!(matches!(h.mode, Mode::GoalInput(_)));
        type_text(&mut h, "add a CLI!");
        h.handle_key(code(KeyCode::Backspace));
        assert_eq!(h.mode, Mode::GoalInput("   add a CLI".into()));
        h.handle_key(code(KeyCode::Esc));
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn plan_without_an_existing_file_starts_with_the_current_settings() {
        let mut h = home_with(PlanState::Missing);
        h.model = Some("opus".into());
        h.handle_key(key('a'));
        h.effort = Some("high".into());
        h.handle_key(key('p'));
        type_text(&mut h, "  add a CLI ");
        assert_eq!(
            h.handle_key(code(KeyCode::Enter)),
            Some(HomeCommand::Plan(PlanLaunch {
                goal: "add a CLI".into(),
                agent: AgentKind::Codex,
                options: LaunchOptions {
                    model: None,
                    effort: Some("high".into()),
                },
                force: false,
            }))
        );
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn plan_over_an_existing_file_asks_and_y_or_1_confirms() {
        for existing in [
            PlanState::Valid(summary(AgentKind::Claude)),
            PlanState::Invalid(Vec::new()),
        ] {
            for yes in ['y', 'Y', '1'] {
                let mut h = home_with(existing.clone());
                h.handle_key(key('p'));
                type_text(&mut h, "goal");
                assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
                assert_eq!(h.mode, Mode::ConfirmReplace("goal".into(), 0));
                match h.handle_key(key(yes)) {
                    Some(HomeCommand::Plan(launch)) => {
                        assert!(launch.force);
                        assert_eq!(launch.goal, "goal");
                    }
                    other => panic!("expected a plan command, got {other:?}"),
                }
                assert_eq!(h.mode, Mode::Normal);
            }
            for no in [key('n'), key('2'), code(KeyCode::Esc)] {
                let mut h = home_with(existing.clone());
                h.handle_key(key('p'));
                type_text(&mut h, "goal");
                h.handle_key(code(KeyCode::Enter));
                assert_eq!(h.handle_key(no), None);
                assert_eq!(h.mode, Mode::Normal);
            }
        }
    }

    #[test]
    fn keys_are_refused_while_planning_but_esc_x_and_help_work() {
        let mut h = valid();
        h.begin_planning();
        for c in ['a', 'm', 'e', 'p', 'c', 'o'] {
            assert_eq!(h.handle_key(key(c)), None);
            assert_eq!(text(&h), Some(PLANNING_BUSY), "key {c}");
            assert_eq!(h.mode, Mode::Normal);
        }
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(text(&h), Some(PLANNING_BUSY));
        assert_eq!(h.agent, AgentKind::Claude);
        for stop in [key('x'), code(KeyCode::Esc)] {
            assert_eq!(h.handle_key(stop), None);
            assert_eq!(h.mode, Mode::ConfirmStopPlan(0));
            assert_eq!(h.handle_key(key('y')), Some(HomeCommand::CancelPlan));
            assert_eq!(h.mode, Mode::Normal);
        }
        h.handle_key(key('x'));
        assert_eq!(h.handle_key(key('n')), None);
        assert_eq!(h.mode, Mode::Normal);
        h.handle_key(key('?'));
        assert_eq!(h.mode, Mode::Help);
        h.handle_key(key('?'));
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn esc_does_nothing_on_home_without_planning() {
        let mut h = valid();
        assert_eq!(h.handle_key(code(KeyCode::Esc)), None);
        assert_eq!(h.mode, Mode::Normal);
        assert!(h.message.is_none());
    }

    #[test]
    fn x_without_planning_is_refused() {
        let mut h = valid();
        assert_eq!(h.handle_key(key('x')), None);
        assert!(h.message.is_some());
    }

    #[test]
    fn ctrl_c_quits_from_popups_and_inputs() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut h = valid();
        h.mode = Mode::GoalInput("half a goal".into());
        assert_eq!(h.handle_key(ctrl_c), Some(HomeCommand::Quit));
        assert_eq!(h.mode, Mode::Normal);
        h.mode = Mode::Error(vec!["boom".into()]);
        assert_eq!(h.handle_key(ctrl_c), Some(HomeCommand::Quit));
        // While planning it still asks first.
        h.begin_planning();
        h.mode = Mode::EffortPopup(0);
        assert_eq!(h.handle_key(ctrl_c), None);
        assert_eq!(h.mode, Mode::ConfirmQuit(0));
    }

    #[test]
    fn quit_is_immediate_when_idle_and_confirmed_while_working() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut h = valid();
        assert_eq!(h.handle_key(key('q')), Some(HomeCommand::Quit));
        assert_eq!(h.handle_key(ctrl_c), Some(HomeCommand::Quit));
        h.begin_planning();
        assert_eq!(h.handle_key(key('q')), None);
        assert_eq!(h.mode, Mode::ConfirmQuit(0));
        assert_eq!(h.handle_key(key('y')), Some(HomeCommand::Quit));
        h.handle_key(key('q'));
        assert_eq!(h.handle_key(key('n')), None);
        assert_eq!(h.mode, Mode::Normal);
        assert_eq!(h.handle_key(ctrl_c), None);
        assert_eq!(h.mode, Mode::ConfirmQuit(0));
        // Ctrl-C while the question is open confirms.
        assert_eq!(h.handle_key(ctrl_c), Some(HomeCommand::Quit));
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn other_control_keys_do_nothing() {
        let mut h = valid();
        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(h.handle_key(ctrl_a), None);
        assert_eq!(h.agent, AgentKind::Claude);
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn enter_is_refused_without_a_valid_plan() {
        let mut missing = home_with(PlanState::Missing);
        assert_eq!(missing.handle_key(code(KeyCode::Enter)), None);
        assert!(text(&missing).is_some_and(|m| {
            m.contains("press p to write one with the agent, or run `phasewise init`")
        }));
        let mut invalid = home_with(PlanState::Invalid(Vec::new()));
        assert_eq!(invalid.handle_key(code(KeyCode::Enter)), None);
        assert!(text(&invalid).is_some_and(|m| {
            m.contains("press p to write one with the agent, or run `phasewise init`")
        }));
    }

    #[test]
    fn enter_starts_with_overrides_only_for_what_was_chosen() {
        let mut h = valid();
        assert_eq!(
            h.handle_key(code(KeyCode::Enter)),
            Some(HomeCommand::Start {
                agent: AgentKind::Claude,
                overrides: Overrides::default(),
            })
        );
        h.handle_key(key('a'));
        h.model = Some("gpt".into());
        h.effort = Some("low".into());
        assert_eq!(
            h.handle_key(code(KeyCode::Enter)),
            Some(HomeCommand::Start {
                agent: AgentKind::Codex,
                overrides: Overrides {
                    model: Some(Override::Set("gpt".into())),
                    effort: Some(Override::Set("low".into())),
                },
            })
        );
        // A codex plan starts with codex unless the user switches.
        let mut codex = home_with(PlanState::Valid(summary(AgentKind::Codex)));
        assert_eq!(codex.agent, AgentKind::Codex);
        assert_eq!(
            codex.handle_key(code(KeyCode::Enter)),
            Some(HomeCommand::Start {
                agent: AgentKind::Codex,
                overrides: Overrides::default(),
            })
        );
    }

    #[test]
    fn quit_asks_while_a_phase_of_the_run_runs() {
        let mut h = valid();
        h.run_busy = true;
        assert_eq!(h.handle_key(key('q')), None);
        assert_eq!(h.mode, Mode::ConfirmQuit(0));
        assert_eq!(h.handle_key(code(KeyCode::Enter)), Some(HomeCommand::Quit));
    }

    #[test]
    fn error_popup_closes_with_esc_enter_or_q_and_q_does_not_quit() {
        for close in [code(KeyCode::Esc), code(KeyCode::Enter), key('q')] {
            let mut h = valid();
            h.show_error(vec!["boom".into()]);
            assert_eq!(h.handle_key(close), None);
            assert_eq!(h.mode, Mode::Normal);
        }
        let mut h = valid();
        h.show_error(vec!["boom".into()]);
        for ignored in [key('a'), key('x'), key('p'), code(KeyCode::Tab)] {
            assert_eq!(h.handle_key(ignored), None);
            assert!(matches!(h.mode, Mode::Error(_)));
        }
        assert_eq!(h.agent, AgentKind::Claude);
    }

    #[test]
    fn help_closes_on_any_key() {
        let mut h = valid();
        h.handle_key(key('?'));
        assert_eq!(h.mode, Mode::Help);
        assert_eq!(h.handle_key(key('q')), None);
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn message_clears_on_the_next_key() {
        let mut h = home_with(PlanState::Missing);
        h.handle_key(code(KeyCode::Enter));
        assert!(h.message.is_some());
        h.handle_key(key('z'));
        assert!(h.message.is_none());
    }

    #[test]
    fn start_error_is_shown_in_full_with_the_commit_hint_for_an_uncommitted_plan() {
        let mut h = valid();
        h.data.plan_uncommitted = true;
        let err = anyhow::anyhow!(
            "The working tree has uncommitted changes:\n  phasewise.md\nCommit it."
        )
        .context("outer");
        h.show_start_error(&err);
        let Mode::Error(lines) = &h.mode else {
            panic!("expected an error popup");
        };
        assert_eq!(lines[0], "phasewise.md is not committed. Commit it first:");
        assert_eq!(
            lines[1],
            "  git add phasewise.md && git commit -m \"Add phasewise plan\""
        );
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "outer");
        assert_eq!(
            lines[4],
            "caused by: The working tree has uncommitted changes:"
        );
        assert_eq!(lines[5], "  phasewise.md");
        assert_eq!(lines[6], "Commit it.");

        let mut committed = valid();
        committed.show_start_error(&anyhow::anyhow!("a\nb"));
        assert_eq!(committed.mode, Mode::Error(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn a_finished_planning_does_not_close_the_quit_question() {
        for report in [
            PlanReport::Success("ok".into()),
            PlanReport::Failure(vec!["bad".into(), "more".into()]),
        ] {
            let mut h = valid();
            h.begin_planning();
            h.handle_key(key('q'));
            assert_eq!(h.mode, Mode::ConfirmQuit(0));
            h.plan_finished(report);
            assert_eq!(h.mode, Mode::ConfirmQuit(0));
            assert!(h.message.is_some());
            assert_eq!(h.planning, None);
        }
        let mut h = valid();
        h.begin_planning();
        h.handle_key(key('x'));
        h.plan_finished(PlanReport::Success("ok".into()));
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn plan_events_accumulate_and_finishing_reports() {
        let mut h = valid();
        h.plan_event(Some("▶ Planning".into()), None);
        assert_eq!(h.planning, None, "ignored before the worker starts");
        h.begin_planning();
        h.plan_event(Some("▶ Planning · claude".into()), None);
        let line = crate::display::plan_event_line(&crate::ops::PlanEvent::Written {
            path: "phasewise.md".into(),
        })
        .map(|l| l.at(3));
        h.plan_event(None, line.clone());
        let planning = h.planning.clone().expect("planning");
        assert_eq!(planning.header.as_deref(), Some("▶ Planning · claude"));
        assert_eq!(planning.lines, line.into_iter().collect::<Vec<_>>());
        h.plan_finished(PlanReport::Success("ok".into()));
        assert_eq!((h.planning.clone(), text(&h)), (None, Some("ok")));
        assert_eq!(h.message.as_ref().map(|n| n.level), Some(Level::Ok));
        h.begin_planning();
        h.plan_finished(PlanReport::Failure(vec!["bad".into()]));
        assert_eq!(h.mode, Mode::Error(vec!["bad".into()]));
        assert_eq!(h.planning, None);
    }

    fn keys(h: &Home) -> Vec<String> {
        h.footer().into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn footer_follows_the_state() {
        let mut h = valid();
        assert_eq!(keys(&h), ["enter", "p", "m", "a", "e", "q"]);
        h.data.plan_uncommitted = true;
        assert_eq!(keys(&h), ["enter", "c", "p", "m", "a", "e", "q"]);
        assert_eq!(
            keys(&home_with(PlanState::Missing)),
            ["p", "a", "m", "e", "q"]
        );
        assert_eq!(
            keys(&home_with(PlanState::Invalid(Vec::new()))),
            ["p", "a", "m", "e", "q"]
        );
        h.begin_planning();
        assert_eq!(keys(&h), ["esc", "q"]);
        h.mode = Mode::ConfirmQuit(0);
        assert_eq!(keys(&h), ["enter", "1–2", "esc"]);
        h.mode = Mode::Error(Vec::new());
        assert_eq!(keys(&h), ["enter"]);
        let mut active = valid();
        active.data.run = Some(run_summary(RunStatus::Active));
        assert_eq!(keys(&active), ["enter", "q"]);
        let mut popup = valid();
        popup.mode = Mode::EffortPopup(0);
        assert_eq!(keys(&popup), ["enter", "1–6", "esc"]);
        assert!(keys(&valid()).iter().all(|k| k != "?"));
    }

    #[test]
    fn shortcut_groups_cover_the_new_keys() {
        let all: Vec<&str> = Home::shortcuts()
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(k, _)| *k))
            .collect();
        for k in ["enter", "o", "p", "c", "a", "m", "e", "esc", "?"] {
            assert!(all.contains(&k), "{k}");
        }
        assert!(!all.contains(&"x"));
    }

    fn run_summary(status: RunStatus) -> RunSummary {
        RunSummary {
            id: "20260101-120000".into(),
            status,
            run_branch: "phasewise/20260101-120000".into(),
            original_branch: "main".into(),
            phases: vec![RunPhaseSummary {
                agent: None,
                number: 1,
                title: "First".into(),
                status: PhaseStatus::AwaitingReview,
                started_at: None,
            }],
        }
    }

    #[test]
    fn c_commits_only_an_uncommitted_plan() {
        let mut h = valid();
        assert_eq!(h.handle_key(key('c')), None);
        assert_eq!(text(&h), Some("phasewise.md is already committed."));
        let mut missing = home_with(PlanState::Missing);
        assert_eq!(missing.handle_key(key('c')), None);
        assert_eq!(text(&missing), Some("There is no plan to commit."));
        h.data.plan_uncommitted = true;
        assert_eq!(h.handle_key(key('c')), Some(HomeCommand::Commit));
        h.begin_commit();
        assert!(h.animating());
        assert_eq!(h.handle_key(key('c')), None);
        assert_eq!(text(&h), Some(COMMITTING));
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(text(&h), Some(COMMITTING));
        assert_eq!(h.handle_key(key('p')), None);
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn enter_and_c_refuse_on_a_run_branch() {
        let mut h = valid();
        h.data.plan_uncommitted = true;
        h.data.branch = "phasewise/20260101-120000".into();
        h.data.run = Some(run_summary(RunStatus::Aborted));
        let expected =
            "You are on the branch of run 20260101-120000. Go back with: git switch main";
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(text(&h), Some(expected));
        h.message = None;
        // The run is over: `c` commits (after going back to the original branch).
        let keys: Vec<_> = h.footer().into_iter().map(|(key, _)| key).collect();
        assert!(!keys.contains(&"enter".to_string()), "{keys:?}");
        assert!(keys.contains(&"c".to_string()), "{keys:?}");
        assert_eq!(h.handle_key(key('c')), Some(HomeCommand::Commit));
        // An active run still refuses both.
        h.data.run = Some(run_summary(RunStatus::Active));
        assert_eq!(h.handle_key(key('c')), None);
        assert_eq!(text(&h), Some(RUN_ACTIVE));
        let keys: Vec<_> = h.footer().into_iter().map(|(key, _)| key).collect();
        assert!(!keys.contains(&"c".to_string()), "{keys:?}");
        h.data.branch = "main".into();
        h.data.run = Some(run_summary(RunStatus::Aborted));
        assert!(h.handle_key(code(KeyCode::Enter)).is_none());
        assert_eq!(h.mode, Mode::ConfirmCommitStart(0));
    }

    #[test]
    fn the_commit_command_is_relative_to_the_directory_phasewise_started_in() {
        let mut h = valid();
        assert_eq!(
            h.commit_hint(),
            "git add phasewise.md && git commit -m \"Add phasewise plan\""
        );
        h.data.plan_rel = "../phasewise.md".into();
        assert_eq!(
            h.commit_hint(),
            "git add ../phasewise.md && git commit -m \"Add phasewise plan\""
        );
        assert_eq!(h.plan_name(), "../phasewise.md");
        h.data.plan_rel = "../my plan.md".into();
        assert!(h.commit_hint().starts_with("git add '../my plan.md' &&"));
    }

    #[test]
    fn commit_result_is_reported_as_a_notice_or_an_error_popup() {
        let mut h = valid();
        h.begin_commit();
        assert!(h.commit_finished(&Ok("abc1234".into())));
        assert_eq!(h.committing, None);
        assert_eq!(text(&h), Some("Committed phasewise.md (abc1234)"));
        assert_eq!(h.message.as_ref().map(|n| n.level), Some(Level::Ok));
        h.begin_commit();
        assert!(!h.commit_finished(&Err("hook failed\nsecond line".into())));
        assert_eq!(
            h.mode,
            Mode::Error(vec![
                "Could not commit phasewise.md".into(),
                "hook failed".into(),
                "second line".into(),
                String::new(),
                "Commit it yourself:".into(),
                format!("  {}", h.commit_hint()),
            ])
        );
    }

    #[test]
    fn enter_on_an_uncommitted_plan_asks_to_commit_and_start() {
        let mut h = valid();
        h.data.plan_uncommitted = true;
        h.model = Some("opus".into());
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(h.mode, Mode::ConfirmCommitStart(0));
        let expected = HomeCommand::CommitAndStart {
            agent: AgentKind::Claude,
            overrides: Overrides {
                model: Some(Override::Set("opus".into())),
                effort: None,
            },
        };
        assert_eq!(h.handle_key(key('y')), Some(expected.clone()));
        assert_eq!(h.mode, Mode::Normal);
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.handle_key(code(KeyCode::Enter)), Some(expected));
        h.handle_key(code(KeyCode::Enter));
        h.handle_key(code(KeyCode::Down));
        assert_eq!(h.mode, Mode::ConfirmCommitStart(1));
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(h.mode, Mode::Normal);
        h.handle_key(code(KeyCode::Enter));
        assert_eq!(h.handle_key(key('n')), None);
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn enter_opens_the_active_run_and_blocks_the_plan_keys() {
        let mut h = valid();
        h.data.run = Some(run_summary(RunStatus::Active));
        assert_eq!(
            h.handle_key(code(KeyCode::Enter)),
            Some(HomeCommand::OpenRun)
        );
        for c in ['p', 'c', 'a', 'm', 'e'] {
            assert_eq!(h.handle_key(key(c)), None);
            assert_eq!(
                text(&h),
                Some("A run is active. Open it (enter) to finish or abandon it."),
                "key {c}"
            );
            assert_eq!(h.mode, Mode::Normal);
        }
        assert_eq!(h.agent, AgentKind::Claude);
    }

    #[test]
    fn o_opens_a_finished_run_and_is_refused_without_one() {
        let mut h = valid();
        assert_eq!(h.handle_key(key('o')), None);
        assert!(h.message.is_some());
        for status in [RunStatus::Completed, RunStatus::Aborted] {
            h.data.run = Some(run_summary(status));
            assert_eq!(h.handle_key(key('o')), Some(HomeCommand::OpenRun));
            // A finished run does not stop starting a new one.
            assert!(matches!(
                h.handle_key(code(KeyCode::Enter)),
                Some(HomeCommand::Start { .. })
            ));
            assert_eq!(h.handle_key(key('a')), None);
            assert_eq!(h.agent, AgentKind::Codex);
            h.handle_key(key('a'));
        }
    }

    #[test]
    fn digits_choose_rows_in_the_pickers() {
        let mut h = valid();
        h.handle_key(key('m'));
        h.set_models(AgentKind::Claude, Ok(models()));
        h.handle_key(key('3'));
        assert_eq!(h.model.as_deref(), Some("sonnet"));
        assert_eq!(h.mode, Mode::Normal);
        h.handle_key(key('m'));
        h.handle_key(key('9'));
        assert!(matches!(h.mode, Mode::ModelPopup(_)), "out of range");
        h.handle_key(key('4'));
        assert!(matches!(h.mode, Mode::ModelInput(_)));
        h.handle_key(code(KeyCode::Esc));
        h.handle_key(key('m'));
        h.handle_key(key('1'));
        assert_eq!(h.model, None);

        h.handle_key(key('e'));
        h.handle_key(key('4'));
        assert_eq!(h.effort.as_deref(), Some("high"));
        assert_eq!(h.mode, Mode::Normal);
        h.handle_key(key('e'));
        h.handle_key(key('1'));
        assert_eq!(h.effort, None);
    }

    #[test]
    fn text_inputs_take_ctrl_u_and_ctrl_w() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut h = home_with(PlanState::Missing);
        h.handle_key(key('p'));
        type_text(&mut h, "add a CLI tool");
        h.handle_key(ctrl('w'));
        assert_eq!(h.mode, Mode::GoalInput("add a CLI ".into()));
        h.handle_key(ctrl('u'));
        assert_eq!(h.mode, Mode::GoalInput(String::new()));
        h.handle_key(code(KeyCode::Esc));
        h.handle_key(key('m'));
        h.handle_key(key('2'));
        type_text(&mut h, "my model");
        h.handle_key(ctrl('w'));
        assert_eq!(h.mode, Mode::ModelInput("my ".into()));
        h.handle_key(ctrl('u'));
        assert_eq!(h.mode, Mode::ModelInput(String::new()));
    }

    #[test]
    fn confirmations_move_with_arrows_and_ignore_other_keys() {
        let mut h = valid();
        h.begin_planning();
        h.handle_key(key('q'));
        assert_eq!(h.handle_key(key('z')), None);
        assert_eq!(h.mode, Mode::ConfirmQuit(0));
        h.handle_key(key('j'));
        assert_eq!(h.mode, Mode::ConfirmQuit(1));
        h.handle_key(code(KeyCode::Up));
        assert_eq!(h.mode, Mode::ConfirmQuit(0));
        h.handle_key(key('j'));
        assert_eq!(h.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(h.mode, Mode::Normal);
    }

    #[test]
    fn set_data_reports_whether_anything_changed() {
        let mut h = valid();
        let same = h.data.clone();
        assert!(!h.set_data(same.clone()));
        let mut other = same;
        other.branch = "dev".into();
        assert!(h.set_data(other));
    }

    #[test]
    fn animating_while_planning_committing_a_run_or_loading_models() {
        let mut h = valid();
        assert!(!h.animating());
        h.begin_planning();
        assert!(h.animating());
        h.plan_finished(PlanReport::Success("ok".into()));
        assert!(!h.animating());
        h.run_busy = true;
        assert!(h.animating());
        h.run_busy = false;
        h.handle_key(key('m'));
        assert!(h.animating(), "model list loading");
        h.set_models(AgentKind::Claude, Ok(models()));
        assert!(!h.animating());
    }

    fn outcome(result: PlanResult, cost: Option<f64>) -> anyhow::Result<PlanOutcome> {
        Ok(PlanOutcome {
            plan_id: "p1".into(),
            dir: PathBuf::from("/work/demo/.phasewise/plans/p1"),
            cost_usd: cost,
            result,
        })
    }

    #[test]
    fn report_for_a_written_plan() {
        let plan = plan::parse("# T\n\n## Phase 1: A\n\nx\n\n## Phase 2: B\n\ny\n").expect("plan");
        let written = |plan: plan::Plan, cost| {
            plan_report(
                &outcome(
                    PlanResult::Written {
                        path: PathBuf::from("/work/demo/phasewise.md"),
                        plan,
                    },
                    cost,
                ),
                Path::new("/work/demo"),
            )
        };
        assert_eq!(
            written(plan.clone(), Some(0.1234)),
            PlanReport::Success(
                "Plan written to phasewise.md (2 phases, $0.12). Review it, commit it, then press Enter."
                    .into()
            )
        );
        assert_eq!(
            written(plan, None),
            PlanReport::Success(
                "Plan written to phasewise.md (2 phases). Review it, commit it, then press Enter."
                    .into()
            )
        );
    }

    #[test]
    fn report_for_a_failed_or_cancelled_plan() {
        let failed = |cancelled| {
            plan_report(
                &outcome(
                    PlanResult::Failed {
                        problems: vec!["line 3: bad".into(), "no markers".into()],
                        last_draft: Some(PathBuf::from(
                            "/work/demo/.phasewise/plans/p1/draft-2.md",
                        )),
                        cancelled,
                    },
                    Some(0.5),
                ),
                Path::new("/work/demo"),
            )
        };
        let PlanReport::Failure(lines) = failed(false) else {
            panic!("expected a failure");
        };
        assert_eq!(
            lines,
            [
                "phasewise could not get a valid plan. Nothing was written.",
                "- line 3: bad",
                "- no markers",
                "Last draft: .phasewise/plans/p1/draft-2.md (fix it by hand and check it with `phasewise check`).",
                "Agent output and drafts: .phasewise/plans/p1. Press p to try again.",
                "Cost: $0.50",
            ]
        );
        let PlanReport::Failure(lines) = failed(true) else {
            panic!("expected a failure");
        };
        assert_eq!(lines[0], "Planning was interrupted. No plan was written.");
    }

    #[test]
    fn report_for_an_error() {
        let err: anyhow::Result<PlanOutcome> = Err(anyhow::anyhow!("cannot write"));
        let PlanReport::Failure(lines) = plan_report(&err, Path::new("/work/demo")) else {
            panic!("expected a failure");
        };
        assert_eq!(lines[1], "cannot write");
        assert!(lines[0].contains("could not write the plan"));
    }
}
