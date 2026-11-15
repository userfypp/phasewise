//! Pieces both screens share: notices, the spinner, the two-row confirmation
//! pickers, text editing and footer items. Pure: no terminal, no I/O.

use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::settings::{Kind, REGISTRY, Settings};

/// How serious a message is; the renderer colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Error,
    Info,
}

/// The transient result of the last action, shown above the footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: Level,
    pub text: String,
}

impl Notice {
    fn new(level: Level, text: impl Into<String>) -> Notice {
        Notice {
            level,
            text: text.into(),
        }
    }

    pub fn ok(text: impl Into<String>) -> Notice {
        Notice::new(Level::Ok, text)
    }

    pub fn warn(text: impl Into<String>) -> Notice {
        Notice::new(Level::Warn, text)
    }

    pub fn error(text: impl Into<String>) -> Notice {
        Notice::new(Level::Error, text)
    }

    pub fn info(text: impl Into<String>) -> Notice {
        Notice::new(Level::Info, text)
    }
}

/// Spinner frames of the running work.
pub const SPINNER: [&str; 10] = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"];
/// How long one spinner frame lasts.
pub const FRAME_TIME: Duration = Duration::from_millis(120);

/// Index of the spinner frame for the time elapsed since a fixed start.
pub fn spinner_frame(elapsed: Duration) -> usize {
    let frames = elapsed.as_millis() / FRAME_TIME.as_millis();
    usize::try_from(frames % SPINNER.len() as u128).unwrap_or(0)
}

/// The glyph of a frame index (any index; it wraps).
pub fn spinner(frame: usize) -> &'static str {
    SPINNER.get(frame % SPINNER.len()).copied().unwrap_or("·")
}

/// What a key means in a two-row confirmation (row 0 acts, row 1 cancels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Act,
    Cancel,
    /// Stay open with this row highlighted.
    Row(usize),
}

pub fn confirm_key(key: KeyEvent, row: usize) -> Choice {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return Choice::Row(row);
    }
    match key.code {
        KeyCode::Char('y' | 'Y' | '1') => Choice::Act,
        KeyCode::Char('n' | 'N' | '2') | KeyCode::Esc => Choice::Cancel,
        KeyCode::Char('j') | KeyCode::Down => Choice::Row(1),
        KeyCode::Char('k') | KeyCode::Up => Choice::Row(0),
        KeyCode::Enter if row == 0 => Choice::Act,
        KeyCode::Enter => Choice::Cancel,
        _ => Choice::Row(row),
    }
}

/// The row a digit key (1 to 9) picks directly in a picker.
pub fn digit_row(key: KeyEvent) -> Option<usize> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char(c @ '1'..='9') => Some(usize::from(c as u8 - b'1')),
        _ => None,
    }
}

/// Applies a text-editing key to `buffer`: characters, Backspace, Ctrl-U
/// (clear) and Ctrl-W (delete the last word). False for any other key.
pub fn edit_text(buffer: &mut String, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Backspace => {
            buffer.pop();
            true
        }
        KeyCode::Char('u') if ctrl => {
            buffer.clear();
            true
        }
        KeyCode::Char('w') if ctrl => {
            let kept = buffer.trim_end().len();
            buffer.truncate(kept);
            let start = buffer
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_whitespace())
                .map_or(0, |(i, c)| i + c.len_utf8());
            buffer.truncate(start);
            true
        }
        KeyCode::Char(c) if !ctrl => {
            buffer.push(c);
            true
        }
        _ => false,
    }
}

/// A footer entry: key and action.
pub type FooterItem = (String, String);

pub fn item(key: &str, action: &str) -> FooterItem {
    (key.to_string(), action.to_string())
}

/// Footer of a picker with `rows` rows.
pub fn picker_footer(rows: usize) -> Vec<FooterItem> {
    vec![
        item("enter", "choose"),
        item(&format!("1–{}", rows.clamp(1, 9)), "jump"),
        item("esc", "cancel"),
    ]
}

/// What a key did in the settings popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelOutcome {
    Stay,
    Close,
    /// A value changed: the caller saves `SettingsPanel::settings`.
    Changed,
}

/// The settings popup of both screens: the list, its selection and the inline
/// edit of the free-text setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPanel {
    pub settings: Settings,
    pub selected: usize,
    /// The text being typed for a free-text setting.
    pub editing: Option<String>,
    /// Why the typed text was refused.
    pub error: Option<String>,
}

impl SettingsPanel {
    pub fn new(settings: Settings) -> SettingsPanel {
        SettingsPanel {
            settings,
            selected: 0,
            editing: None,
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PanelOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.editing.is_some() {
            return self.key_editing(key);
        }
        if ctrl {
            return PanelOutcome::Stay;
        }
        let last = REGISTRY.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('s' | 'q') => PanelOutcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(last);
                PanelOutcome::Stay
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                PanelOutcome::Stay
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            _ => PanelOutcome::Stay,
        }
    }

    /// Toggles or cycles the selected setting, or starts editing a text one.
    fn activate(&mut self) -> PanelOutcome {
        let Some(spec) = REGISTRY.get(self.selected) else {
            return PanelOutcome::Stay;
        };
        if spec.kind == Kind::Text {
            self.editing = Some(self.settings.get(spec.key));
            self.error = None;
            return PanelOutcome::Stay;
        }
        self.settings.cycle(spec.key);
        PanelOutcome::Changed
    }

    fn key_editing(&mut self, key: KeyEvent) -> PanelOutcome {
        let Some(mut buffer) = self.editing.take() else {
            return PanelOutcome::Stay;
        };
        match key.code {
            KeyCode::Esc => {
                self.error = None;
                PanelOutcome::Stay
            }
            KeyCode::Enter => {
                let Some(spec) = REGISTRY.get(self.selected) else {
                    return PanelOutcome::Stay;
                };
                let before = self.settings.get(spec.key);
                match self.settings.set(spec.key, &buffer) {
                    Ok(()) => {
                        self.error = None;
                        if self.settings.get(spec.key) == before {
                            PanelOutcome::Stay
                        } else {
                            PanelOutcome::Changed
                        }
                    }
                    Err(why) => {
                        self.error = Some(why);
                        self.editing = Some(buffer);
                        PanelOutcome::Stay
                    }
                }
            }
            _ => {
                edit_text(&mut buffer, key);
                self.editing = Some(buffer);
                PanelOutcome::Stay
            }
        }
    }

    /// Footer of the popup.
    pub fn footer(&self) -> Vec<FooterItem> {
        if self.editing.is_some() {
            return vec![item("enter", "confirm"), item("esc", "cancel")];
        }
        let spec = REGISTRY.get(self.selected);
        let action = match spec.map(|s| s.kind) {
            Some(Kind::Text) => "edit",
            Some(Kind::Bool) => "toggle",
            _ => "change",
        };
        vec![
            item("enter", action),
            item("j/k", "move"),
            item("esc", "close"),
        ]
    }
}

/// The keys of the settings popup for the `?` popups.
pub const SETTINGS_SHORTCUT: (&str, &str) = ("s", "open the settings menu");

/// Shortcuts of a screen, in groups: `(group, [(key, description)])`.
pub type ShortcutGroups = Vec<(&'static str, Vec<(&'static str, &'static str)>)>;

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn spinner_advances_every_120_ms_and_wraps() {
        assert_eq!(spinner_frame(Duration::from_millis(0)), 0);
        assert_eq!(spinner_frame(Duration::from_millis(119)), 0);
        assert_eq!(spinner_frame(Duration::from_millis(120)), 1);
        assert_eq!(spinner_frame(Duration::from_millis(1200)), 0);
        assert_eq!(spinner(4), "✻");
        assert_eq!(spinner(14), "✻");
    }

    #[test]
    fn confirmation_keys() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(confirm_key(key('y'), 1), Choice::Act);
        assert_eq!(confirm_key(key('1'), 1), Choice::Act);
        assert_eq!(confirm_key(key('n'), 0), Choice::Cancel);
        assert_eq!(confirm_key(key('2'), 0), Choice::Cancel);
        assert_eq!(confirm_key(esc, 0), Choice::Cancel);
        assert_eq!(confirm_key(key('j'), 0), Choice::Row(1));
        assert_eq!(confirm_key(key('k'), 1), Choice::Row(0));
        assert_eq!(confirm_key(enter, 0), Choice::Act);
        assert_eq!(confirm_key(enter, 1), Choice::Cancel);
        assert_eq!(confirm_key(key('z'), 1), Choice::Row(1));
        assert_eq!(confirm_key(ctrl('y'), 0), Choice::Row(0));
    }

    #[test]
    fn digits_pick_rows_one_to_nine() {
        assert_eq!(digit_row(key('1')), Some(0));
        assert_eq!(digit_row(key('9')), Some(8));
        assert_eq!(digit_row(key('0')), None);
        assert_eq!(digit_row(key('a')), None);
        assert_eq!(digit_row(ctrl('1')), None);
    }

    #[test]
    fn text_editing_keys() {
        let mut s = String::from("add a CLI");
        assert!(edit_text(&mut s, ctrl('w')));
        assert_eq!(s, "add a ");
        assert!(edit_text(&mut s, ctrl('w')));
        assert_eq!(s, "add ");
        assert!(edit_text(&mut s, key('x')));
        assert!(edit_text(
            &mut s,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
        ));
        assert_eq!(s, "add ");
        assert!(edit_text(&mut s, ctrl('u')));
        assert_eq!(s, "");
        assert!(edit_text(&mut s, ctrl('w')));
        assert!(!edit_text(&mut s, ctrl('a')));
        assert!(!edit_text(
            &mut s,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
        ));
    }

    #[test]
    fn footer_of_a_picker_names_its_rows() {
        assert_eq!(picker_footer(2)[1], item("1–2", "jump"));
        assert_eq!(picker_footer(30)[1], item("1–9", "jump"));
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn index_of(key: &str) -> usize {
        REGISTRY
            .iter()
            .position(|s| s.key == key)
            .expect("a setting")
    }

    #[test]
    fn the_settings_list_moves_clamps_and_closes() {
        let mut panel = SettingsPanel::new(Settings::default());
        assert_eq!(panel.handle_key(key('k')), PanelOutcome::Stay);
        assert_eq!(panel.selected, 0);
        assert_eq!(panel.handle_key(key('j')), PanelOutcome::Stay);
        assert_eq!(panel.handle_key(code(KeyCode::Down)), PanelOutcome::Stay);
        assert_eq!(panel.selected, 2);
        assert_eq!(panel.handle_key(code(KeyCode::Up)), PanelOutcome::Stay);
        assert_eq!(panel.selected, 1);
        for _ in 0..20 {
            panel.handle_key(key('j'));
        }
        assert_eq!(panel.selected, REGISTRY.len() - 1);
        assert_eq!(panel.handle_key(key('x')), PanelOutcome::Stay);
        for close in [code(KeyCode::Esc), key('s'), key('q')] {
            assert_eq!(panel.handle_key(close), PanelOutcome::Close);
        }
        assert_eq!(panel.handle_key(ctrl('s')), PanelOutcome::Stay);
    }

    #[test]
    fn enter_or_space_toggles_a_bool_and_cycles_a_choice() {
        let mut panel = SettingsPanel::new(Settings::default());
        assert!(panel.settings.limits());
        assert_eq!(
            panel.handle_key(code(KeyCode::Enter)),
            PanelOutcome::Changed
        );
        assert!(!panel.settings.limits());
        assert_eq!(panel.handle_key(key(' ')), PanelOutcome::Changed);
        assert!(panel.settings.limits());
        panel.selected = index_of("color");
        assert_eq!(
            panel.handle_key(code(KeyCode::Enter)),
            PanelOutcome::Changed
        );
        assert!(panel.settings.color_never());
        assert_eq!(
            panel.handle_key(code(KeyCode::Enter)),
            PanelOutcome::Changed
        );
        assert!(!panel.settings.color_never());
    }

    #[test]
    fn the_model_is_edited_inline_and_confirmed_with_enter() {
        let mut panel = SettingsPanel::new(Settings::default());
        panel.selected = index_of("model");
        assert_eq!(panel.handle_key(code(KeyCode::Enter)), PanelOutcome::Stay);
        assert_eq!(panel.editing.as_deref(), Some(""));
        // Keys that close or move the list are text while editing.
        for c in "opus".chars() {
            assert_eq!(panel.handle_key(key(c)), PanelOutcome::Stay);
        }
        panel.handle_key(key('s'));
        panel.handle_key(code(KeyCode::Backspace));
        assert_eq!(panel.editing.as_deref(), Some("opus"));
        assert_eq!(
            panel.footer(),
            vec![item("enter", "confirm"), item("esc", "cancel")]
        );
        assert_eq!(
            panel.handle_key(code(KeyCode::Enter)),
            PanelOutcome::Changed
        );
        assert_eq!(panel.editing, None);
        assert_eq!(panel.settings.model().as_deref(), Some("opus"));
        // Editing again and confirming the same text changes nothing.
        panel.handle_key(code(KeyCode::Enter));
        assert_eq!(panel.editing.as_deref(), Some("opus"));
        assert_eq!(panel.handle_key(code(KeyCode::Enter)), PanelOutcome::Stay);
    }

    #[test]
    fn esc_cancels_an_edit_and_a_bad_model_stays_open_with_the_reason() {
        let mut panel = SettingsPanel::new(Settings::default());
        panel.selected = index_of("model");
        panel.handle_key(code(KeyCode::Enter));
        for c in "a b".chars() {
            panel.handle_key(key(c));
        }
        assert_eq!(panel.handle_key(code(KeyCode::Enter)), PanelOutcome::Stay);
        assert!(panel.editing.is_some());
        assert!(panel.error.is_some());
        assert_eq!(panel.handle_key(code(KeyCode::Esc)), PanelOutcome::Stay);
        assert_eq!(panel.editing, None);
        assert_eq!(panel.error, None);
        assert_eq!(panel.settings.model(), None);
        // Esc out of an edit does not close the popup; a second one does.
        assert_eq!(panel.handle_key(code(KeyCode::Esc)), PanelOutcome::Close);
    }

    #[test]
    fn the_settings_footer_names_the_action_of_the_row() {
        let mut panel = SettingsPanel::new(Settings::default());
        assert_eq!(panel.footer()[0], item("enter", "toggle"));
        panel.selected = index_of("agent");
        assert_eq!(panel.footer()[0], item("enter", "change"));
        panel.selected = index_of("model");
        assert_eq!(panel.footer()[0], item("enter", "edit"));
    }
}
