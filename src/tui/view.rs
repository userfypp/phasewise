//! Rendering of the run screen. Deterministic: the output depends only on
//! the `App`.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::{App, Mode, Tab};
use super::data::{DiffView, LogLine, PhaseView};
use super::shared::spinner;
use super::widgets::{
    MIN_HEIGHT, MIN_WIDTH, accent, accent_bold, bold, char_width, confirm_popup, dim, dim_join,
    draw_too_small, duration_text, elapsed_since, event_spans, footer_line, input_popup,
    limit_spans, line_width, margin, notice_line, render_popup, role_style, sanitize,
    settings_popup, shortcuts_popup, text_width, truncate, wrap_text,
};
use crate::display::{money, status_label};
use crate::state::PhaseStatus;
use crate::style::Role;

/// The phase list is at most this share of the width, and at least `LIST_MIN`.
const LIST_PERCENT: u16 = 40;
const LIST_MIN: u16 = 24;
/// Lines of a failure's detail shown before it is cut.
const FAILURE_LINES: usize = 8;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area);
        return;
    }
    let [header, limits, body, message, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let body = margin(body);
    let list_width = list_width(app, body.width);
    let [list, rule, detail] = Layout::horizontal([
        Constraint::Length(list_width),
        Constraint::Length(3),
        Constraint::Min(1),
    ])
    .areas(body);
    draw_header(frame, app, margin(header));
    if let Some(l) = app.view.limits.as_ref().filter(|_| app.settings.limits()) {
        let area = margin(limits);
        let spans = limit_spans(l, dim(), usize::from(area.width));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
    draw_phases(frame, app, list);
    draw_rule(frame, rule);
    draw_detail(frame, app, detail);
    if let Some(notice) = &app.message {
        frame.render_widget(Paragraph::new(notice_line(notice)), margin(message));
    }
    let footer_area = margin(footer);
    let keys = app.footer();
    frame.render_widget(
        Paragraph::new(footer_line(
            &keys,
            app.mode == Mode::Normal,
            usize::from(footer_area.width),
        )),
        footer_area,
    );
    draw_popup(frame, app, area);
}

/// Width of the phase list: the longest row plus two, within 24 columns and
/// 40 % of the width.
fn list_width(app: &App, total: u16) -> u16 {
    let rows = app
        .view
        .phases
        .iter()
        .map(|p| row_prefix_width(app) + text_width(&p.title))
        .max()
        .unwrap_or(0);
    let wanted = u16::try_from(rows + 2).unwrap_or(u16::MAX);
    wanted.clamp(LIST_MIN, (total * LIST_PERCENT / 100).max(LIST_MIN))
}

/// Digits of the widest phase number, at least two.
fn number_width(app: &App) -> usize {
    app.view
        .phases
        .iter()
        .map(|p| p.number.to_string().len())
        .max()
        .unwrap_or(2)
        .max(2)
}

/// `  ◆ 12  ` in front of a title.
fn row_prefix_width(app: &App) -> usize {
    2 + 2 + number_width(app) + 2
}

fn agent_title(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        other => other,
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let v = &app.view;
    let right = format!("{} · {}", agent_title(&v.agent), v.run_branch);
    let room = usize::from(area.width).saturating_sub(text_width(&right) + 2);
    let mut left = vec![
        Span::styled("✻", accent()),
        Span::styled(" phasewise", bold()),
    ];
    if !v.plan_title.is_empty() {
        left.push(Span::styled(
            format!(" · {}", truncate(&v.plan_title, room.saturating_sub(12))),
            dim(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(left)), area);
    frame.render_widget(
        Paragraph::new(Line::styled(right, dim())).alignment(Alignment::Right),
        area,
    );
}

/// Status glyph of a phase; the spinner for work in progress.
fn glyph(status: PhaseStatus, frame: usize) -> (&'static str, Style) {
    match status {
        PhaseStatus::Pending => ("·", dim()),
        PhaseStatus::Running | PhaseStatus::Verifying => (spinner(frame), accent()),
        PhaseStatus::AwaitingReview => ("◆", role_style(Role::Warn)),
        PhaseStatus::Done => ("✓", role_style(Role::Ok)),
        PhaseStatus::Failed => ("✗", role_style(Role::Fail)),
    }
}

fn draw_phases(frame: &mut Frame, app: &App, area: Rect) {
    let numw = number_width(app);
    let room = usize::from(area.width).saturating_sub(row_prefix_width(app));
    let height = usize::from(area.height).max(1);
    // Keep the selected phase in view.
    let first = (app.selected + 1).saturating_sub(height);
    let lines: Vec<Line> = app
        .view
        .phases
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, p)| {
            let selected = i == app.selected;
            let (mark, style) = glyph(p.status, app.frame);
            let title_style = if selected { bold() } else { Style::new() };
            Line::from(vec![
                Span::styled(if selected { "❯" } else { " " }, accent()),
                Span::raw(" "),
                Span::styled(mark, style),
                Span::raw(format!(" {:>numw$}  ", p.number)),
                Span::styled(truncate(&p.title, room), title_style),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The dim `│` column between the list and the detail pane.
fn draw_rule(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = (0..area.height)
        .map(|_| Line::styled(" │", dim()))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// `Phase 2 · awaiting review · attempt 1 · 4m 12s · 2 files · $0.04 · Sonnet 5.5`.
fn meta_line(app: &App, p: &PhaseView, width: usize) -> Line<'static> {
    let mut parts = vec![format!("Phase {}", p.number)];
    let working = matches!(p.status, PhaseStatus::Running | PhaseStatus::Verifying);
    parts.push(match (working, p.started_at) {
        (true, Some(at)) => format!("{} {}", status_label(p.status), elapsed_since(at)),
        _ => status_label(p.status).to_string(),
    });
    if p.attempts > 0 {
        parts.push(format!("attempt {}", p.attempts));
    }
    if !working {
        if let (Some(start), Some(end)) = (p.started_at, p.finished_at) {
            parts.push(duration_text(end.as_second() - start.as_second()));
        }
        let files = app.diff.files.len();
        if files > 0 {
            parts.push(format!(
                "{files} {}",
                if files == 1 { "file" } else { "files" }
            ));
        }
        parts.extend(p.cost_usd.map(money));
        parts.extend(p.agent.map(|a| a.to_string()));
        parts.extend(p.model.clone());
    }
    // Whole parts are dropped from the end rather than cut.
    while parts.len() > 2 && text_width(&parts.join(" · ")) > width {
        parts.pop();
    }
    Line::from(dim_join(&parts))
}

/// The failure block: red `✗` and the kind in words, then the detail wrapped
/// to the pane, at most `FAILURE_LINES` lines (the last one ends with `…`).
fn failure_lines(p: &PhaseView, width: usize) -> Vec<Line<'static>> {
    let Some(kind) = &p.failure_kind else {
        return Vec::new();
    };
    let mut lines = vec![Line::styled(format!("✗ {kind}"), role_style(Role::Fail))];
    let mut detail: Vec<String> =
        wrap_text(p.failure_detail.as_deref().unwrap_or("").trim(), width);
    if detail.len() > FAILURE_LINES {
        detail.truncate(FAILURE_LINES);
        if let Some(last) = detail.last_mut() {
            *last = format!("{}…", truncate(last, width.saturating_sub(1)));
        }
    }
    lines.extend(detail.into_iter().map(Line::raw));
    lines
}

fn tab_line(active: Tab, position: Option<String>, width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let style = if tab == active {
            accent_bold().add_modifier(Modifier::UNDERLINED)
        } else {
            dim()
        };
        spans.push(Span::styled(tab.title(), style));
    }
    if let Some(position) = position {
        let used = line_width(&Line::from(spans.clone()));
        let pad = width.saturating_sub(used + text_width(&position));
        spans.push(Span::raw(" ".repeat(pad.max(2))));
        spans.push(Span::styled(position, dim()));
    }
    Line::from(spans)
}

fn draw_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(p) = app.view.phases.get(app.selected).cloned() else {
        frame.render_widget(Paragraph::new(Line::styled("No phases", dim())), area);
        return;
    };
    let width = usize::from(area.width);
    let mut head = vec![
        Line::styled(truncate(&p.title, width), bold()),
        meta_line(app, &p, width),
    ];
    let failure = failure_lines(&p, width);
    if !failure.is_empty() {
        head.push(Line::raw(""));
        head.extend(failure);
    }
    head.push(Line::raw(""));
    // The tab bar and the rule follow the head.
    let used = u16::try_from(head.len() + 2).unwrap_or(u16::MAX);
    let content_height = area.height.saturating_sub(used);
    let lines = panel_lines(app);
    app.page = content_height.max(1);
    let total = lines.len();
    let max_scroll = total.saturating_sub(usize::from(content_height));
    app.scroll = app.scroll.min(max_scroll);
    let position = (total > usize::from(content_height) && content_height > 0).then(|| {
        let first = app.scroll + 1;
        let last = (app.scroll + usize::from(content_height)).min(total);
        format!("{first}–{last} of {total}")
    });
    head.push(tab_line(app.tab, position, width));
    head.push(Line::styled("─".repeat(width), dim()));
    let head_height = u16::try_from(head.len())
        .unwrap_or(area.height)
        .min(area.height);
    let [top, content] =
        Layout::vertical([Constraint::Length(head_height), Constraint::Min(0)]).areas(area);
    frame.render_widget(Paragraph::new(head), top);
    // `Paragraph::scroll` takes a `u16`, which stops at 65,535 lines: cut the
    // visible window out of the lines instead.
    let widest = lines.iter().map(line_width).max().unwrap_or(0);
    app.hscroll = app.hscroll.min(widest.saturating_sub(width));
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(app.scroll)
        .take(usize::from(content_height))
        .map(|line| cut_columns(line, app.hscroll))
        .collect();
    frame.render_widget(Paragraph::new(visible), content);
}

/// The lines of the current tab's panel, as drawn (and as searched).
pub(super) fn panel_lines(app: &App) -> Vec<Line<'static>> {
    match app.tab {
        Tab::Diff => diff_lines(&app.diff),
        Tab::Log => app.log.iter().map(log_line).collect(),
        Tab::Verify => verify_lines(&app.verify),
    }
}

/// `line` without its first `skip` columns. A wide character cut in half
/// leaves a space for the column that remains.
fn cut_columns(line: Line<'static>, skip: usize) -> Line<'static> {
    if skip == 0 {
        return line;
    }
    let mut col = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        let mut kept = String::new();
        for c in span.content.chars() {
            let w = char_width(c);
            if col >= skip {
                kept.push(c);
            } else if col + w > skip {
                kept.push_str(&" ".repeat(col + w - skip));
            }
            col += w;
        }
        if !kept.is_empty() {
            spans.push(Span::styled(kept, span.style));
        }
    }
    Line { spans, ..line }
}

fn log_line(line: &LogLine) -> Line<'static> {
    match line {
        LogLine::Attempt { number, resumed } => Line::styled(
            LogLine::attempt_heading(*number, *resumed),
            role_style(Role::Heading),
        ),
        LogLine::Event(event) => Line::from(
            event_spans(event)
                .into_iter()
                .map(|mut span| {
                    span.content = sanitize(&span.content).into();
                    span
                })
                .collect::<Vec<_>>(),
        ),
    }
}

/// The verification log: `$ command` lines are headings with the result of
/// the command in front (read from the `exit code:` line that follows it),
/// output lines are plain, failure trailers are red.
fn verify_lines(text: &str) -> Vec<Line<'static>> {
    let all: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, raw) in all.iter().enumerate() {
        let clean = sanitize(raw);
        let line = clean.as_str();
        if let Some(command) = line.strip_prefix("$ ") {
            let result = all[i + 1..]
                .iter()
                .take_while(|l| !l.starts_with("$ ") && !l.starts_with("=== "))
                .find_map(|l| {
                    if *l == "exit code: 0" {
                        Some(("✓", Role::Ok))
                    } else if l.starts_with("exit code: ")
                        || l.starts_with("timed out after ")
                        || *l == "interrupted"
                    {
                        Some(("✗", Role::Fail))
                    } else {
                        None
                    }
                });
            let (icon, role) = result.unwrap_or(("⚙", Role::Dim));
            out.push(Line::from(vec![
                Span::styled(icon, role_style(role)),
                Span::raw(" "),
                Span::styled(format!("$ {command}"), role_style(Role::Heading)),
            ]));
        } else if line.starts_with("=== ") {
            out.push(Line::styled(line.to_string(), role_style(Role::Dim)));
        } else if (line.starts_with("exit code: ") && line != "exit code: 0")
            || line.starts_with("timed out after ")
            || line == "interrupted"
        {
            out.push(Line::styled(line.to_string(), role_style(Role::Fail)));
        } else {
            out.push(Line::raw(clean));
        }
    }
    out
}

fn diff_lines(diff: &DiffView) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (path, added, removed) in &diff.files {
        let counts = match (added, removed) {
            (Some(a), Some(r)) => format!("+{a} -{r}"),
            _ => "bin".to_string(),
        };
        lines.push(Line::styled(
            format!("  {}  {counts}", sanitize(path)),
            role_style(Role::Heading),
        ));
    }
    if let Some(note) = &diff.note {
        lines.push(Line::raw(sanitize(note)));
    }
    if !diff.text.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.extend(diff.text.lines().map(diff_line));
    }
    if !diff.untracked.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            "Untracked files:",
            Style::new().add_modifier(Modifier::BOLD),
        ));
        lines.extend(
            diff.untracked
                .iter()
                .map(|p| Line::raw(sanitize(&format!("  ? {p}")))),
        );
    }
    lines
}

const HEADER_PREFIXES: [&str; 9] = [
    "diff ",
    "index ",
    "+++",
    "---",
    "new file",
    "deleted file",
    "rename ",
    "similarity ",
    "Binary files",
];

fn diff_line(line: &str) -> Line<'static> {
    let style = if HEADER_PREFIXES.iter().any(|p| line.starts_with(p)) {
        Style::new().add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::new().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::new().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new()
    };
    Line::styled(sanitize(line), style)
}

fn draw_popup(frame: &mut Frame, app: &App, area: Rect) {
    let view = &app.view;
    let popup = match &app.mode {
        Mode::Normal => return,
        Mode::ConfirmRollback(n, row) => confirm_popup(
            &if *n == 0 {
                "Roll back to the start?".to_string()
            } else {
                format!("Roll back to phase {n}?")
            },
            "Later phases are reset and their changes discarded.",
            "Roll back",
            "Cancel",
            *row,
        ),
        Mode::ConfirmInterrupt(row) => confirm_popup(
            &app.running.map_or("Interrupt the phase?".to_string(), |n| {
                format!("Interrupt phase {n}?")
            }),
            "",
            "Interrupt",
            "Keep running",
            *row,
        ),
        Mode::ConfirmQuit(row) => confirm_popup(
            "Quit phasewise?",
            if app.running.is_some() {
                "The running phase is interrupted."
            } else {
                "The plan being written is discarded."
            },
            "Quit",
            "Keep working",
            *row,
        ),
        Mode::ConfirmAbandon(row) => confirm_popup(
            &format!("Abandon run {}?", view.run_id),
            &format!(
                "The run stops for good. Uncommitted changes are saved in a commit on {}, which is kept, and the repository goes back to {}.",
                view.run_branch, view.original_branch
            ),
            "Abandon it",
            "Cancel",
            *row,
        ),
        Mode::ConfirmMerge(row) => confirm_popup(
            &format!("Merge run {}?", view.run_id),
            &format!(
                "The work of {} is squashed into {} and committed there. The run branch is kept and nothing is pushed.",
                view.run_branch, view.original_branch
            ),
            "Merge it",
            "Cancel",
            *row,
        ),
        Mode::Help => shortcuts_popup(&App::shortcuts()).scroll(app.help_scroll),
        Mode::Settings(panel) => settings_popup(panel, area.width),
        Mode::Search(buffer) => input_popup(
            "Search",
            "Case-insensitive; ] and [ go to the next and previous match.",
            buffer,
            area.width,
        ),
        Mode::Note(buffer) => input_popup(
            &format!("Retry phase {}", retry_target(app)),
            "Optional note for the agent, added to the prompt.",
            buffer,
            area.width,
        ),
    };
    render_popup(frame, area, &popup);
}

/// The phase a retry applies to: the failed one, else the one in review.
fn retry_target(app: &App) -> usize {
    let find = |status| app.view.phases.iter().find(|p| p.status == status);
    find(PhaseStatus::Failed)
        .or_else(|| find(PhaseStatus::AwaitingReview))
        .or_else(|| app.view.phases.get(app.selected))
        .map_or(0, |p| p.number)
}

#[cfg(test)]
pub(super) mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::agent::AgentEvent;
    use crate::display::agent_event_line;
    use crate::settings::Settings;
    use crate::tui::data::{PhaseView, RunView};
    use crate::tui::shared::{Notice, SettingsPanel};

    pub(in crate::tui) fn app() -> App {
        let phase = |number, title: &str, status, checkpoint| PhaseView {
            agent: None,
            number,
            title: title.to_string(),
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
        };
        let mut app = App::new(RunView {
            run_id: "20260101-120000".into(),
            plan_title: "Demo plan".into(),
            agent: "claude".into(),
            run_branch: "phasewise/20260101-120000".into(),
            original_branch: "main".into(),
            active: true,
            completed: false,
            limits: None,
            phases: vec![
                phase(1, "Scaffold the project", PhaseStatus::Done, true),
                PhaseView {
                    cost_usd: Some(0.04),
                    started_at: jiff::Timestamp::from_second(1_794_660_000).ok(),
                    finished_at: jiff::Timestamp::from_second(1_794_660_252).ok(),
                    model: Some("Sonnet 5.5".into()),
                    ..phase(2, "Add the parser", PhaseStatus::AwaitingReview, true)
                },
                phase(3, "Write the docs", PhaseStatus::Pending, false),
                PhaseView {
                    failure_kind: Some("Checks failed".into()),
                    failure_detail: Some("cargo test failed".into()),
                    ..phase(4, "Release", PhaseStatus::Failed, false)
                },
            ],
        });
        app.selected = 1;
        app
    }

    pub(in crate::tui) fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal.draw(|f| draw(f, app)).expect("draw");
        terminal
    }

    const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 111..222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n fn main() {\n-    old();\n+    new();\n+    more();\n }\n";

    pub(in crate::tui) fn with_diff(app: &mut App) {
        app.diff = DiffView {
            files: vec![
                ("src/lib.rs".into(), Some(2), Some(1)),
                ("assets/logo.png".into(), None, None),
            ],
            text: DIFF.to_string(),
            untracked: vec!["notes.txt".into()],
            note: None,
        };
    }

    /// Sizes for the sweeps: every one up to a few cells (the "too small"
    /// message), and a spread around the smallest size the screens draw at.
    pub(in crate::tui) fn sizes() -> Vec<(u16, u16)> {
        let small = (0..=8).flat_map(|w| (0..=8).map(move |h| (w, h)));
        let around_the_minimum = [MIN_WIDTH - 1, MIN_WIDTH, MIN_WIDTH + 1, 100, 140, 300]
            .into_iter()
            .flat_map(|w| {
                [MIN_HEIGHT - 1, MIN_HEIGHT, MIN_HEIGHT + 1, 30, 60]
                    .into_iter()
                    .map(move |h| (w, h))
            });
        small.chain(around_the_minimum).collect()
    }

    #[test]
    fn no_mode_panics_at_any_size() {
        use crate::tui::app::Mode;
        let modes = [
            Mode::Normal,
            Mode::Help,
            Mode::ConfirmRollback(1, 0),
            Mode::ConfirmInterrupt(0),
            Mode::ConfirmQuit(0),
            Mode::ConfirmAbandon(0),
            Mode::ConfirmMerge(0),
            Mode::Note("a note with some words in it".into()),
            Mode::Search("needle".into()),
            Mode::Settings(SettingsPanel::new(Settings::default())),
        ];
        for mode in modes {
            for tab in Tab::ALL {
                let mut app = app();
                with_diff(&mut app);
                app.tab = tab;
                app.mode = mode.clone();
                app.message = Some(Notice::warn(
                    "A message that is long enough to wrap, probably",
                ));
                for (width, height) in sizes() {
                    render(&mut app, width, height);
                }
            }
        }
    }

    #[test]
    fn phase_list_and_diff_tab() {
        let mut app = app();
        with_diff(&mut app);
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn limits_line_under_the_header() {
        let mut app = app();
        app.view.limits = Some(crate::tui::home_view::tests::sample_limits(true));
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn limits_line_leaves_out_what_does_not_fit() {
        let limits = crate::tui::home_view::tests::sample_limits(true);
        let text = |room| -> String {
            limit_spans(&limits, dim(), room)
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert_eq!(text(60), "5 h 91 % · allowed_warning · resets 15:40");
        assert_eq!(text(20), "5 h 91 % · allowed_…");
        assert!(text(100).ends_with("7 d 49 % · resets Wed 12:40"));
    }

    #[test]
    fn log_tab_with_subagent_line() {
        let mut app = app();
        app.tab = Tab::Log;
        let call = |name: &str, detail: &str, subagent| {
            LogLine::Event(
                agent_event_line(&AgentEvent::ToolCall {
                    name: name.into(),
                    detail: detail.into(),
                    subagent,
                })
                .expect("tool line"),
            )
        };
        app.log = vec![
            LogLine::Attempt {
                number: 1,
                resumed: false,
            },
            LogLine::Event(
                agent_event_line(&AgentEvent::Message {
                    text: "Working on phase 2.".into(),
                    subagent: false,
                })
                .expect("message line"),
            ),
            call("Write", "src/parser.rs", false),
            call("Read", "src/plan.rs", true),
            call("Bash", "cargo test", false),
            LogLine::Attempt {
                number: 2,
                resumed: true,
            },
            LogLine::Event(
                agent_event_line(&AgentEvent::Retry {
                    detail: "rate limited".into(),
                })
                .expect("retry line"),
            ),
        ];
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn sanitize_replaces_control_characters() {
        assert_eq!(sanitize("+\t\tif x {"), "+        if x {");
        assert_eq!(sanitize("\x1b[31mred"), "\u{FFFD}[31mred");
        assert_eq!(sanitize("a\x7fb\rc"), "a\u{FFFD}b\u{FFFD}c");
        assert_eq!(sanitize("plain text {}"), "plain text {}");
        assert_eq!(sanitize("héllo ✓ 日本 😀"), "héllo ✓ 日本 😀");
    }

    #[test]
    fn diff_keeps_tab_indentation_and_hides_escapes() {
        let mut app = app();
        with_diff(&mut app);
        app.diff.text =
            "@@ -1,2 +1,3 @@\n fn main() {\n+\t\tif x {\n+\x1b[31mred\x1b[0m\n }\n".to_string();
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn verify_tab() {
        let mut app = app();
        app.tab = Tab::Verify;
        app.verify = "=== attempt 1 ===\n$ cargo test\nrunning 3 tests\nexit code: 0\n$ test -f a.md\nexit code: 1\n$ sleep 9\ntimed out after 1 s\n".to_string();
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn failed_phase_wraps_and_cuts_a_long_failure() {
        let mut app = app();
        app.selected = 3;
        let detail = (1..=40)
            .map(|i| format!("error line {i} with some text to fill the width of the pane"))
            .collect::<Vec<_>>()
            .join("\n");
        app.view.phases[3].failure_detail = Some(detail);
        let terminal = render(&mut app, 100, 30);
        let text = format!("{}", terminal.backend());
        assert!(text.contains("✗ Checks failed"), "{text}");
        assert!(
            !text.contains("error line 9 "),
            "cut at eight lines\n{text}"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn running_phase_shows_the_spinner_and_a_short_meta_line() {
        let mut app = app();
        app.view.phases[2].status = PhaseStatus::Running;
        app.selected = 2;
        app.running = Some(3);
        app.frame = 2;
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn scroll_position_shows_when_the_content_is_longer_than_the_pane() {
        let mut app = app();
        app.tab = Tab::Verify;
        app.verify = (1..=60).map(|i| format!("line {i}\n")).collect();
        app.scroll = 10;
        let text = format!("{}", render(&mut app, 100, 30).backend());
        assert!(text.contains(" of 60"), "{text}");
        assert!(text.contains("11–"), "{text}");
    }

    #[test]
    fn shortcuts_popup() {
        let mut app = app();
        app.mode = Mode::Help;
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn settings_popup() {
        let mut app = app();
        let mut settings = Settings::default();
        settings.set("limits", "off").expect("valid");
        settings.set("color", "never").expect("valid");
        app.mode = Mode::Settings(SettingsPanel::new(settings));
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn the_settings_popup_has_the_same_rectangle_on_both_screens() {
        use crate::tui::home_view::tests::popup_rect;
        let mut app = app();
        app.mode = Mode::Settings(SettingsPanel::new(Settings::default()));
        let run = popup_rect(&format!("{}", render(&mut app, 100, 30).backend()));
        let mut home = crate::tui::home_view::tests::home(crate::tui::data::PlanState::Missing);
        home.mode = crate::tui::home::Mode::Settings(SettingsPanel::new(Settings::default()));
        let home = popup_rect(&format!(
            "{}",
            crate::tui::home_view::tests::render_sized(&home, 100, 30).backend()
        ));
        assert_eq!(run, home);
    }

    #[test]
    fn long_titles_are_truncated_with_an_ellipsis() {
        let mut app = app();
        app.view.phases[0].title =
            "A very long phase title that cannot possibly fit in the list".into();
        insta::assert_snapshot!(render(&mut app, 80, 20).backend());
    }

    #[test]
    fn small_terminal_shows_only_the_message() {
        let mut app = app();
        insta::assert_snapshot!(render(&mut app, 60, 15).backend());
    }

    #[test]
    fn rollback_confirmation_popup() {
        let mut app = app();
        with_diff(&mut app);
        app.mode = Mode::ConfirmRollback(2, 0);
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn abandon_confirmation_popup() {
        let mut app = app();
        app.mode = Mode::ConfirmAbandon(1);
        let terminal = render(&mut app, 100, 30);
        let text = format!("{}", terminal.backend());
        assert!(text.contains("❯ 2. Cancel"), "{text}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn merge_confirmation_popup() {
        let mut app = app();
        app.view.completed = true;
        app.mode = Mode::ConfirmMerge(0);
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn note_input_popup() {
        let mut app = app();
        app.mode = Mode::Note("handle empty input".into());
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn message_and_footer_are_separate_rows_and_the_footer_drops_items() {
        let mut app = app();
        app.message = Some(Notice::ok("Phase 2 is ready for review."));
        let terminal = render(&mut app, 80, 24);
        let rows: Vec<String> = format!("{}", terminal.backend())
            .lines()
            .map(|l| l.trim_matches('"').trim_end().to_string())
            .collect();
        let n = rows.len();
        assert!(
            rows[n - 2].contains("✓ Phase 2 is ready for review."),
            "{rows:#?}"
        );
        assert!(rows[n - 1].ends_with("? shortcuts"), "{rows:#?}");
        assert!(!rows[n - 1].contains('✓'), "{rows:#?}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn a_diff_of_more_than_65535_lines_reaches_its_last_line() {
        let mut app = app();
        app.diff = DiffView {
            text: (1..=100_000).map(|i| format!("+line {i}\n")).collect(),
            ..DiffView::default()
        };
        app.scroll = usize::MAX;
        let text = format!("{}", render(&mut app, 100, 30).backend());
        assert!(text.contains("+line 100000"), "{text}");
        assert!(text.contains("of 100000"), "{text}");
        assert!(app.scroll > usize::from(u16::MAX), "{}", app.scroll);
        // A line in the middle past the old limit is reachable too.
        app.scroll = 70_000;
        let text = format!("{}", render(&mut app, 100, 30).backend());
        assert!(text.contains("+line 70001"), "{text}");
        assert!(text.contains("70001–"), "{text}");
    }

    #[test]
    fn scroll_is_clamped_to_the_content() {
        let mut app = app();
        with_diff(&mut app);
        app.scroll = 500;
        render(&mut app, 100, 30);
        assert!(app.scroll < 30);
        assert_eq!(app.page, 21);
    }

    #[test]
    fn the_list_is_at_least_24_columns_and_at_most_40_percent() {
        let app = app();
        assert_eq!(list_width(&app, 98), 30);
        let mut short = app.clone();
        for p in &mut short.view.phases {
            p.title = "x".into();
        }
        assert_eq!(list_width(&short, 98), 24);
        let mut long = app.clone();
        long.view.phases[0].title = "x".repeat(200);
        assert_eq!(list_width(&long, 98), 39);
    }

    fn row_of(terminal: &Terminal<TestBackend>, needle: &str) -> Option<String> {
        format!("{}", terminal.backend())
            .lines()
            .find(|l| l.contains(needle))
            .map(str::to_string)
    }

    #[test]
    fn horizontal_scroll_cuts_columns_and_stops_at_the_widest_line() {
        let mut app = app();
        app.diff = DiffView {
            text: format!("+{}END", "a".repeat(150)),
            ..DiffView::default()
        };
        let before = render(&mut app, 100, 30);
        assert!(row_of(&before, "+aaaa").is_some());
        assert!(row_of(&before, "END").is_none());
        app.hscroll = 8;
        let row = row_of(&render(&mut app, 100, 30), "aaaa").expect("row");
        assert!(!row.contains('+'), "{row}");
        app.hscroll = 10_000;
        let row = row_of(&render(&mut app, 100, 30), "END").expect("the end of the line");
        assert!(row.contains("aaaEND") || row.contains("END"), "{row}");
        // 151 columns + `+` + `END` = 154 wide in a pane of 66.
        assert_eq!(app.hscroll, 154 - 66, "clamped to the widest line");
    }

    #[test]
    fn horizontal_scroll_keeps_wide_characters_whole() {
        let mut app = app();
        app.diff = DiffView {
            text: format!(" {}", "漢字".repeat(40)),
            ..DiffView::default()
        };
        // Columns: ` ` 0, `漢` 1-2, `字` 3-4: skipping 3 starts at `字`.
        app.hscroll = 3;
        let row = row_of(&render(&mut app, 100, 30), "字").expect("row");
        assert!(row.contains("│ 字漢字"), "{row}");
        let line = cut_columns(Line::raw("a漢b"), 2);
        assert_eq!(line.spans[0].content, " b");
        let line = cut_columns(Line::raw("a漢b"), 3);
        assert_eq!(line.spans[0].content, "b");
    }

    #[test]
    fn search_input_popup() {
        let mut app = app();
        with_diff(&mut app);
        app.mode = Mode::Search("new".into());
        insta::assert_snapshot!(render(&mut app, 100, 30).backend());
    }

    #[test]
    fn the_shortcuts_popup_scrolls_on_a_short_screen() {
        let mut app = app();
        app.mode = Mode::Help;
        let top = format!("{}", render(&mut app, 80, 24).backend());
        assert!(top.contains("↓ more"), "{top}");
        assert!(!top.contains("go to the next or previous match"), "{top}");
        app.help_scroll = usize::MAX;
        let bottom = format!("{}", render(&mut app, 80, 24).backend());
        assert!(bottom.contains("↑ more"), "{bottom}");
        assert!(
            bottom.contains("go to the next or previous match"),
            "{bottom}"
        );
        assert!(bottom.contains("Shortcuts"), "{bottom}");
    }

    #[test]
    fn the_shortcuts_popup_lists_the_right_panel_keys() {
        let mut app = app();
        app.mode = Mode::Help;
        let rows: Vec<String> = format!("{}", render(&mut app, 80, 30).backend())
            .lines()
            .map(str::to_string)
            .collect();
        assert!(
            rows.iter()
                .any(|r| r.contains("go to the next or previous match"))
        );
    }
}
