//! TUI state machine: keys in, commands out. No terminal, no I/O.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::data::{DiffView, LogLine, RunView};
use super::shared::{
    Choice, FooterItem, Notice, PanelOutcome, SETTINGS_SHORTCUT, SettingsPanel, ShortcutGroups,
    confirm_key, edit_text, item, picker_footer,
};
use crate::settings::Settings;
use crate::state::PhaseStatus;

const DEFAULT_PAGE: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Diff,
    Log,
    Verify,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Diff, Tab::Log, Tab::Verify];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Diff => "Diff",
            Tab::Log => "Log",
            Tab::Verify => "Verify",
        }
    }

    fn next(self) -> Tab {
        match self {
            Tab::Diff => Tab::Log,
            Tab::Log => Tab::Verify,
            Tab::Verify => Tab::Diff,
        }
    }
}

/// Confirmation modes hold the highlighted row: 0 is the action, 1 cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Phase to roll back to, highlighted row.
    ConfirmRollback(usize, usize),
    ConfirmInterrupt(usize),
    ConfirmQuit(usize),
    ConfirmAbandon(usize),
    ConfirmMerge(usize),
    /// Optional note for a retry, being typed.
    Note(String),
    /// Popup listing every key.
    Help,
    /// The user settings menu.
    Settings(SettingsPanel),
}

/// What the event loop must do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Next,
    Retry(Option<String>),
    Rollback(usize),
    Interrupt,
    /// Abandon the run (`ops::abort`), already confirmed.
    Abandon,
    /// Merge the completed run (`ops::merge`), already confirmed.
    Merge,
    /// Go back to the home screen.
    Home,
    /// Save these settings; they already apply on this screen.
    SaveSettings(Settings),
    /// Reload the caches of the (newly) selected phase.
    Reload,
}

#[derive(Debug, Clone)]
pub struct App {
    pub view: RunView,
    /// Index into `view.phases`.
    pub selected: usize,
    pub tab: Tab,
    /// First visible line of the right panel (a `usize`: a diff can have more
    /// than 65,535 lines).
    pub scroll: usize,
    /// Content height of the right panel; set by the view on every draw.
    pub page: u16,
    pub mode: Mode,
    /// Phase run by a worker thread, if any.
    pub running: Option<usize>,
    /// The run's worker thread is alive (set by the loop every turn; `running`
    /// only follows the engine's events).
    pub worker_busy: bool,
    /// The home screen is planning (set by the loop, for the quit question).
    pub planning_busy: bool,
    /// Spinner frame, set by the loop from the elapsed time.
    pub frame: usize,
    pub message: Option<Notice>,
    /// The user settings (what the screen shows depends on them).
    pub settings: Settings,
    pub diff: DiffView,
    pub log: Vec<LogLine>,
    pub verify: String,
}

impl App {
    /// Selects the phase that most likely needs attention.
    pub fn new(view: RunView) -> App {
        let pick =
            |wanted: &[PhaseStatus]| view.phases.iter().position(|p| wanted.contains(&p.status));
        let selected = pick(&[
            PhaseStatus::Failed,
            PhaseStatus::Running,
            PhaseStatus::Verifying,
            PhaseStatus::AwaitingReview,
        ])
        .or_else(|| pick(&[PhaseStatus::Pending]))
        .unwrap_or(view.phases.len().saturating_sub(1));
        App {
            view,
            selected,
            tab: Tab::Diff,
            scroll: 0,
            page: DEFAULT_PAGE,
            mode: Mode::Normal,
            running: None,
            worker_busy: false,
            planning_busy: false,
            frame: 0,
            message: None,
            settings: Settings::default(),
            diff: DiffView::default(),
            log: Vec::new(),
            verify: String::new(),
        }
    }

    /// Number of the selected phase (1-based); `None` for an empty run.
    pub fn selected_number(&self) -> Option<usize> {
        self.view.phases.get(self.selected).map(|p| p.number)
    }

    /// Replaces the run view, keeping the selection in range.
    pub fn set_view(&mut self, view: RunView) {
        self.view = view;
        self.selected = self.selected.min(self.view.phases.len().saturating_sub(1));
    }

    /// Selects phase `number` when it starts running, so its progress shows
    /// without a key.
    pub fn follow(&mut self, number: usize) {
        if let Some(index) = self.view.phases.iter().position(|p| p.number == number)
            && index != self.selected
        {
            self.selected = index;
            self.scroll = 0;
        }
    }

    pub fn set_caches(&mut self, diff: DiffView, log: Vec<LogLine>, verify: String) {
        self.diff = diff;
        self.log = log;
        self.verify = verify;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
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
                return Some(Command::Quit);
            }
            self.mode = Mode::Normal;
        }
        match self.mode.clone() {
            Mode::Normal => self.key_normal(key),
            Mode::ConfirmRollback(n, row) => self.confirm(
                key,
                row,
                |row| Mode::ConfirmRollback(n, row),
                Command::Rollback(n),
            ),
            Mode::ConfirmInterrupt(row) => {
                self.confirm(key, row, Mode::ConfirmInterrupt, Command::Interrupt)
            }
            Mode::ConfirmQuit(row) => self.confirm(key, row, Mode::ConfirmQuit, Command::Quit),
            Mode::ConfirmAbandon(row) => {
                self.confirm(key, row, Mode::ConfirmAbandon, Command::Abandon)
            }
            Mode::ConfirmMerge(row) => self.confirm(key, row, Mode::ConfirmMerge, Command::Merge),
            Mode::Note(buffer) => self.key_note(key, buffer),
            Mode::Help => {
                // Any key closes the help; `?` is the one that opened it.
                self.mode = Mode::Normal;
                None
            }
            Mode::Settings(panel) => self.key_settings(key, panel),
        }
    }

    fn key_settings(&mut self, key: KeyEvent, mut panel: SettingsPanel) -> Option<Command> {
        match panel.handle_key(key) {
            PanelOutcome::Close => {
                self.mode = Mode::Normal;
                None
            }
            PanelOutcome::Stay => {
                self.mode = Mode::Settings(panel);
                None
            }
            PanelOutcome::Changed => {
                self.settings = panel.settings.clone();
                let command = Command::SaveSettings(panel.settings.clone());
                self.mode = Mode::Settings(panel);
                Some(command)
            }
        }
    }

    fn confirm(
        &mut self,
        key: KeyEvent,
        row: usize,
        again: impl FnOnce(usize) -> Mode,
        command: Command,
    ) -> Option<Command> {
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

    fn key_note(&mut self, key: KeyEvent, mut buffer: String) -> Option<Command> {
        match key.code {
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                let note = buffer.trim();
                return Some(Command::Retry((!note.is_empty()).then(|| note.to_string())));
            }
            KeyCode::Esc => self.mode = Mode::Normal,
            _ => {
                if edit_text(&mut buffer, key) {
                    self.mode = Mode::Note(buffer);
                }
            }
        }
        None
    }

    fn key_normal(&mut self, key: KeyEvent) -> Option<Command> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => self.quit(),
            KeyCode::Char('q') => self.quit(),
            KeyCode::Esc => Some(Command::Home),
            KeyCode::Char('A') => self.abandon(),
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                None
            }
            KeyCode::Char('s') => {
                self.mode = Mode::Settings(SettingsPanel::new(self.settings.clone()));
                None
            }
            KeyCode::Char('j') | KeyCode::Down => self.select(1),
            KeyCode::Char('k') | KeyCode::Up => self.select(-1),
            KeyCode::Tab => {
                self.tab = self.tab.next();
                self.scroll = 0;
                None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(usize::from(self.page));
                None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(usize::from(self.page));
                None
            }
            KeyCode::Char('n') => match self.why_not_next() {
                None => Some(Command::Next),
                Some(why) => self.refuse(why),
            },
            KeyCode::Char('r') => match self.why_not_retry() {
                None => {
                    self.mode = Mode::Note(String::new());
                    None
                }
                Some(why) => self.refuse(why),
            },
            KeyCode::Char('b') => match self.why_not_rollback() {
                None => {
                    self.selected_number()
                        .inspect(|&n| self.mode = Mode::ConfirmRollback(n, 0));
                    None
                }
                Some(why) => self.refuse(why),
            },
            KeyCode::Char('m') => match self.why_not_merge() {
                None => {
                    self.mode = Mode::ConfirmMerge(0);
                    None
                }
                Some(why) => self.refuse(why),
            },
            KeyCode::Char('x') => {
                if self.running.is_some() {
                    self.mode = Mode::ConfirmInterrupt(0);
                    None
                } else {
                    self.refuse("No phase is running.".to_string())
                }
            }
            _ => None,
        }
    }

    fn refuse(&mut self, why: String) -> Option<Command> {
        self.message = Some(Notice::warn(why));
        None
    }

    fn abandon(&mut self) -> Option<Command> {
        if self.running.is_some() || self.worker_busy {
            return self.refuse("Interrupt the running phase first (x).".to_string());
        }
        if !self.view.active {
            return self.refuse("The run is already finished.".to_string());
        }
        self.mode = Mode::ConfirmAbandon(0);
        None
    }

    fn quit(&mut self) -> Option<Command> {
        if self.running.is_some() || self.worker_busy || self.planning_busy {
            self.mode = Mode::ConfirmQuit(0);
            None
        } else {
            Some(Command::Quit)
        }
    }

    fn select(&mut self, delta: isize) -> Option<Command> {
        let last = self.view.phases.len().saturating_sub(1);
        let target = self.selected.saturating_add_signed(delta).min(last);
        if target == self.selected {
            return None;
        }
        self.selected = target;
        self.scroll = 0;
        Some(Command::Reload)
    }

    fn why_not_merge(&self) -> Option<String> {
        if self.running.is_some() || self.worker_busy {
            return Some("Wait for the running work to finish first.".to_string());
        }
        if !self.view.completed {
            return Some("The run is not completed: nothing to merge yet.".to_string());
        }
        None
    }

    fn running_reason(&self) -> Option<String> {
        self.running
            .map(|n| format!("Phase {n} is running: press x to interrupt it."))
    }

    fn first_with(&self, status: PhaseStatus) -> Option<usize> {
        self.view
            .phases
            .iter()
            .find(|p| p.status == status)
            .map(|p| p.number)
    }

    fn why_not_next(&self) -> Option<String> {
        if !self.view.active {
            return Some("The run is not active: nothing more to launch.".to_string());
        }
        if let Some(why) = self.running_reason() {
            return Some(why);
        }
        if let Some(n) = self.first_with(PhaseStatus::Failed) {
            return Some(format!("Phase {n} failed: press r to retry it."));
        }
        let can = self
            .view
            .phases
            .iter()
            .any(|p| matches!(p.status, PhaseStatus::AwaitingReview | PhaseStatus::Pending));
        (!can).then(|| "No phase is left to run.".to_string())
    }

    fn why_not_retry(&self) -> Option<String> {
        if !self.view.active {
            return Some("The run is not active: nothing to retry.".to_string());
        }
        if let Some(why) = self.running_reason() {
            return Some(why);
        }
        let can = self
            .view
            .phases
            .iter()
            .any(|p| matches!(p.status, PhaseStatus::Failed | PhaseStatus::AwaitingReview));
        (!can).then(|| "Nothing to retry: no phase failed or awaits review.".to_string())
    }

    fn why_not_rollback(&self) -> Option<String> {
        if let Some(why) = self.running_reason() {
            return Some(why);
        }
        match self.view.phases.get(self.selected) {
            None => Some("There is no phase to roll back to.".to_string()),
            Some(p) if !p.has_checkpoint => Some(format!(
                "Phase {} has no checkpoint to roll back to.",
                p.number
            )),
            Some(_) => None,
        }
    }

    /// True while something on this screen moves (a phase runs).
    pub fn animating(&self) -> bool {
        self.running.is_some()
            || self
                .view
                .phases
                .iter()
                .any(|p| matches!(p.status, PhaseStatus::Running | PhaseStatus::Verifying))
    }

    /// Footer items for the current state, highest priority first. The `?`
    /// item is not in the list: the renderer appends it.
    pub fn footer(&self) -> Vec<FooterItem> {
        match &self.mode {
            Mode::Settings(panel) => return panel.footer(),
            Mode::ConfirmRollback(..)
            | Mode::ConfirmInterrupt(_)
            | Mode::ConfirmQuit(_)
            | Mode::ConfirmAbandon(_)
            | Mode::ConfirmMerge(_) => return picker_footer(2),
            Mode::Note(_) => return vec![item("enter", "retry"), item("esc", "cancel")],
            Mode::Help => return vec![item("any key", "close")],
            Mode::Normal => {}
        }
        let tail = [
            item("tab", "view"),
            item("esc", "home"),
            item("q", "quit"),
            item("s", "settings"),
        ];
        let mut keys = Vec::new();
        let status = self.view.phases.get(self.selected).map(|p| p.status);
        if self.running.is_some() {
            keys.push(item("x", "interrupt"));
        } else if self.view.active && status == Some(PhaseStatus::AwaitingReview) {
            keys.extend([
                item("n", "approve"),
                item("r", "retry"),
                item("b", "roll back"),
            ]);
        } else if self.view.active && status == Some(PhaseStatus::Failed) {
            keys.extend([item("r", "retry"), item("b", "roll back")]);
        } else if self.view.completed {
            keys.extend([item("m", "merge"), item("j/k", "phase")]);
        } else {
            keys.push(item("j/k", "phase"));
        }
        keys.extend(tail);
        keys
    }

    /// Every key of the screen in groups, for the `?` popup.
    pub fn shortcuts() -> ShortcutGroups {
        vec![
            (
                "Phases",
                vec![
                    ("j / k", "select the next or previous phase"),
                    ("tab", "switch between Diff, Log and Verify"),
                    ("PgUp / PgDn", "scroll the right panel"),
                ],
            ),
            (
                "Run",
                vec![
                    ("n", "approve the phase and run the next one"),
                    ("r", "retry the phase, with an optional note"),
                    ("b", "roll back to the checkpoint of the phase"),
                    ("m", "merge the completed run into its branch"),
                    ("x", "interrupt the running phase"),
                    ("A", "abandon the run"),
                ],
            ),
            (
                "General",
                vec![
                    ("esc", "back to the home screen"),
                    ("?", "show or hide the shortcuts"),
                    SETTINGS_SHORTCUT,
                    ("q / Ctrl-C", "quit"),
                ],
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::PhaseView;
    use crate::tui::shared::Level;

    fn phase(number: usize, status: PhaseStatus, checkpoint: bool) -> PhaseView {
        PhaseView {
            agent: None,
            number,
            title: format!("Phase {number}"),
            status,
            attempts: 1,
            failure: None,
            has_checkpoint: checkpoint,
            model: None,
            effort: None,
            cost_usd: None,
            started_at: None,
            finished_at: None,
            failure_kind: None,
            failure_detail: None,
        }
    }

    fn app(phases: Vec<PhaseView>) -> App {
        App::new(RunView {
            run_id: "run".into(),
            plan_title: "Plan".into(),
            agent: "claude".into(),
            run_branch: "phasewise/run".into(),
            original_branch: "main".into(),
            active: true,
            completed: false,
            phases,
            limits: None,
        })
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn reviewing() -> App {
        app(vec![
            phase(1, PhaseStatus::Done, true),
            phase(2, PhaseStatus::AwaitingReview, true),
            phase(3, PhaseStatus::Pending, false),
        ])
    }

    #[test]
    fn initial_selection_is_the_phase_needing_attention() {
        assert_eq!(reviewing().selected, 1);
        let failed = app(vec![
            phase(1, PhaseStatus::Done, true),
            phase(2, PhaseStatus::Failed, false),
        ]);
        assert_eq!(failed.selected, 1);
        let done = app(vec![phase(1, PhaseStatus::Done, true)]);
        assert_eq!(done.selected, 0);
    }

    #[test]
    fn selection_moves_clamps_and_resets_scroll() {
        let mut a = reviewing();
        a.scroll = 5;
        assert_eq!(a.handle_key(key('j')), Some(Command::Reload));
        assert_eq!((a.selected, a.scroll), (2, 0));
        assert_eq!(a.handle_key(code(KeyCode::Down)), None);
        assert_eq!(a.selected, 2);
        assert_eq!(a.handle_key(key('k')), Some(Command::Reload));
        assert_eq!(a.handle_key(code(KeyCode::Up)), Some(Command::Reload));
        assert_eq!(a.handle_key(code(KeyCode::Up)), None);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn tab_cycles_and_resets_scroll() {
        let mut a = reviewing();
        a.scroll = 3;
        a.handle_key(code(KeyCode::Tab));
        assert_eq!((a.tab, a.scroll), (Tab::Log, 0));
        a.handle_key(code(KeyCode::Tab));
        assert_eq!(a.tab, Tab::Verify);
        a.handle_key(code(KeyCode::Tab));
        assert_eq!(a.tab, Tab::Diff);
    }

    #[test]
    fn page_keys_scroll_by_the_page_height() {
        let mut a = reviewing();
        a.page = 7;
        a.handle_key(code(KeyCode::PageDown));
        a.handle_key(code(KeyCode::PageDown));
        assert_eq!(a.scroll, 14);
        a.handle_key(code(KeyCode::PageUp));
        assert_eq!(a.scroll, 7);
        a.handle_key(code(KeyCode::PageUp));
        a.handle_key(code(KeyCode::PageUp));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn next_is_allowed_when_a_phase_awaits_review_or_is_pending() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(key('n')), Some(Command::Next));
        let mut p = app(vec![phase(1, PhaseStatus::Pending, false)]);
        assert_eq!(p.handle_key(key('n')), Some(Command::Next));
    }

    #[test]
    fn next_is_refused_with_a_reason() {
        let mut failed = app(vec![
            phase(1, PhaseStatus::Done, true),
            phase(2, PhaseStatus::Failed, false),
        ]);
        assert_eq!(failed.handle_key(key('n')), None);
        assert_eq!(text(&failed), Some("Phase 2 failed: press r to retry it."));

        let mut running = reviewing();
        running.running = Some(2);
        assert_eq!(running.handle_key(key('n')), None);
        assert!(text(&running).is_some_and(|m| m.contains("running")));

        let mut inactive = reviewing();
        inactive.view.active = false;
        assert_eq!(inactive.handle_key(key('n')), None);
        assert!(inactive.message.is_some());

        let mut done = app(vec![phase(1, PhaseStatus::Done, true)]);
        assert_eq!(done.handle_key(key('n')), None);
        assert!(done.message.is_some());
    }

    #[test]
    fn message_clears_on_the_next_key() {
        let mut a = app(vec![phase(1, PhaseStatus::Done, true)]);
        a.handle_key(key('n'));
        assert!(a.message.is_some());
        a.handle_key(key('j'));
        assert!(a.message.is_none());
    }

    #[test]
    fn retry_opens_the_note_and_enter_sends_it() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(key('r')), None);
        assert_eq!(a.mode, Mode::Note(String::new()));
        for c in "fix it".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(key('!'));
        a.handle_key(code(KeyCode::Backspace));
        assert_eq!(a.mode, Mode::Note("fix it".into()));
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            Some(Command::Retry(Some("fix it".into())))
        );
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn retry_with_an_empty_note_sends_none_and_esc_cancels() {
        let mut a = reviewing();
        a.handle_key(key('r'));
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            Some(Command::Retry(None))
        );
        a.handle_key(key('r'));
        a.handle_key(key('x'));
        assert_eq!(a.handle_key(code(KeyCode::Esc)), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn retry_is_refused_without_a_candidate_or_while_running() {
        let mut a = app(vec![
            phase(1, PhaseStatus::Done, true),
            phase(2, PhaseStatus::Pending, false),
        ]);
        a.handle_key(key('r'));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.message.is_some());

        let mut running = reviewing();
        running.running = Some(2);
        running.handle_key(key('r'));
        assert_eq!(running.mode, Mode::Normal);
        assert!(running.message.is_some());
    }

    #[test]
    fn rollback_needs_a_checkpoint_and_confirmation() {
        let mut a = reviewing();
        a.handle_key(key('b'));
        assert_eq!(a.mode, Mode::ConfirmRollback(2, 0));
        assert_eq!(a.handle_key(key('y')), Some(Command::Rollback(2)));
        assert_eq!(a.mode, Mode::Normal);

        a.handle_key(key('b'));
        assert_eq!(a.handle_key(key('n')), None);
        assert_eq!(a.mode, Mode::Normal);

        a.handle_key(key('j'));
        a.handle_key(key('b'));
        assert_eq!(a.mode, Mode::Normal);
        assert!(text(&a).is_some_and(|m| m.contains("no checkpoint")));
    }

    #[test]
    fn rollback_is_refused_while_running() {
        let mut a = reviewing();
        a.running = Some(3);
        a.handle_key(key('b'));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.message.is_some());
    }

    #[test]
    fn interrupt_only_while_running_and_confirmed() {
        let mut a = reviewing();
        a.handle_key(key('x'));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.message.is_some());

        a.running = Some(3);
        a.handle_key(key('x'));
        assert_eq!(a.mode, Mode::ConfirmInterrupt(0));
        assert_eq!(a.handle_key(key('y')), Some(Command::Interrupt));
        a.handle_key(key('x'));
        assert_eq!(a.handle_key(key('n')), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn ctrl_c_quits_from_popups_and_inputs() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut a = reviewing();
        a.mode = Mode::Note("half a note".into());
        assert_eq!(a.handle_key(ctrl_c), Some(Command::Quit));
        assert_eq!(a.mode, Mode::Normal);
        a.mode = Mode::ConfirmRollback(1, 0);
        assert_eq!(a.handle_key(ctrl_c), Some(Command::Quit));
        // While a phase runs it still asks first.
        a.running = Some(1);
        a.mode = Mode::Help;
        assert_eq!(a.handle_key(ctrl_c), None);
        assert_eq!(a.mode, Mode::ConfirmQuit(0));
    }

    #[test]
    fn quit_is_immediate_when_idle_and_confirmed_when_running() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(key('q')), Some(Command::Quit));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.handle_key(ctrl_c), Some(Command::Quit));

        a.running = Some(3);
        assert_eq!(a.handle_key(key('q')), None);
        assert_eq!(a.mode, Mode::ConfirmQuit(0));
        assert_eq!(a.handle_key(key('y')), Some(Command::Quit));
        a.handle_key(key('q'));
        // Ctrl-C while the question is open confirms.
        assert_eq!(a.handle_key(ctrl_c), Some(Command::Quit));
        a.handle_key(key('q'));
        assert_eq!(a.handle_key(key('n')), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    fn keys(a: &App) -> Vec<String> {
        a.footer().into_iter().map(|(k, _)| k).collect()
    }

    fn text(a: &App) -> Option<&str> {
        a.message.as_ref().map(|n| n.text.as_str())
    }

    #[test]
    fn a_starting_phase_is_selected() {
        let mut a = app(vec![
            phase(1, PhaseStatus::Done, true),
            phase(2, PhaseStatus::Pending, false),
        ]);
        a.selected = 0;
        a.scroll = 7;
        a.follow(2);
        assert_eq!((a.selected, a.scroll), (1, 0));
        a.follow(9);
        assert_eq!(a.selected, 1);
    }

    #[test]
    fn footer_follows_the_state() {
        let a = reviewing();
        assert_eq!(keys(&a), ["n", "r", "b", "tab", "esc", "q", "s"]);
        let mut running = reviewing();
        running.running = Some(3);
        assert_eq!(keys(&running), ["x", "tab", "esc", "q", "s"]);
        let failed = app(vec![
            phase(1, PhaseStatus::Done, true),
            phase(2, PhaseStatus::Failed, false),
        ]);
        assert_eq!(keys(&failed), ["r", "b", "tab", "esc", "q", "s"]);
        let mut other = reviewing();
        other.selected = 0;
        assert_eq!(keys(&other), ["j/k", "tab", "esc", "q", "s"]);
        other.view.active = false;
        other.selected = 1;
        assert_eq!(keys(&other), ["j/k", "tab", "esc", "q", "s"]);
        let mut note = reviewing();
        note.mode = Mode::Note(String::new());
        assert_eq!(keys(&note), ["enter", "esc"]);
        let mut confirm = reviewing();
        confirm.mode = Mode::ConfirmQuit(0);
        assert_eq!(keys(&confirm), ["enter", "1–2", "esc"]);
        confirm.mode = Mode::Help;
        assert_eq!(keys(&confirm), ["any key"]);
        assert!(keys(&a).iter().all(|k| k != "?"));
    }

    #[test]
    fn shortcuts_are_grouped_and_mention_the_new_keys() {
        let groups = App::shortcuts();
        let all: Vec<&str> = groups
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(k, _)| *k))
            .collect();
        assert!(all.contains(&"A") && all.contains(&"esc"));
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn esc_goes_home_only_in_normal_mode() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(code(KeyCode::Esc)), Some(Command::Home));
        a.handle_key(key('r'));
        assert_eq!(a.handle_key(code(KeyCode::Esc)), None);
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('?'));
        assert_eq!(a.handle_key(code(KeyCode::Esc)), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn m_asks_before_merging_a_completed_run() {
        let mut a = app(vec![phase(1, PhaseStatus::Done, true)]);
        a.view.active = false;
        a.view.completed = true;
        assert_eq!(a.handle_key(key('m')), None);
        assert_eq!(a.mode, Mode::ConfirmMerge(0));
        assert_eq!(a.footer().len(), 3);
        assert_eq!(a.handle_key(key('y')), Some(Command::Merge));
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('m'));
        assert_eq!(a.handle_key(key('n')), None);
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('m'));
        a.handle_key(key('j'));
        assert_eq!(a.mode, Mode::ConfirmMerge(1));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn m_is_refused_unless_the_run_is_completed_and_idle() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(key('m')), None);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(
            a.message.as_ref().map(|n| n.text.as_str()),
            Some("The run is not completed: nothing to merge yet.")
        );
        let mut busy = app(vec![phase(1, PhaseStatus::Done, true)]);
        busy.view.completed = true;
        busy.worker_busy = true;
        assert_eq!(busy.handle_key(key('m')), None);
        assert_eq!(busy.mode, Mode::Normal);
        assert!(busy.message.is_some());
    }

    #[test]
    fn the_footer_offers_merge_only_for_a_completed_run() {
        let mut a = app(vec![phase(1, PhaseStatus::Done, true)]);
        let keys = |a: &App| a.footer().into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        assert_eq!(keys(&a), ["j/k", "tab", "esc", "q", "s"]);
        a.view.completed = true;
        assert_eq!(keys(&a), ["m", "j/k", "tab", "esc", "q", "s"]);
        a.mode = Mode::ConfirmMerge(0);
        assert_eq!(keys(&a), ["enter", "1–2", "esc"]);
    }

    #[test]
    fn abandon_is_confirmed_and_refused_while_running_or_finished() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(key('A')), None);
        assert_eq!(a.mode, Mode::ConfirmAbandon(0));
        assert_eq!(a.handle_key(key('y')), Some(Command::Abandon));
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('A'));
        assert_eq!(a.handle_key(key('j')), None);
        assert_eq!(a.mode, Mode::ConfirmAbandon(1));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), None);
        assert_eq!(a.mode, Mode::Normal);

        let mut running = reviewing();
        running.running = Some(2);
        assert_eq!(running.handle_key(key('A')), None);
        assert_eq!(running.mode, Mode::Normal);
        assert_eq!(
            text(&running),
            Some("Interrupt the running phase first (x).")
        );

        let mut done = reviewing();
        done.view.active = false;
        assert_eq!(done.handle_key(key('A')), None);
        assert_eq!(done.mode, Mode::Normal);
        assert_eq!(text(&done), Some("The run is already finished."));
        assert_eq!(done.message.as_ref().map(|n| n.level), Some(Level::Warn));
    }

    #[test]
    fn confirmations_take_digits_arrows_enter_and_ignore_other_keys() {
        let mut a = reviewing();
        a.handle_key(key('b'));
        assert_eq!(a.handle_key(key('z')), None);
        assert_eq!(a.mode, Mode::ConfirmRollback(2, 0));
        a.handle_key(code(KeyCode::Down));
        assert_eq!(a.mode, Mode::ConfirmRollback(2, 1));
        a.handle_key(key('k'));
        assert_eq!(a.mode, Mode::ConfirmRollback(2, 0));
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            Some(Command::Rollback(2))
        );
        a.handle_key(key('b'));
        assert_eq!(a.handle_key(key('1')), Some(Command::Rollback(2)));
        a.handle_key(key('b'));
        assert_eq!(a.handle_key(key('2')), None);
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('b'));
        assert_eq!(a.handle_key(code(KeyCode::Esc)), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn a_live_worker_counts_before_its_first_event_and_after_the_last() {
        let mut a = reviewing();
        a.worker_busy = true;
        assert_eq!(a.running, None);
        assert_eq!(a.handle_key(key('A')), None);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(text(&a), Some("Interrupt the running phase first (x)."));
        assert_eq!(a.handle_key(key('q')), None);
        assert_eq!(a.mode, Mode::ConfirmQuit(0));
    }

    #[test]
    fn quit_asks_while_the_home_screen_is_planning() {
        let mut a = reviewing();
        a.planning_busy = true;
        assert_eq!(a.handle_key(key('q')), None);
        assert_eq!(a.mode, Mode::ConfirmQuit(0));
        assert_eq!(a.handle_key(key('y')), Some(Command::Quit));
    }

    #[test]
    fn the_note_takes_ctrl_u_and_ctrl_w() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut a = reviewing();
        a.handle_key(key('r'));
        for c in "fix the bug".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(ctrl('w'));
        assert_eq!(a.mode, Mode::Note("fix the ".into()));
        a.handle_key(ctrl('u'));
        assert_eq!(a.mode, Mode::Note(String::new()));
    }

    #[test]
    fn animating_while_a_phase_runs() {
        let mut a = reviewing();
        assert!(!a.animating());
        a.running = Some(2);
        assert!(a.animating());
        let running = app(vec![phase(1, PhaseStatus::Running, false)]);
        assert!(running.animating());
    }

    #[test]
    fn question_mark_toggles_the_help_popup() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(key('?')), None);
        assert_eq!(a.mode, Mode::Help);
        // Keys do nothing but close it.
        assert_eq!(a.handle_key(key('n')), None);
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('?'));
        assert_eq!(a.handle_key(key('?')), None);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn s_opens_the_settings_and_a_change_is_applied_and_saved() {
        let mut a = reviewing();
        assert_eq!(a.handle_key(key('s')), None);
        assert!(matches!(a.mode, Mode::Settings(_)));
        assert_eq!(keys(&a), ["enter", "j/k", "esc"]);
        // The first row is `limits`: toggling it applies at once.
        assert!(a.settings.limits());
        let command = a.handle_key(code(KeyCode::Enter));
        assert!(!a.settings.limits());
        let Some(Command::SaveSettings(saved)) = command else {
            panic!("expected SaveSettings, got {command:?}");
        };
        assert!(!saved.limits());
        assert!(matches!(a.mode, Mode::Settings(_)), "the popup stays open");
        assert_eq!(a.handle_key(code(KeyCode::Esc)), None);
        assert_eq!(a.mode, Mode::Normal);
        // `s` closes it too, and Ctrl-C closes it and quits.
        a.handle_key(key('s'));
        assert_eq!(a.handle_key(key('s')), None);
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('s'));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.handle_key(ctrl_c), Some(Command::Quit));
    }

    #[test]
    fn the_footer_and_the_shortcuts_offer_the_settings() {
        let a = reviewing();
        assert!(a.footer().contains(&item("s", "settings")));
        assert!(
            App::shortcuts()
                .iter()
                .flat_map(|(_, rows)| rows)
                .any(|(k, _)| *k == "s")
        );
    }
}
