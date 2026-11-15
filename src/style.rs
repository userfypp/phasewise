//! Terminal colours for the CLI output. The TUI uses ratatui styles instead;
//! both map the same [`Role`]s.

use std::io::IsTerminal;

use ratatui::crossterm::style::{Color, Stylize, force_color_output};

/// What a piece of text means; colours are chosen per role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Plain text, never coloured.
    Default,
    Heading,
    Dim,
    Ok,
    Fail,
    Warn,
    Accent,
    Tool,
}

/// Wraps text in ANSI codes when `color` is true; otherwise returns it as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Styler {
    pub color: bool,
}

/// Pure decision behind [`Styler::for_stdout`]: colour needs a terminal, an
/// unset or empty `NO_COLOR`, a `TERM` other than `dumb` and a `color` setting
/// other than `never`. The environment wins: the setting can only turn colour
/// off, never on.
pub fn color_enabled(
    is_terminal: bool,
    no_color: Option<&str>,
    term: Option<&str>,
    never: bool,
) -> bool {
    is_terminal && no_color.is_none_or(str::is_empty) && term != Some("dumb") && !never
}

impl Styler {
    pub fn for_stdout() -> Styler {
        let no_color = std::env::var("NO_COLOR").ok();
        let term = std::env::var("TERM").ok();
        let env_allows = color_enabled(
            std::io::stdout().is_terminal(),
            no_color.as_deref(),
            term.as_deref(),
            false,
        );
        // The file is only read when colour is still possible.
        Styler {
            color: env_allows && !crate::settings::current().color_never(),
        }
    }

    pub fn paint(&self, role: Role, text: &str) -> String {
        if !self.color || role == Role::Default {
            return text.to_string();
        }
        // crossterm would otherwise drop the codes when NO_COLOR is set, which
        // this type already decided about.
        force_color_output(true);
        match role {
            Role::Default => text.to_string(),
            Role::Heading => text.bold().to_string(),
            Role::Dim => text.with(Color::DarkGrey).to_string(),
            Role::Ok => text.with(Color::Green).to_string(),
            Role::Fail => text.with(Color::Red).to_string(),
            Role::Warn => text.with(Color::Yellow).to_string(),
            Role::Accent => text.with(Color::Cyan).to_string(),
            Role::Tool => text.with(Color::Blue).to_string(),
        }
    }

    pub fn heading(&self, text: &str) -> String {
        self.paint(Role::Heading, text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(Role::Dim, text)
    }

    pub fn ok(&self, text: &str) -> String {
        self.paint(Role::Ok, text)
    }

    pub fn fail(&self, text: &str) -> String {
        self.paint(Role::Fail, text)
    }

    pub fn warn(&self, text: &str) -> String {
        self.paint(Role::Warn, text)
    }

    pub fn accent(&self, text: &str) -> String {
        self.paint(Role::Accent, text)
    }

    pub fn tool(&self, text: &str) -> String {
        self.paint(Role::Tool, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLES: [Role; 7] = [
        Role::Heading,
        Role::Dim,
        Role::Ok,
        Role::Fail,
        Role::Warn,
        Role::Accent,
        Role::Tool,
    ];

    #[test]
    fn plain_styler_returns_the_text_unchanged() {
        let plain = Styler { color: false };
        for role in ROLES {
            assert_eq!(plain.paint(role, "hello"), "hello");
        }
        assert_eq!(plain.heading("x"), "x");
    }

    #[test]
    fn coloured_styler_wraps_the_text_in_ansi_codes() {
        let colour = Styler { color: true };
        for role in ROLES {
            let out = colour.paint(role, "hello");
            assert!(out.starts_with("\x1b["), "{role:?}: {out:?}");
            assert!(out.contains("hello"), "{role:?}: {out:?}");
            // Every style is closed again, by a reset or by the default colour.
            assert!(
                out.ends_with("\x1b[0m") || out.ends_with("\x1b[39m"),
                "{role:?}: {out:?}"
            );
        }
        assert_eq!(colour.paint(Role::Default, "hello"), "hello");
        assert_ne!(colour.ok("a"), colour.fail("a"));
    }

    #[test]
    fn colour_needs_a_terminal_no_no_color_and_a_real_term() {
        assert!(color_enabled(true, None, Some("xterm-256color"), false));
        assert!(color_enabled(true, None, None, false));
        assert!(color_enabled(true, Some(""), Some("xterm"), false));
        assert!(!color_enabled(false, None, Some("xterm"), false));
        assert!(!color_enabled(true, Some("1"), Some("xterm"), false));
        assert!(!color_enabled(true, Some("anything"), None, false));
        assert!(!color_enabled(true, None, Some("dumb"), false));
    }

    #[test]
    fn the_never_setting_turns_colour_off_and_never_on() {
        assert!(!color_enabled(true, None, Some("xterm"), true));
        assert!(!color_enabled(false, None, Some("xterm"), true));
        assert!(!color_enabled(true, Some("1"), Some("xterm"), true));
    }
}
