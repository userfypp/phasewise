//! TUI state machine: keys in, commands out. No terminal, no I/O.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::data::{DiffView, LogLine, RunView};
use super::shared::{
    Choice, FooterItem, Notice, PanelOutcome, SETTINGS_SHORTCUT, SettingsPanel, ShortcutGroups,
    confirm_key, edit_text, help_key, help_scroll_max, item, paste_keys, picker_footer,
};
use crate::settings::Settings;
use crate::state::PhaseStatus;

const DEFAULT_PAGE: u16 = 10;
/// Columns the right panel moves per `h` / `l`.
const HSCROLL_STEP: usize = 8;

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
    /// Text to look for in the right panel, being typed.
    Search(String),
    /// Popup listing every key.
    Help,
    /// Popup with the whole failure of the selected phase and where its logs are.
    Failure,
    /// The user settings menu.
    Settings(SettingsPanel),
}

/// What the event loop must do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Next,
    Retry(Option<String>),
    /// Run the checks on the working tree of the failed phase and checkpoint it.
    Accept,
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
    /// First visible column of the right panel (the view clamps it).
    pub hscroll: usize,
    /// The query of the last search, for `]` and `[`.
    pub search: Option<String>,
    /// Line of the last search jump: the view may clamp `scroll` below it near
    /// the end of the panel, and the next jump must still go on from it.
    found: Option<usize>,
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
    /// First line of the `?` popup shown when it does not fit the screen.
    pub help_scroll: usize,
    /// First line shown in the failure popup (kept within its text by the renderer).
    pub failure_scroll: usize,
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
            hscroll: 0,
            search: None,
            found: None,
            page: DEFAULT_PAGE,
            mode: Mode::Normal,
            running: None,
            worker_busy: false,
            planning_busy: false,
            help_scroll: 0,
            failure_scroll: 0,
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
            self.reset_panel();
        }
    }

    /// Back to the top left of the right panel.
    fn reset_panel(&mut self) {
        self.scroll = 0;
        self.hscroll = 0;
        self.found = None;
    }

    pub fn set_caches(&mut self, diff: DiffView, log: Vec<LogLine>, verify: String) {
        self.diff = diff;
        self.log = log;
        self.verify = verify;
    }

    /// True while a text is being typed: a paste has somewhere to go.
    pub fn accepts_text(&self) -> bool {
        match &self.mode {
            Mode::Note(_) | Mode::Search(_) => true,
            Mode::Settings(panel) => panel.editing.is_some(),
            _ => false,
        }
    }

    /// A paste (bracketed, so its line breaks are not Enter keys) is typed
    /// into the text being edited; anywhere else it is ignored, because as
    /// keys its characters would be commands.
    pub fn handle_paste(&mut self, text: &str) {
        if !self.accepts_text() {
            return;
        }
        for key in paste_keys(text) {
            // Typing a character into a text never yields a command.
            let _ = self.handle_key(key);
        }
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
            Mode::Search(buffer) => self.key_search(key, buffer),
            Mode::Help => {
                // Scrolling keys move the popup; any other key closes it.
                let max = help_scroll_max(&App::shortcuts());
                if help_key(&mut self.help_scroll, max, key) {
                    self.mode = Mode::Normal;
                }
                None
            }
            Mode::Failure => {
                // The renderer holds the scroll within the text.
                if help_key(&mut self.failure_scroll, usize::MAX, key) {
                    self.mode = Mode::Normal;
                }
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

    fn key_search(&mut self, key: KeyEvent, mut buffer: String) -> Option<Command> {
        match key.code {
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                if buffer.is_empty() {
                    self.search = None;
                } else {
                    self.search = Some(buffer);
                    self.find(true);
                }
            }
            KeyCode::Esc => self.mode = Mode::Normal,
            _ => {
                if edit_text(&mut buffer, key) {
                    self.mode = Mode::Search(buffer);
                }
            }
        }
        None
    }

    /// Jumps to the next (or previous) line of the right panel that contains
    /// the stored query, wrapping around; the match becomes the first line.
    fn find(&mut self, forward: bool) {
        let Some(query) = self.search.clone() else {
            self.message = Some(Notice::warn("Press / to search."));
            return;
        };
        let needle = query.to_lowercase();
        let texts: Vec<String> = super::view::panel_lines(self)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .to_lowercase()
            })
            .collect();
        let total = texts.len();
        let last = total.saturating_sub(1);
        let top = self.scroll.min(last);
        let base = match self.found {
            Some(at) if self.scroll == at.min(total.saturating_sub(usize::from(self.page))) => at,
            _ => top,
        };
        let hit = (1..=total)
            .map(|step| {
                if forward {
                    (base + step) % total
                } else {
                    (base + total - step % total) % total
                }
            })
            .find(|&i| texts[i].contains(&needle));
        match hit {
            Some(line) => {
                self.scroll = line;
                self.found = Some(line);
            }
            None => self.message = Some(Notice::warn(format!("No match for \"{query}\"."))),
        }
    }

    /// Scrolls the right panel by one line.
    fn scroll_line(&mut self, down: bool) {
        let total = super::view::panel_lines(self).len();
        let max = total.saturating_sub(usize::from(self.page));
        let scroll = self.scroll.min(max);
        self.scroll = if down {
            (scroll + 1).min(max)
        } else {
            scroll.saturating_sub(1)
        };
    }

    fn key_normal(&mut self, key: KeyEvent) -> Option<Command> {
        // A control key is never a command but Ctrl-C: a habit from the shell
        // (Ctrl-N, Ctrl-R, Ctrl-L) must not approve a phase and start the next
        // one, open the retry note or scroll the panel. Same as the home screen.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return (key.code == KeyCode::Char('c'))
                .then(|| self.quit())
                .flatten();
        }
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Esc => Some(Command::Home),
            KeyCode::Char('A') => self.abandon(),
            KeyCode::Char('f') => {
                match self.view.phases.get(self.selected) {
                    Some(p) if p.failure_kind.is_some() => {
                        self.mode = Mode::Failure;
                        self.failure_scroll = 0;
                    }
                    Some(p) => {
                        self.message =
                            Some(Notice::warn(format!("Phase {} has not failed.", p.number)));
                    }
                    None => self.message = Some(Notice::warn("There is no phase.")),
                }
                None
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                self.help_scroll = 0;
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
                self.reset_panel();
                None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.scroll = 0;
                None
            }
            // The view clamps the scroll to the end of the content.
            KeyCode::Char('G') | KeyCode::End => {
                self.scroll = usize::MAX;
                None
            }
            KeyCode::Char('J') => {
                self.scroll_line(true);
                None
            }
            KeyCode::Char('K') => {
                self.scroll_line(false);
                None
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.hscroll = self.hscroll.saturating_add(HSCROLL_STEP);
                None
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.hscroll = self.hscroll.saturating_sub(HSCROLL_STEP);
                None
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Search(String::new());
                None
            }
            KeyCode::Char(']') => {
                self.find(true);
                None
            }
            KeyCode::Char('[') => {
                self.find(false);
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
            KeyCode::Char('a') => match self.why_not_accept() {
                None => Some(Command::Accept),
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
        self.reset_panel();
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
            return Some(format!(
                "Phase {n} failed: press r to retry it, or a to accept your fixes."
            ));
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

    fn why_not_accept(&self) -> Option<String> {
        if !self.view.active {
            return Some("The run is not active: nothing to accept.".to_string());
        }
        if let Some(why) = self.running_reason() {
            return Some(why);
        }
        if self.worker_busy {
            return Some("Wait for the running work to finish first.".to_string());
        }
        self.first_with(PhaseStatus::Failed)
            .is_none()
            .then(|| "Nothing to accept: no phase failed.".to_string())
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
            Mode::Search(_) => return vec![item("enter", "search"), item("esc", "cancel")],
            Mode::Help | Mode::Failure => return vec![item("j/k", "scroll")],
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
            keys.extend([
                item("r", "retry"),
                item("a", "accept"),
                item("b", "roll back"),
                item("f", "failure"),
            ]);
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
                    ("f", "show the whole failure and its logs"),
                ],
            ),
            (
                "Run",
                vec![
                    ("n", "approve the phase and run the next one"),
                    ("r", "retry the phase, with an optional note"),
                    ("a", "accept your fixes of the failed phase"),
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
            (
                "Right panel",
                vec![
                    ("PgUp / PgDn", "scroll by a page"),
                    ("J / K", "scroll by a line"),
                    ("g / G", "jump to the top or the bottom (Home / End)"),
                    ("h / l", "scroll sideways (left / right arrows)"),
                    ("/", "search the panel, case-insensitive"),
                    ("] / [", "go to the next or previous match"),
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
            logs: Vec::new(),
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
        assert_eq!(
            text(&failed),
            Some("Phase 2 failed: press r to retry it, or a to accept your fixes.")
        );

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
    fn accept_works_on_a_failed_phase_and_is_refused_otherwise() {
        let failed = || {
            app(vec![
                phase(1, PhaseStatus::Done, true),
                phase(2, PhaseStatus::Failed, false),
            ])
        };
        let mut a = failed();
        assert_eq!(a.handle_key(key('a')), Some(Command::Accept));
        // The selected phase need not be the failed one.
        a.selected = 0;
        assert_eq!(a.handle_key(key('a')), Some(Command::Accept));
        assert!(keys(&failed()).contains(&"a".to_string()));

        let mut none = reviewing();
        assert_eq!(none.handle_key(key('a')), None);
        assert!(text(&none).is_some_and(|m| m.contains("Nothing to accept")));

        let mut running = failed();
        running.running = Some(2);
        assert_eq!(running.handle_key(key('a')), None);
        assert!(text(&running).is_some_and(|m| m.contains("running")));

        let mut busy = failed();
        busy.worker_busy = true;
        assert_eq!(busy.handle_key(key('a')), None);
        assert!(busy.message.is_some());

        let mut inactive = failed();
        inactive.view.active = false;
        assert_eq!(inactive.handle_key(key('a')), None);
        assert!(text(&inactive).is_some_and(|m| m.contains("not active")));
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
        assert_eq!(keys(&failed), ["r", "a", "b", "f", "tab", "esc", "q", "s"]);
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
        assert_eq!(keys(&confirm), ["j/k"]);
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
        assert_eq!(groups.len(), 4);
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

    /// A Verify tab of 30 lines, `line 00` .. `line 29`, a page of 5.
    fn panel() -> App {
        let mut a = reviewing();
        a.tab = Tab::Verify;
        a.verify = (0..30).map(|i| format!("line {i:02}\n")).collect();
        a.page = 5;
        a
    }

    fn type_search(a: &mut App, text: &str) {
        a.handle_key(key('/'));
        for c in text.chars() {
            a.handle_key(key(c));
        }
        a.handle_key(code(KeyCode::Enter));
    }

    #[test]
    fn g_and_home_go_to_the_top_and_capital_g_and_end_to_the_bottom() {
        let mut a = panel();
        a.scroll = 7;
        a.handle_key(key('g'));
        assert_eq!(a.scroll, 0);
        a.scroll = 7;
        a.handle_key(code(KeyCode::Home));
        assert_eq!(a.scroll, 0);
        a.handle_key(key('G'));
        assert_eq!(a.scroll, usize::MAX);
        a.scroll = 0;
        a.handle_key(code(KeyCode::End));
        assert_eq!(a.scroll, usize::MAX);
    }

    #[test]
    fn capital_j_and_k_scroll_one_line_within_the_content() {
        let mut a = panel();
        a.handle_key(key('J'));
        a.handle_key(key('J'));
        assert_eq!(a.scroll, 2);
        a.handle_key(key('K'));
        assert_eq!(a.scroll, 1);
        a.scroll = 0;
        a.handle_key(key('K'));
        assert_eq!(a.scroll, 0);
        // From the bottom (`G`) one line up is relative to the real end.
        a.handle_key(key('G'));
        a.handle_key(key('J'));
        assert_eq!(a.scroll, 25);
        a.handle_key(key('K'));
        assert_eq!(a.scroll, 24);
        assert_eq!(a.selected, 1, "lowercase j/k still select phases");
    }

    #[test]
    fn h_l_and_the_arrows_scroll_8_columns_sideways() {
        let mut a = panel();
        a.handle_key(key('l'));
        assert_eq!(a.hscroll, 8);
        a.handle_key(code(KeyCode::Right));
        assert_eq!(a.hscroll, 16);
        a.handle_key(key('h'));
        assert_eq!(a.hscroll, 8);
        a.handle_key(code(KeyCode::Left));
        a.handle_key(code(KeyCode::Left));
        assert_eq!(a.hscroll, 0);
    }

    #[test]
    fn the_scroll_resets_with_the_tab_and_the_selected_phase() {
        let mut a = panel();
        a.scroll = 9;
        a.hscroll = 16;
        a.handle_key(code(KeyCode::Tab));
        assert_eq!((a.scroll, a.hscroll), (0, 0));
        a.scroll = 9;
        a.hscroll = 16;
        a.handle_key(key('j'));
        assert_eq!((a.scroll, a.hscroll), (0, 0));
    }

    #[test]
    fn a_paste_goes_into_the_note_and_the_search_and_nowhere_else() {
        let mut a = reviewing();
        // Not a text: as keys, `r` would open the note and `x` interrupt.
        a.handle_paste("rx");
        assert_eq!(a.mode, Mode::Normal);

        a.handle_key(key('/'));
        a.handle_paste("needle\nhaystack");
        assert_eq!(a.mode, Mode::Search("needle haystack".into()));
        a.handle_key(code(KeyCode::Esc));

        a.mode = Mode::Note("try ".into());
        a.handle_paste("again\r\nplease");
        assert_eq!(a.mode, Mode::Note("try again please".into()));
    }

    fn failed_app() -> App {
        let mut failed = phase(2, PhaseStatus::Failed, false);
        failed.failure_kind = Some("Checks failed".into());
        failed.failure_detail = Some("cargo test failed".into());
        app(vec![phase(1, PhaseStatus::Done, true), failed])
    }

    #[test]
    fn f_opens_the_failure_of_a_failed_phase_and_any_other_key_closes_it() {
        let mut a = failed_app();
        assert_eq!(a.selected_number(), Some(2));
        assert_eq!(a.handle_key(key('f')), None);
        assert_eq!(a.mode, Mode::Failure);
        assert_eq!(a.footer().len(), 1);
        // The scrolling keys move it; the renderer holds the scroll in range.
        a.handle_key(key('j'));
        a.handle_key(code(KeyCode::PageDown));
        assert_eq!(a.mode, Mode::Failure);
        assert_eq!(a.failure_scroll, 6);
        a.handle_key(key('k'));
        assert_eq!(a.failure_scroll, 5);
        assert_eq!(a.handle_key(key('x')), None, "a key that closes is not run");
        assert_eq!(a.mode, Mode::Normal);
        // Opened again, it starts at the top.
        a.handle_key(key('f'));
        assert_eq!(a.failure_scroll, 0);
    }

    #[test]
    fn f_on_a_phase_that_did_not_fail_says_so() {
        let mut a = reviewing();
        a.handle_key(key('f'));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(
            a.message.as_ref().map(|m| m.text.as_str()),
            Some("Phase 2 has not failed.")
        );
    }

    #[test]
    fn control_keys_other_than_ctrl_c_do_nothing() {
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut a = reviewing();
        // Phase 2 awaits review and is selected: plain `n` approves it, Ctrl-N
        // is not that.
        for c in [
            'n', 'r', 'b', 'm', 'x', 'l', 'h', 'j', 'k', 'g', 'G', 's', '/', '?',
        ] {
            assert_eq!(a.handle_key(ctrl(c)), None, "ctrl-{c}");
            assert_eq!(a.mode, Mode::Normal, "ctrl-{c}");
            assert_eq!(a.hscroll, 0, "ctrl-{c}");
            assert!(a.message.is_none(), "ctrl-{c}");
        }
        assert_eq!(a.selected_number(), Some(2));
        assert_eq!(a.handle_key(key('n')), Some(Command::Next));
        // Ctrl-C still quits.
        assert_eq!(a.handle_key(ctrl('c')), Some(Command::Quit));
    }

    #[test]
    fn slash_opens_the_search_and_esc_cancels_it() {
        let mut a = panel();
        a.handle_key(key('/'));
        assert_eq!(a.mode, Mode::Search(String::new()));
        a.handle_key(key('l'));
        a.handle_key(key('x'));
        a.handle_key(code(KeyCode::Backspace));
        assert_eq!(a.mode, Mode::Search("l".into()));
        assert_eq!(
            a.footer(),
            vec![item("enter", "search"), item("esc", "cancel")]
        );
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.search, None);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn enter_jumps_to_the_next_match_ignoring_case() {
        let mut a = panel();
        type_search(&mut a, "LINE 07");
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.search.as_deref(), Some("LINE 07"));
        assert_eq!(a.scroll, 7);
        // The search starts after the top line and wraps around.
        type_search(&mut a, "line 03");
        assert_eq!(a.scroll, 3);
    }

    #[test]
    fn a_search_without_a_match_warns_and_an_empty_one_clears_it() {
        let mut a = panel();
        a.scroll = 4;
        type_search(&mut a, "zzz");
        let notice = a.message.clone().expect("a notice");
        assert_eq!(notice.level, Level::Warn);
        assert_eq!(notice.text, "No match for \"zzz\".");
        assert_eq!(a.scroll, 4);
        type_search(&mut a, "");
        assert_eq!(a.search, None);
    }

    #[test]
    fn brackets_go_to_the_next_and_previous_match_and_wrap() {
        let mut a = panel();
        type_search(&mut a, "line 1");
        assert_eq!(a.scroll, 10);
        a.handle_key(key(']'));
        assert_eq!(a.scroll, 11);
        for _ in 0..8 {
            a.handle_key(key(']'));
        }
        assert_eq!(a.scroll, 19);
        a.handle_key(key(']'));
        assert_eq!(a.scroll, 10, "wraps to the first match");
        a.handle_key(key('['));
        assert_eq!(a.scroll, 19, "wraps to the last match");
        a.handle_key(key('['));
        assert_eq!(a.scroll, 18);
    }

    #[test]
    fn brackets_without_a_query_say_how_to_search() {
        let mut a = panel();
        a.handle_key(key(']'));
        assert_eq!(
            a.message.as_ref().map(|n| n.text.as_str()),
            Some("Press / to search.")
        );
        a.handle_key(key('['));
        assert_eq!(a.message.as_ref().map(|n| n.level), Some(Level::Warn));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn n_still_approves_and_does_not_search_next() {
        let mut a = panel();
        type_search(&mut a, "line 1");
        assert_eq!(a.handle_key(key('n')), Some(Command::Next));
    }

    #[test]
    fn the_help_scrolls_with_j_k_and_pages_and_any_other_key_closes_it() {
        let mut a = reviewing();
        a.handle_key(key('?'));
        assert_eq!(a.mode, Mode::Help);
        a.handle_key(key('j'));
        a.handle_key(code(KeyCode::Down));
        assert_eq!((a.mode.clone(), a.help_scroll), (Mode::Help, 2));
        a.handle_key(code(KeyCode::PageDown));
        assert_eq!(a.help_scroll, 7);
        a.handle_key(key('k'));
        a.handle_key(code(KeyCode::PageUp));
        assert_eq!((a.mode.clone(), a.help_scroll), (Mode::Help, 1));
        for _ in 0..100 {
            a.handle_key(code(KeyCode::PageDown));
        }
        assert_eq!(a.help_scroll, help_scroll_max(&App::shortcuts()));
        a.handle_key(code(KeyCode::Char('x')));
        assert_eq!(a.mode, Mode::Normal);
        // Opening it again starts at the top.
        a.handle_key(key('?'));
        assert_eq!(a.help_scroll, 0);
    }
}
