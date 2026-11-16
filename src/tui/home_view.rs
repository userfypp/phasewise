//! Rendering of the home screen. Deterministic: the output depends only on
//! the [`Home`]. Top-aligned: the header card, the content, a blank line and
//! the footer, one after the other; nothing stretches to the bottom.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::data::{HomePhase, PlanState, PlanSummary, RunSummary};
use super::home::{Home, Mode, ModelItem, ModelsState, Probe};
use super::shared::spinner;
use super::widgets::{
    MIN_HEIGHT, MIN_WIDTH, PickerRow, accent, bold, confirm_popup, dim, draw_too_small,
    duration_text, elapsed_since, error_popup, event_text, footer_line, input_popup, limit_spans,
    margin, notice_line, pad_to, picker_lines, render_card, render_popup, role_style,
    settings_popup, shortcuts_popup, text_width, truncate, wrap_text,
};
use crate::display::{model_name, phase_count, status_label};
use crate::plan::AgentKind;
use crate::state::{PhaseStatus, RunStatus};
use crate::style::Role;

/// The header card is at least this wide.
const CARD_MIN_WIDTH: u16 = 44;
/// Problems of an invalid plan shown before `… N more`.
const MAX_PROBLEMS: usize = 6;
/// The title column of the phase list is at most this wide, so that the
/// metadata stays close to the titles.
const TITLE_COLUMN_MAX: usize = 40;
/// Longest line of prose (descriptions, hints) on the screen.
const PROSE_WIDTH: usize = 72;

pub fn draw(frame: &mut Frame, home: &Home) {
    draw_with_version(frame, home, env!("CARGO_PKG_VERSION"));
}

/// Like [`draw`], with the version shown in the header card (tests pin it).
pub(super) fn draw_with_version(frame: &mut Frame, home: &Home, version: &str) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area);
        return;
    }
    let area = margin(area);
    let width = usize::from(area.width);
    let card_height = draw_header(frame, home, area, version);
    // The content follows the card after one blank line; the footer follows
    // the content after another.
    let fixed = usize::from(card_height) + 1 + 1 + usize::from(home.message.is_some()) + 1;
    let room = usize::from(frame.area().height).saturating_sub(fixed);
    // Cut the list (phases or errors) until the content fits; whatever is
    // still too tall is cut at the end. The footer never overlaps anything.
    let mut limit = list_len(home);
    let mut doc = document(home, width, limit);
    while doc.len() > room && limit > 1 {
        limit -= 1;
        doc = document(home, width, limit);
    }
    doc.truncate(room);
    let top = area.y + card_height + 1;
    let doc_height = u16::try_from(doc.len()).unwrap_or(u16::MAX);
    let bottom = frame.area().bottom();
    frame.render_widget(
        Paragraph::new(doc),
        Rect::new(
            area.x,
            top,
            area.width,
            doc_height.min(bottom.saturating_sub(top)),
        ),
    );
    let mut y = top.saturating_add(doc_height).saturating_add(1);
    if let Some(notice) = &home.message {
        frame.render_widget(
            Paragraph::new(notice_line(notice)),
            Rect::new(area.x, y.min(bottom - 1), area.width, 1),
        );
        y = y.saturating_add(1);
    }
    let keys = home.footer();
    frame.render_widget(
        Paragraph::new(footer_line(&keys, home.mode == Mode::Normal, width)),
        Rect::new(area.x, y.min(bottom - 1), area.width, 1),
    );
    draw_popup(frame, home, frame.area());
}

/// Rows of the list that can be cut: the phases, or the errors shown.
fn list_len(home: &Home) -> usize {
    match &home.data.plan {
        PlanState::Valid(plan) => plan.phases.len(),
        PlanState::Invalid(problems) => problems.len().min(MAX_PROBLEMS),
        PlanState::Missing => 0,
    }
}

/// `✻ phasewise 0.3.0` and `repo · branch` in a rounded card.
fn draw_header(frame: &mut Frame, home: &Home, area: Rect, version: &str) -> u16 {
    let lines = vec![
        Line::from(vec![
            Span::styled("✻", accent()),
            Span::styled(" phasewise", bold()),
            Span::styled(format!(" {version}"), dim()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{} · {}", home.data.repo_name, home.data.branch),
                dim(),
            ),
        ]),
    ];
    render_card(frame, area, lines, CARD_MIN_WIDTH)
}

fn agent_title(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "Claude Code",
        AgentKind::Codex => "Codex",
    }
}

/// The content below the card, as lines; sections are separated by a blank
/// line. At most `phase_limit` phases of a valid plan are listed.
fn document(home: &Home, width: usize, phase_limit: usize) -> Vec<Line<'static>> {
    let mut sections: Vec<Vec<Line<'static>>> = Vec::new();
    if let Some(run) = home.data.run.as_ref().filter(|r| !r.active()) {
        let how = match run.status {
            RunStatus::Completed => "completed",
            _ => "abandoned",
        };
        sections.push(vec![Line::styled(
            format!("Last run {} · {how} · o to open", run.id),
            dim(),
        )]);
    }
    if let Some(run) = home.active_run() {
        sections.push(run_card(home, run, width));
    } else {
        sections.push(plan_section(home, width, phase_limit));
        sections.push(settings_section(home, width));
        sections.push(notices(home));
    }
    sections.push(activity(home, width));
    let mut lines = Vec::new();
    for section in sections.into_iter().filter(|s| !s.is_empty()) {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.extend(section);
    }
    lines
}

/// The active run: its branch and each phase with its state.
fn run_card(home: &Home, run: &RunSummary, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("Run in progress", bold()),
        Span::styled(format!(" · {}", run.run_branch), dim()),
    ])];
    let title_width = run
        .phases
        .iter()
        .map(|p| text_width(&p.title))
        .max()
        .unwrap_or(0)
        .min(width.saturating_sub(30).max(8));
    for phase in &run.phases {
        let (mark, style) = match phase.status {
            PhaseStatus::Pending => ("·", dim()),
            PhaseStatus::Running | PhaseStatus::Verifying => (spinner(home.frame), accent()),
            PhaseStatus::AwaitingReview => ("◆", role_style(Role::Warn)),
            PhaseStatus::Done => ("✓", role_style(Role::Ok)),
            PhaseStatus::Failed => ("✗", role_style(Role::Fail)),
        };
        let state = match (phase.status, phase.started_at) {
            (PhaseStatus::Pending | PhaseStatus::Done, _) => String::new(),
            (PhaseStatus::Running | PhaseStatus::Verifying, Some(at)) => {
                format!("{} {}", status_label(phase.status), elapsed_since(at))
            }
            (status, _) => status_label(status).to_string(),
        };
        let title = truncate(&phase.title, title_width);
        let mut spans = vec![
            Span::styled(mark, style),
            Span::raw(format!(
                " {:>2}  {}",
                phase.number,
                pad_to(&title, title_width)
            )),
        ];
        let state = match phase.agent {
            Some(agent) if state.is_empty() => agent.to_string(),
            Some(agent) => format!("{state} · {agent}"),
            None => state,
        };
        if !state.is_empty() {
            spans.push(Span::styled(format!("   {state}"), dim()));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The plan: valid (title, file, phases), missing or invalid.
fn plan_section(home: &Home, width: usize, phase_limit: usize) -> Vec<Line<'static>> {
    let file = home.plan_name();
    match &home.data.plan {
        PlanState::Missing => {
            let mut lines = vec![Line::styled("No plan yet", bold())];
            lines.extend(
                wrap_text(
                    "Let the agent write one from a sentence, or run phasewise init.",
                    width.min(PROSE_WIDTH),
                )
                .into_iter()
                .map(|l| Line::styled(l, dim())),
            );
            lines
        }
        PlanState::Valid(plan) => {
            let mut lines = vec![
                Line::styled(plan.title.clone().unwrap_or_else(|| file.clone()), bold()),
                Line::styled(
                    format!("{file} · {}", phase_count(plan.phases.len())),
                    dim(),
                ),
            ];
            lines.extend(phase_rows(home, plan, width, phase_limit));
            lines
        }
        PlanState::Invalid(problems) => {
            let mut lines = vec![Line::styled(
                format!("✗ {file} has errors"),
                role_style(Role::Fail),
            )];
            let shown = problems.len().min(MAX_PROBLEMS).min(phase_limit);
            let digits = problems
                .iter()
                .take(shown)
                .map(|p| p.line.to_string().len())
                .max()
                .unwrap_or(1);
            for problem in problems.iter().take(shown) {
                let mut spans = vec![Span::raw("  ")];
                if problem.line > 0 {
                    spans.push(Span::styled(
                        format!("line {:>digits$}  ", problem.line),
                        dim(),
                    ));
                }
                let room = width.saturating_sub(2 + 10);
                spans.push(Span::raw(truncate(
                    problem.message.lines().next().unwrap_or(""),
                    room,
                )));
                lines.push(Line::from(spans));
            }
            if problems.len() > shown {
                lines.push(Line::styled(
                    format!("  … {} more", problems.len() - shown),
                    dim(),
                ));
            }
            lines.push(Line::styled(
                "Fix them in your editor; this screen updates on its own.",
                dim(),
            ));
            lines
        }
    }
}

/// `  1  Title       2 checks · sonnet`: the title column is as wide as the
/// longest title (cut so that the metadata fits), the metadata follows it.
fn phase_rows(
    home: &Home,
    plan: &PlanSummary,
    width: usize,
    phase_limit: usize,
) -> Vec<Line<'static>> {
    let metas: Vec<String> = plan
        .phases
        .iter()
        .map(|p| phase_meta(home, plan, p))
        .collect();
    let meta_width = metas.iter().map(|m| text_width(m)).max().unwrap_or(0);
    let prefix = 2 + 2 + 2;
    let longest = plan
        .phases
        .iter()
        .map(|p| text_width(&p.title))
        .max()
        .unwrap_or(0);
    let title_width = longest
        .min(TITLE_COLUMN_MAX)
        .min(width.saturating_sub(prefix + 3 + meta_width))
        .max(8);
    let shown = plan.phases.len().min(phase_limit);
    let mut lines: Vec<Line<'static>> = plan
        .phases
        .iter()
        .zip(&metas)
        .take(shown)
        .map(|(phase, meta)| {
            let title = truncate(&phase.title, title_width);
            Line::from(vec![
                Span::raw(format!(
                    "  {:>2}  {}   ",
                    phase.number,
                    pad_to(&title, title_width)
                )),
                Span::styled(meta.clone(), dim()),
            ])
        })
        .collect();
    if shown < plan.phases.len() {
        lines.push(Line::styled(
            format!("  … {} more", plan.phases.len() - shown),
            dim(),
        ));
    }
    lines
}

/// `2 checks · sonnet`: the checks, and the phase's own model and effort
/// when the plan's agent is the selected one (a model name belongs to one).
fn phase_meta(home: &Home, plan: &PlanSummary, phase: &HomePhase) -> String {
    let checks = match phase.checks {
        0 => "no checks".to_string(),
        1 => "1 check".to_string(),
        n => format!("{n} checks"),
    };
    let mut parts = vec![checks];
    if home.agent == plan.agent {
        let agent = phase.agent.unwrap_or(plan.agent);
        parts.extend(phase.agent.map(|a| a.to_string()));
        parts.extend(phase.model.as_deref().map(|m| model_name(agent, m)));
        parts.extend(phase.effort.clone());
    }
    parts.join(" · ")
}

/// The version number out of what `--version` printed.
fn version_number(text: &str) -> &str {
    text.split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(text)
}

/// `Agent`, `Model` and `Effort`, aligned; then `Limits` when the selected
/// agent is Claude Code and it has reported some.
fn settings_section(home: &Home, width: usize) -> Vec<Line<'static>> {
    let label = |text: &str| Span::styled(format!("{text:<8}"), dim());
    let mut agent = vec![label("Agent"), Span::raw(agent_title(home.agent))];
    match home.probe(home.agent) {
        Probe::Checking => agent.push(Span::styled(" checking…", dim())),
        Probe::Version(v) => agent.push(Span::styled(format!(" {}", version_number(v)), dim())),
        Probe::NotInstalled => agent.push(Span::styled(" not installed", role_style(Role::Fail))),
    }
    let (plan_model, plan_effort) = plan_values(home);
    let model = home.model.as_deref().map(|m| model_name(home.agent, m));
    let plan_model = plan_model.map(|m| model_name(home.agent, m));
    let mut lines = vec![
        Line::from(agent),
        setting("Model", model, plan_model, home.agent),
        setting(
            "Effort",
            home.effort.clone(),
            plan_effort.map(str::to_string),
            home.agent,
        ),
    ];
    if home.agent == AgentKind::Claude
        && home.settings.limits()
        && let Some(limits) = home.data.limits.as_ref().filter(|l| l.agent == home.agent)
    {
        let mut spans = vec![label("Limits")];
        spans.extend(limit_spans(limits, Style::new(), width.saturating_sub(8)));
        lines.push(Line::from(spans));
    }
    lines
}

/// The plan's model and effort, when the plan is for the selected agent.
fn plan_values(home: &Home) -> (Option<&str>, Option<&str>) {
    match &home.data.plan {
        PlanState::Valid(plan) if plan.agent == home.agent => {
            (plan.model.as_deref(), plan.effort.as_deref())
        }
        _ => (None, None),
    }
}

/// A chosen value, else the plan's (` · from the plan`), else `Default`.
fn setting(
    name: &str,
    chosen: Option<String>,
    from_plan: Option<String>,
    agent: AgentKind,
) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{name:<8}"), dim())];
    match (chosen, from_plan) {
        (Some(value), _) => spans.push(Span::raw(value)),
        (None, Some(value)) => {
            spans.push(Span::raw(value));
            spans.push(Span::styled(" · from the plan", dim()));
        }
        (None, None) => {
            spans.push(Span::raw("Default"));
            spans.push(Span::styled(
                format!(" · {}'s choice", agent_title(agent)),
                dim(),
            ));
        }
    }
    Line::from(spans)
}

/// Yellow `!` notices; continuation lines are indented.
fn notices(home: &Home) -> Vec<Line<'static>> {
    let warn = role_style(Role::Warn);
    let mut lines = Vec::new();
    let mut push = |first: String, rest: Vec<String>| {
        lines.push(Line::from(vec![
            Span::styled("!", warn),
            Span::raw(format!(" {first}")),
        ]));
        lines.extend(rest.into_iter().map(|l| Line::raw(format!("  {l}"))));
    };
    if home.data.plan_uncommitted {
        push(
            format!("{} is not committed", home.plan_name()),
            vec![home.commit_hint()],
        );
    }
    if let Some(id) = home.run_branch_id() {
        push(
            format!("You are on the branch of run {id}; a new run would start from it."),
            vec![format!(
                "Go back with: git switch {}",
                home.original_branch()
            )],
        );
    }
    lines
}

/// The work in progress: planning (with its latest event) or the commit.
fn activity(home: &Home, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(planning) = &home.planning {
        let elapsed = home
            .planning_started
            .map(|at| duration_text(i64::try_from(at.elapsed().as_secs()).unwrap_or(0)))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(spinner(home.frame), accent()),
            Span::raw(format!(
                " Writing the plan with {}…",
                agent_title(home.agent)
            )),
            Span::styled(format!(" {elapsed}"), dim()),
        ]));
        if let Some(last) = planning.lines.last() {
            let text = truncate(&event_text(last), width.saturating_sub(4));
            lines.push(Line::styled(format!("  ⎿ {text}"), dim()));
        }
    }
    if let Some(started) = home.committing {
        lines.push(Line::from(vec![
            Span::styled(spinner(home.frame), accent()),
            Span::raw(format!(" Committing {}…", home.plan_name())),
            Span::styled(format!(" {}s", started.elapsed().as_secs()), dim()),
        ]));
    }
    lines
}

fn draw_popup(frame: &mut Frame, home: &Home, area: Rect) {
    let plan = home.plan_name();
    let agent = agent_title(home.agent);
    let popup = match &home.mode {
        Mode::Normal => return,
        Mode::Help => shortcuts_popup(&Home::shortcuts()).scroll(home.help_scroll),
        Mode::Settings(panel) => settings_popup(panel, area.width),
        Mode::ConfirmQuit(row) => confirm_popup(
            "Quit phasewise?",
            if home.planning.is_some() {
                "The plan being written is discarded."
            } else {
                "The running phase is interrupted."
            },
            "Quit",
            "Keep working",
            *row,
        ),
        Mode::ConfirmStopPlan(row) => confirm_popup(
            "Stop planning?",
            "The plan being written is discarded.",
            "Stop planning",
            "Keep working",
            *row,
        ),
        Mode::ConfirmCommitStart(row) => confirm_popup(
            &format!("{plan} is not committed"),
            "A run needs a clean working tree. Commit the plan and start the run?",
            "Commit and start",
            "Cancel",
            *row,
        ),
        Mode::ConfirmReplace(_, row) => confirm_popup(
            &format!("Replace {plan}?"),
            "The agent writes a new plan in its place. Your current file is overwritten.",
            "Replace it",
            "Cancel",
            *row,
        ),
        Mode::GoalInput(buffer) => input_popup(
            "New plan",
            &format!(
                "Describe the goal in one sentence. {agent} reads the project, changes nothing, and writes {plan}."
            ),
            buffer,
            area.width,
        ),
        Mode::ModelInput(buffer) => {
            let flag = match home.agent {
                AgentKind::Claude => "claude --model",
                AgentKind::Codex => "codex -m",
            };
            input_popup(
                "Other model",
                &format!("Any name {flag} accepts."),
                buffer,
                area.width,
            )
        }
        Mode::ModelPopup(selected) => model_popup(home, *selected),
        Mode::EffortPopup(selected) => effort_popup(home, *selected),
        Mode::Error(lines) => error_popup(lines, area.width),
    };
    render_popup(frame, area, &popup);
}

/// What `Default` means: the plan's value when it sets one, else the agent's.
fn default_description(home: &Home, from_plan: Option<&str>) -> String {
    match from_plan {
        Some(value) => format!("From the plan: {value}"),
        None => format!("{}'s own choice", agent_title(home.agent)),
    }
}

fn model_popup(home: &Home, selected: usize) -> super::widgets::Popup {
    let (plan_model, _) = plan_values(home);
    let plan_model = plan_model.map(|m| model_name(home.agent, m));
    let items = home.model_items();
    let known = |id: &str| {
        items
            .iter()
            .any(|i| matches!(i, ModelItem::Model(known, _) if known == id))
    };
    let mut rows = Vec::new();
    for item in &items {
        match item {
            ModelItem::Default => {
                rows.push(PickerRow::item(
                    "Default",
                    &default_description(home, plan_model.as_deref()),
                    home.model.is_none(),
                ));
                match home.models(home.agent) {
                    ModelsState::Loading => rows.push(PickerRow::Note(Line::from(vec![
                        Span::styled(spinner(home.frame), accent()),
                        Span::styled(
                            format!(" Loading {} models…", agent_title(home.agent)),
                            dim(),
                        ),
                    ]))),
                    ModelsState::Failed(error) => rows.push(PickerRow::Note(Line::styled(
                        format!("Could not list the models: {}", truncate(error, 40)),
                        dim(),
                    ))),
                    ModelsState::NotLoaded | ModelsState::Ready(_) => {}
                }
            }
            ModelItem::Model(id, label) => {
                let description = match home.models(home.agent) {
                    ModelsState::Ready(models) => models
                        .iter()
                        .find(|m| &m.id == id)
                        .map(|m| m.description.clone())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                rows.push(PickerRow::item(
                    label,
                    &description,
                    home.model.as_deref() == Some(id.as_str()),
                ));
            }
            ModelItem::Custom => rows.push(PickerRow::item(
                "Other…",
                "Type any model name",
                home.model.as_deref().is_some_and(|m| !known(m)),
            )),
        }
    }
    super::widgets::Popup::new("Model").body(picker_lines(&rows, selected))
}

fn effort_popup(home: &Home, selected: usize) -> super::widgets::Popup {
    let (_, plan_effort) = plan_values(home);
    let mut rows = vec![PickerRow::item(
        "Default",
        &default_description(home, plan_effort),
        home.effort.is_none(),
    )];
    for level in Home::effort_items().into_iter().flatten() {
        rows.push(PickerRow::item(
            level,
            "",
            home.effort.as_deref() == Some(level),
        ));
    }
    super::widgets::Popup::new("Effort").body(picker_lines(&rows, selected))
}

#[cfg(test)]
pub(super) mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::agent::ModelInfo;
    use crate::display::plan_event_line;
    use crate::ops::PlanEvent;
    use crate::settings::Settings;
    use crate::state::{PhaseStatus, RunStatus};
    use crate::tui::data::{HomeData, HomePhase, PlanProblem, RunPhaseSummary};
    use crate::tui::shared::{Notice, SettingsPanel};

    pub(in crate::tui) fn data(plan: PlanState) -> HomeData {
        HomeData {
            root: PathBuf::from("/work/demo"),
            repo_name: "demo".into(),
            branch: "main".into(),
            plan_path: PathBuf::from("/work/demo/phasewise.md"),
            plan,
            plan_uncommitted: false,
            plan_rel: "phasewise.md".into(),
            run: None,
            limits: None,
        }
    }

    pub(in crate::tui) fn valid_plan() -> PlanState {
        let phase = |number, title: &str, checks, model: Option<&str>| HomePhase {
            agent: None,
            number,
            title: title.into(),
            checks,
            model: model.map(str::to_string),
            effort: None,
        };
        PlanState::Valid(PlanSummary {
            title: Some("Demo plan".into()),
            agent: AgentKind::Claude,
            model: Some("sonnet".into()),
            effort: None,
            phases: vec![
                phase(1, "Scaffold the project", 2, Some("sonnet")),
                phase(2, "Add the parser", 1, Some("opus")),
                phase(
                    3,
                    "Write the docs and the long release notes for everybody",
                    0,
                    Some("sonnet"),
                ),
            ],
        })
    }

    /// Limits seen a minute before the fixed test clock (a Saturday, 12:40 UTC).
    pub(in crate::tui) fn sample_limits(warning: bool) -> crate::state::RateLimits {
        const NOW: i64 = 1_794_660_000;
        let window =
            |name: &str, utilization: f64, resets_at: i64, status: &str| crate::agent::RateWindow {
                name: name.into(),
                utilization: Some(utilization),
                resets_at: Some(resets_at),
                status: status.into(),
            };
        crate::state::RateLimits {
            agent: AgentKind::Claude,
            seen_at: NOW - 60,
            windows: vec![
                if warning {
                    window("five_hour", 0.91, NOW + 3 * 3600, "allowed_warning")
                } else {
                    window("five_hour", 0.36, NOW + 3 * 3600, "allowed")
                },
                window("seven_day", 0.49, NOW + 4 * 86400, "allowed"),
            ],
        }
    }

    pub(in crate::tui) fn home(plan: PlanState) -> Home {
        let mut home = Home::new(data(plan));
        home.set_probe(
            AgentKind::Claude,
            Probe::Version("2.1.325 (Claude Code)".into()),
        );
        home.set_probe(AgentKind::Codex, Probe::NotInstalled);
        home
    }

    pub(in crate::tui) fn render_sized(
        home: &Home,
        width: u16,
        height: u16,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|f| draw_with_version(f, home, "0.3.0"))
            .expect("draw");
        terminal
    }

    fn render(home: &Home) -> Terminal<TestBackend> {
        render_sized(home, 100, 24)
    }

    fn press(home: &mut Home, c: char) {
        home.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    pub(in crate::tui) fn model(id: &str, label: &str, description: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: label.into(),
            description: description.into(),
            efforts: Vec::new(),
        }
    }

    pub(in crate::tui) fn catalog() -> Vec<ModelInfo> {
        vec![
            model(
                "claude-fable-5-2",
                "Fable 5.2",
                "Most capable, for the hardest work",
            ),
            model("claude-opus-5-5", "Opus 5.5", "Complex, multi-step work"),
            model(
                "claude-sonnet-5-5",
                "Sonnet 5.5",
                "Fast and capable, for most coding",
            ),
            model(
                "claude-haiku-5-5",
                "Haiku 5.5",
                "Fastest and cheapest, for simple phases",
            ),
        ]
    }

    fn run_summary(status: RunStatus) -> RunSummary {
        let phase = |number, title: &str, status| RunPhaseSummary {
            agent: None,
            number,
            title: title.into(),
            status,
            started_at: None,
        };
        RunSummary {
            id: "20260101-120000".into(),
            status,
            run_branch: "phasewise/20260101-120000".into(),
            original_branch: "main".into(),
            phases: vec![
                phase(1, "Scaffold the project", PhaseStatus::Done),
                phase(2, "Add the parser", PhaseStatus::Running),
                phase(3, "Write the docs", PhaseStatus::Pending),
            ],
        }
    }

    #[test]
    fn missing_plan() {
        insta::assert_snapshot!(render(&home(PlanState::Missing)).backend());
    }

    #[test]
    fn valid_plan_uncommitted() {
        let mut home = home(valid_plan());
        home.data.plan_uncommitted = true;
        home.effort = Some("high".into());
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn valid_plan_with_limits() {
        let mut home = home(valid_plan());
        home.data.limits = Some(sample_limits(false));
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn limits_are_hidden_for_another_agent() {
        let mut home = home(valid_plan());
        home.data.limits = Some(sample_limits(false));
        press(&mut home, 'a');
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn valid_plan_for_another_agent_hides_the_plan_models() {
        let mut home = home(valid_plan());
        press(&mut home, 'a');
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn invalid_plan() {
        let problems = vec![
            PlanProblem {
                line: 3,
                message: "unknown key `modle` in the front matter".into(),
            },
            PlanProblem {
                line: 12,
                message: "phase 2 has no instructions".into(),
            },
        ];
        insta::assert_snapshot!(render(&home(PlanState::Invalid(problems))).backend());
    }

    #[test]
    fn many_problems_are_cut_with_a_count() {
        let problems = (1..=9)
            .map(|line| PlanProblem {
                line,
                message: format!("problem {line}"),
            })
            .collect();
        let text = format!("{}", render(&home(PlanState::Invalid(problems))).backend());
        assert!(
            text.contains("problem 6") && !text.contains("problem 7"),
            "{text}"
        );
        assert!(text.contains("… 3 more"), "{text}");
    }

    #[test]
    fn model_popup() {
        let mut home = home(valid_plan());
        home.model = Some("claude-opus-5-5".into());
        press(&mut home, 'm');
        home.set_models(AgentKind::Claude, Ok(catalog()));
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn model_popup_while_loading_and_after_an_error() {
        let mut home = home(valid_plan());
        press(&mut home, 'm');
        let loading = render(&home);
        home.set_models(AgentKind::Claude, Err("`claude` was not found.".into()));
        let failed = render(&home);
        insta::assert_snapshot!("model_popup_loading", loading.backend());
        insta::assert_snapshot!("model_popup_failed", failed.backend());
    }

    #[test]
    fn effort_popup() {
        let mut home = home(valid_plan());
        home.effort = Some("medium".into());
        home.mode = Mode::EffortPopup(2);
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn plan_input() {
        let mut home = home(PlanState::Missing);
        home.mode = Mode::GoalInput("a CLI to convert CSV files to JSON".into());
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn replace_confirmation() {
        let mut home = home(valid_plan());
        home.mode = Mode::ConfirmReplace("goal".into(), 1);
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn commit_and_start_confirmation() {
        let mut home = home(valid_plan());
        home.data.plan_uncommitted = true;
        home.mode = Mode::ConfirmCommitStart(0);
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn creation_in_progress() {
        let mut home = home(PlanState::Missing);
        home.begin_planning();
        let mut at = 0;
        let mut push = |home: &mut Home, event: PlanEvent| {
            let header =
                crate::display::plan_header(&event, &crate::style::Styler { color: false });
            let line = plan_event_line(&event).map(|l| l.at(at));
            at += 4;
            home.plan_event(header, line);
        };
        push(
            &mut home,
            PlanEvent::Started {
                plan_id: "p".into(),
                agent: AgentKind::Claude,
                model: Some("opus".into()),
                effort: None,
            },
        );
        push(
            &mut home,
            PlanEvent::Agent {
                event: crate::agent::AgentEvent::ToolCall {
                    name: "Read".into(),
                    detail: "Cargo.toml".into(),
                    subagent: false,
                },
            },
        );
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn creation_shows_only_the_latest_event() {
        let mut home = home(valid_plan());
        home.begin_planning();
        for i in 0..40u64 {
            let event = PlanEvent::Agent {
                event: crate::agent::AgentEvent::ToolCall {
                    name: "Read".into(),
                    detail: format!("file-{i}.rs"),
                    subagent: false,
                },
            };
            home.plan_event(None, plan_event_line(&event).map(|l| l.at(i)));
        }
        let text = format!("{}", render(&home).backend());
        assert!(
            text.contains("⎿ Read") && text.contains("file-39.rs"),
            "{text}"
        );
        assert!(!text.contains("file-38.rs"), "{text}");
        assert!(
            text.contains("Writing the plan with Claude Code…"),
            "{text}"
        );
    }

    #[test]
    fn error_popup() {
        let mut home = home(valid_plan());
        home.data.plan_uncommitted = true;
        home.show_start_error(&anyhow::anyhow!(
            "The working tree has uncommitted changes:\n  phasewise.md\nCommit or stash them, then run phasewise again."
        ));
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn commit_error_popup() {
        let mut home = home(valid_plan());
        home.commit_finished(&Err("pre-commit hook failed".into()));
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn shortcuts_popup() {
        let mut home = home(valid_plan());
        home.mode = Mode::Help;
        insta::assert_snapshot!(render(&home).backend());
    }

    /// Home screen with the settings popup open on `selected`, after `change`.
    fn settings_home(change: impl FnOnce(&mut SettingsPanel)) -> Home {
        let mut home = home(valid_plan());
        let mut panel = SettingsPanel::new(Settings::default());
        change(&mut panel);
        home.mode = Mode::Settings(panel);
        home
    }

    /// Row, left column, right column and bottom row of the settings popup's
    /// frame (found by its title).
    pub(in crate::tui) fn popup_rect(screen: &str) -> (usize, usize, usize, usize) {
        let col = |line: &str, c: char| line.chars().position(|x| x == c);
        let title = screen
            .lines()
            .position(|l| l.contains("│ Settings"))
            .expect("title");
        let top = title - 1;
        let bottom = title
            + screen
                .lines()
                .skip(title)
                .position(|l| l.contains('╰'))
                .expect("bottom");
        let line = screen.lines().nth(top).expect("line");
        (
            top,
            col(line, '╭').expect("left"),
            col(line, '╮').expect("right"),
            bottom,
        )
    }

    #[test]
    fn settings_popup_default_state() {
        let home = settings_home(|_| {});
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn settings_popup_with_a_toggled_bool() {
        let home = settings_home(|panel| {
            panel.settings.cycle("bell");
            panel.selected = 1;
        });
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn settings_popup_with_an_enum_at_its_longest_value() {
        let home = settings_home(|panel| {
            panel.settings.set("effort", "medium").expect("valid");
            panel.settings.set("agent", "claude").expect("valid");
            panel.selected = 5;
        });
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn settings_popup_editing_a_long_model_with_an_error() {
        let home = settings_home(|panel| {
            panel.selected = 4;
            panel.editing = Some("a-very-long-model-name-that-does-not-fit-in-the-column".into());
            panel.error = Some("Invalid value for `model`: it has a space.".into());
        });
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn the_settings_popup_never_changes_its_rectangle() {
        let states: Vec<Home> = vec![
            settings_home(|_| {}),
            settings_home(|p| {
                p.settings.cycle("bell");
                p.selected = 1;
            }),
            settings_home(|p| {
                p.settings.set("effort", "medium").expect("valid");
                p.selected = 5;
            }),
            settings_home(|p| {
                p.settings.set("model", "opus").expect("valid");
                p.selected = 4;
                p.editing = Some("a-very-long-model-name-that-does-not-fit-in-the-column".into());
                p.error = Some("Invalid value for `model`: it has a space. Try again.".into());
            }),
            settings_home(|p| p.selected = 3),
        ];
        let rects: Vec<_> = states
            .iter()
            .map(|h| popup_rect(&format!("{}", render_sized(h, 100, 30).backend())))
            .collect();
        assert!(rects.windows(2).all(|w| w[0] == w[1]), "{rects:?}");
    }

    #[test]
    fn limits_are_hidden_when_the_setting_is_off() {
        let mut home = home(valid_plan());
        home.data.limits = Some(sample_limits(false));
        home.settings.set("limits", "off").expect("valid");
        let text = format!("{}", render(&home).backend());
        assert!(!text.contains("Limits"), "{text}");
    }

    #[test]
    fn active_run_card() {
        let mut home = home(valid_plan());
        home.data.run = Some(run_summary(RunStatus::Active));
        home.run_busy = true;
        home.frame = 3;
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn last_run_line_for_a_finished_run() {
        let mut home = home(valid_plan());
        home.data.run = Some(run_summary(RunStatus::Aborted));
        let text = format!("{}", render(&home).backend());
        assert!(
            text.contains("Last run 20260101-120000 · abandoned · o to open"),
            "{text}"
        );
    }

    #[test]
    fn committing_shows_the_activity_line() {
        let mut home = home(valid_plan());
        home.data.plan_uncommitted = true;
        home.begin_commit();
        home.frame = 4;
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn run_branch_notice() {
        let mut home = home(valid_plan());
        home.data.branch = "phasewise/20260101-120000".into();
        home.data.run = Some(run_summary(RunStatus::Aborted));
        insta::assert_snapshot!(render(&home).backend());
    }

    #[test]
    fn commit_command_is_relative_to_the_directory() {
        let mut home = home(valid_plan());
        home.data.plan_uncommitted = true;
        home.data.plan_rel = "../phasewise.md".into();
        let text = format!("{}", render(&home).backend());
        assert!(
            text.contains("git add ../phasewise.md && git commit -m \"Add phasewise plan\""),
            "{text}"
        );
    }

    #[test]
    fn footer_drops_the_lowest_priority_items_on_80_columns() {
        let mut home = home(valid_plan());
        home.data.plan_uncommitted = true;
        home.message = Some(Notice::ok("Committed phasewise.md (abc1234)"));
        insta::assert_snapshot!(render_sized(&home, 80, 24).backend());
    }

    #[test]
    fn notice_levels_show_their_glyph() {
        let mut home = home(valid_plan());
        home.message = Some(Notice::error("boom"));
        let text = format!("{}", render(&home).backend());
        assert!(text.contains("✗ boom"), "{text}");
        home.message = Some(Notice::warn("careful"));
        let text = format!("{}", render(&home).backend());
        assert!(text.contains("! careful"), "{text}");
    }

    #[test]
    fn many_errors_on_a_short_terminal_keep_the_footer_apart() {
        let problems = (1..=15)
            .map(|line| PlanProblem {
                line,
                message: format!("problem {line}"),
            })
            .collect();
        let mut home = home(PlanState::Invalid(problems));
        home.data.plan_uncommitted = true;
        home.message = Some(Notice::error("Could not read the plan."));
        let terminal = render_sized(&home, 80, 20);
        let text = format!("{}", terminal.backend());
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[18].contains("Could not read the plan."), "{text}");
        assert!(rows[19].contains("? shortcuts"), "{text}");
        assert!(text.contains(" more"), "{text}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn long_plans_list_what_fits_and_count_the_rest() {
        let PlanState::Valid(mut plan) = valid_plan() else {
            unreachable!()
        };
        for n in 4..=30 {
            plan.phases.push(HomePhase {
                agent: None,
                number: n,
                title: format!("Phase {n}"),
                checks: 1,
                model: None,
                effort: None,
            });
        }
        let home = home(PlanState::Valid(plan));
        let terminal = render(&home);
        let text = format!("{}", terminal.backend());
        assert!(text.contains(" more"), "{text}");
        assert!(
            text.contains("enter start"),
            "the footer stays on screen\n{text}"
        );
    }

    #[test]
    fn small_terminal_shows_only_the_message() {
        let terminal = render_sized(&home(valid_plan()), 60, 15);
        insta::assert_snapshot!(terminal.backend());
    }
}
