//! Formatting of engine events and run states, shared by the CLI and the TUI.

use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use crate::agent::claude::CLAUDE_MODELS;
use crate::agent::{AgentEvent, RateWindow};
use crate::engine::EngineEvent;
use crate::ops::PlanEvent;
use crate::plan::AgentKind;
use crate::state::{FailureKind, PhaseStatus, RateLimits};
use crate::style::{Role, Styler};

const MAX_MESSAGE_CHARS: usize = 160;

pub fn first_line_short(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    line.unwrap_or("").chars().take(MAX_MESSAGE_CHARS).collect()
}

/// The name to show for a model value. Claude: a catalog ID gives its name, an
/// ID followed by `[1m]` gives the name and the context size; anything else
/// (aliases included) and every Codex value is shown as written.
pub fn model_name(agent: AgentKind, value: &str) -> String {
    if agent == AgentKind::Claude {
        let (id, long_context) = match value.strip_suffix("[1m]") {
            Some(id) => (id, true),
            None => (value, false),
        };
        if let Some((name, _, _)) = CLAUDE_MODELS
            .iter()
            .find(|(_, model_id, _)| *model_id == id)
        {
            return if long_context {
                format!("{name} · 1M context")
            } else {
                (*name).to_string()
            };
        }
    }
    value.to_string()
}

/// `path` relative to `base` when it is inside it, else as given.
pub fn path_in(base: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(base).unwrap_or(path).to_path_buf()
}

/// `path` with the home directory written as `~`.
pub fn path_with_home(home: Option<&Path>, path: &Path) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Width the phase header is padded to, so its right part lines up.
const HEADER_WIDTH: usize = 80;
/// Labels are padded to this width in event lines.
const LABEL_WIDTH: usize = 8;

/// One line of progress, structured so that the CLI (ANSI codes) and the TUI
/// (ratatui styles) render the same content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLine {
    /// Seconds since the run started; `None` where there is no clock (TUI log).
    pub time: Option<u64>,
    pub icon: &'static str,
    pub label: String,
    pub detail: String,
    pub role: Role,
}

impl EventLine {
    fn new(icon: &'static str, label: &str, detail: impl Into<String>, role: Role) -> EventLine {
        EventLine {
            time: None,
            icon,
            label: label.to_string(),
            detail: detail.into(),
            role,
        }
    }

    pub fn at(mut self, secs: u64) -> EventLine {
        self.time = Some(secs);
        self
    }

    /// `label` padded to its column, or empty when the line has no label.
    pub fn label_column(&self) -> String {
        if self.label.is_empty() {
            String::new()
        } else {
            format!("{:<LABEL_WIDTH$}", self.label)
        }
    }

    /// Detail is coloured only for warnings and failures; otherwise it stays plain.
    pub fn detail_role(&self) -> Role {
        match self.role {
            Role::Warn | Role::Fail => self.role,
            _ => Role::Default,
        }
    }

    /// `  mm:ss  <icon> <label padded to 8> <detail>`.
    pub fn render(&self, styler: &Styler) -> String {
        let mut out = String::from("  ");
        if let Some(secs) = self.time {
            out.push_str(&styler.dim(&format!("{:02}:{:02}", secs / 60, secs % 60)));
            out.push_str("  ");
        }
        out.push_str(&styler.paint(self.role, self.icon));
        if !self.label.is_empty() {
            out.push(' ');
            let label = if self.detail.is_empty() {
                self.label.clone()
            } else {
                self.label_column()
            };
            out.push_str(&styler.paint(self.role, &label));
        }
        if !self.detail.is_empty() {
            out.push(' ');
            out.push_str(&styler.paint(self.detail_role(), &self.detail));
        }
        out
    }
}

/// Line for an engine event. `None` for the events that are not lines:
/// the phase start (see [`phase_header`]), the phase end (see
/// [`phase_summary`]) and the agent events that carry nothing to show.
pub fn event_line(event: &EngineEvent, verify_secs: u64) -> Option<EventLine> {
    Some(match event {
        EngineEvent::PhaseStarted { .. } | EngineEvent::PhaseFinished { .. } => return None,
        EngineEvent::Agent { event, .. } => return agent_event_line(event),
        EngineEvent::AgentExited { exit_code, .. } => match exit_code {
            Some(0) => EventLine::new("✓", "Agent finished", "", Role::Ok),
            Some(code) => {
                EventLine::new("✗", "Agent", format!("exited with code {code}"), Role::Fail)
            }
            None => EventLine::new("!", "Agent", "stopped", Role::Warn),
        },
        EngineEvent::VerifyStarted { command, .. } => {
            EventLine::new("⚙", "Check", command.as_str(), Role::Dim)
        }
        EngineEvent::VerifyFinished {
            command,
            ok,
            exit_code,
            timed_out,
            ..
        } => {
            if *ok {
                EventLine::new("✓", "Passed", command.as_str(), Role::Ok)
            } else {
                let why = if *timed_out {
                    format!("timed out after {verify_secs} s")
                } else {
                    match exit_code {
                        Some(code) => format!("exit code {code}"),
                        None => "stopped".to_string(),
                    }
                };
                EventLine::new("✗", "Failed", format!("{command} ({why})"), Role::Fail)
            }
        }
        EngineEvent::Retrying { reason, .. } => {
            EventLine::new("!", "Retry", reason.as_str(), Role::Warn)
        }
        EngineEvent::CheckpointCreated { sha, .. } => EventLine::new(
            "●",
            "Checkpoint",
            sha.chars().take(7).collect::<String>(),
            Role::Accent,
        ),
    })
}

fn tool_icon(name: &str, subagent: bool) -> &'static str {
    if subagent {
        return "↳";
    }
    match name {
        "Read" | "Grep" | "Glob" => "◦",
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => "✎",
        "Bash" | "Shell" => "$",
        _ => "·",
    }
}

pub fn agent_event_line(event: &AgentEvent) -> Option<EventLine> {
    match event {
        AgentEvent::SessionStarted { .. }
        | AgentEvent::Finished { .. }
        | AgentEvent::RateLimits(_) => None,
        AgentEvent::Message { text, subagent } => {
            let line = first_line_short(text);
            let tag = if *subagent { "[subagent] " } else { "" };
            (!line.is_empty())
                .then(|| EventLine::new("›", "", format!("{tag}{line}"), Role::Default))
        }
        AgentEvent::ToolCall {
            name,
            detail,
            subagent,
        } => Some(EventLine::new(
            tool_icon(name, *subagent),
            name,
            detail.as_str(),
            Role::Tool,
        )),
        AgentEvent::Retry { detail } => {
            Some(EventLine::new("!", "Retry", detail.as_str(), Role::Warn))
        }
        AgentEvent::PermissionDenied { detail } => {
            Some(EventLine::new("!", "Denied", detail.as_str(), Role::Warn))
        }
        AgentEvent::Error { detail } => Some(EventLine::new(
            "✗",
            "Error",
            first_line_short(detail),
            Role::Fail,
        )),
    }
}

/// The heading of a phase block: `▶ Phase 1/2 · Title`, then the dim right
/// part (`attempt 1 · claude`), padded so it ends at column 80.
///
/// The model and effort of the attempt follow the agent when they are set.
pub fn phase_header(event: &EngineEvent, styler: &Styler) -> Option<String> {
    let EngineEvent::PhaseStarted {
        phase,
        total,
        title,
        attempt,
        resumed,
        agent,
        model,
        effort,
    } = event
    else {
        return None;
    };
    let left = format!("▶ Phase {phase}/{total} · {title}");
    let mut parts = vec![if *resumed {
        format!("attempt {attempt} · resumed")
    } else {
        format!("attempt {attempt}")
    }];
    parts.push(agent.to_string());
    parts.extend(model.clone());
    parts.extend(effort.clone());
    let right = parts.join(" · ");
    let pad = HEADER_WIDTH
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(2);
    Some(format!(
        "{}{}{}",
        styler.heading(&left),
        " ".repeat(pad),
        styler.dim(&right)
    ))
}

/// Heading of a planning session: `▶ Planning · claude · opus · high`.
pub fn plan_header(event: &PlanEvent, styler: &Styler) -> Option<String> {
    let PlanEvent::Started {
        agent,
        model,
        effort,
        ..
    } = event
    else {
        return None;
    };
    let mut parts = vec!["▶ Planning".to_string(), agent.to_string()];
    parts.extend(model.clone());
    parts.extend(effort.clone());
    Some(styler.heading(&parts.join(" · ")))
}

/// Line for a planning event; `None` for the heading (see [`plan_header`]) and
/// for agent events with nothing to show.
pub fn plan_event_line(event: &PlanEvent) -> Option<EventLine> {
    Some(match event {
        PlanEvent::Started { .. } => return None,
        PlanEvent::Agent { event } => {
            // The answer that carries the plan would show as its marker line.
            if let AgentEvent::Message { text, .. } = event
                && let Some(plan) = crate::prompt::extract_plan(text)
            {
                let lines = plan.lines().count();
                return Some(EventLine::new(
                    "›",
                    "",
                    format!("Plan draft ({lines} lines)"),
                    Role::Default,
                ));
            }
            return agent_event_line(event);
        }
        PlanEvent::AttemptFinished { exit_code, .. } => {
            return event_line(
                &EngineEvent::AgentExited {
                    phase: 0,
                    exit_code: *exit_code,
                },
                0,
            );
        }
        PlanEvent::Repairing { round, problems } => EventLine::new(
            "!",
            "Repair",
            format!(
                "round {round}: {}",
                problems
                    .iter()
                    .map(|p| first_line_short(p))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Role::Warn,
        ),
        PlanEvent::Written { path } => {
            EventLine::new("✓", "Written", path.display().to_string(), Role::Ok)
        }
    })
}

/// `1 phase`, `2 phases`.
pub fn phase_count(n: usize) -> String {
    if n == 1 {
        "1 phase".to_string()
    } else {
        format!("{n} phases")
    }
}

pub fn money(cost: f64) -> String {
    if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

/// A duration as `45s`, `2m 05s` or `1h 03m`; fractions of a second are dropped.
pub fn duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Limits seen longer ago than this get a `seen HH:MM` note.
const LIMITS_STALE_SECS: i64 = 600;

/// A piece of the limits line; `warn` for a window whose status is not `allowed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitPart {
    pub text: String,
    pub warn: bool,
}

/// The current time and the local time zone, for [`limit_parts`]. Fixed under
/// test so that snapshots are deterministic.
pub fn limits_clock() -> (Timestamp, TimeZone) {
    #[cfg(test)]
    {
        (
            Timestamp::from_second(1_794_660_000).unwrap_or(Timestamp::UNIX_EPOCH),
            TimeZone::UTC,
        )
    }
    #[cfg(not(test))]
    {
        (Timestamp::now(), TimeZone::system())
    }
}

fn window_label(name: &str) -> &str {
    match name {
        "five_hour" => "5 h",
        "seven_day" => "7 d",
        other => other,
    }
}

/// `23:00` when `at` is on the same local day as `now`, else `Thu 12:00`.
fn clock_text(at: i64, now: Timestamp, tz: &TimeZone) -> Option<String> {
    let at: Zoned = Timestamp::from_second(at).ok()?.to_zoned(tz.clone());
    let now = now.to_zoned(tz.clone());
    let format = if at.date() == now.date() {
        "%H:%M"
    } else {
        "%a %H:%M"
    };
    Some(at.strftime(format).to_string())
}

/// The windows of the limits line, one part each, plus a final `seen HH:MM`
/// part when the values are older than ten minutes:
/// `5 h 36 % · resets 23:00`. A window that has reset shows `5 h reset`.
pub fn limit_parts(limits: &RateLimits, now: Timestamp, tz: &TimeZone) -> Vec<LimitPart> {
    let now_secs = now.as_second();
    let mut parts: Vec<LimitPart> = limits
        .windows
        .iter()
        .map(|w| window_part(w, now, tz))
        .collect();
    if now_secs - limits.seen_at > LIMITS_STALE_SECS
        && let Some(at) = clock_text(limits.seen_at, now, tz)
    {
        parts.push(LimitPart {
            text: format!("seen {at}"),
            warn: false,
        });
    }
    parts
}

fn window_part(w: &RateWindow, now: Timestamp, tz: &TimeZone) -> LimitPart {
    let label = window_label(&w.name);
    if w.resets_at.is_some_and(|at| at <= now.as_second()) {
        return LimitPart {
            text: format!("{label} reset"),
            warn: false,
        };
    }
    let mut text = label.to_string();
    if let Some(u) = w.utilization {
        text.push_str(&format!(" {:.0} %", u * 100.0));
    }
    let warn = w.status != "allowed";
    if warn {
        text.push_str(&format!(" · {}", w.status));
    }
    if let Some(at) = w.resets_at.and_then(|at| clock_text(at, now, tz)) {
        text.push_str(&format!(" · resets {at}"));
    }
    LimitPart { text, warn }
}

/// The limits line as plain text, parts separated by three spaces.
pub fn limits_text(limits: &RateLimits, now: Timestamp, tz: &TimeZone) -> String {
    limit_parts(limits, now, tz)
        .into_iter()
        .map(|p| p.text)
        .collect::<Vec<_>>()
        .join("   ")
}

/// The last line of a phase: ready for review (with files and cost when
/// known) or failed. `None` for other outcomes.
pub fn phase_summary(event: &EngineEvent, styler: &Styler) -> Option<String> {
    let EngineEvent::PhaseFinished {
        phase,
        status,
        failure,
        files,
        cost_usd,
    } = event
    else {
        return None;
    };
    if let Some(failure) = failure {
        let text = format!(
            "✗ Phase {phase} failed ({})",
            failure_kind_label(failure.kind)
        );
        return Some(styler.fail(&text));
    }
    if *status != PhaseStatus::AwaitingReview {
        return None;
    }
    let mut text = format!("✓ Phase {phase} ready for review");
    if let Some(n) = files {
        text.push_str(&format!(" · {n} file{}", if *n == 1 { "" } else { "s" }));
    }
    if let Some(cost) = cost_usd {
        text.push_str(&format!(" · {}", money(*cost)));
    }
    Some(styler.ok(&text))
}

pub fn status_label(status: PhaseStatus) -> &'static str {
    match status {
        PhaseStatus::Pending => "pending",
        PhaseStatus::Running => "running",
        PhaseStatus::Verifying => "verifying",
        PhaseStatus::AwaitingReview => "awaiting review",
        PhaseStatus::Done => "done",
        PhaseStatus::Failed => "failed",
    }
}

pub fn failure_kind_label(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::AgentError => "agent_error",
        FailureKind::VerifyFailed => "verify_failed",
        FailureKind::Timeout => "timeout",
        FailureKind::Interrupted => "interrupted",
        FailureKind::CheckpointFailed => "checkpoint_failed",
    }
}

/// A failure kind in plain words, for the run screen.
pub fn failure_kind_words(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::AgentError => "Agent error",
        FailureKind::VerifyFailed => "Checks failed",
        FailureKind::Timeout => "Timed out",
        FailureKind::Interrupted => "Interrupted",
        FailureKind::CheckpointFailed => "Checkpoint failed",
    }
}

/// One phase of a run report.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportPhase {
    pub number: usize,
    pub title: String,
    pub status: PhaseStatus,
    pub agent: AgentKind,
    pub secs: Option<u64>,
    pub cost_usd: Option<f64>,
    pub files: Option<usize>,
    /// The phase's own checks.
    pub verify: Vec<String>,
}

/// What `phasewise report` needs, already read from the run.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub title: Option<String>,
    pub run_id: String,
    pub run_branch: String,
    pub original_branch: String,
    pub agent: AgentKind,
    /// The plan's global checks.
    pub verify: Vec<String>,
    pub phases: Vec<ReportPhase>,
    /// Time of the finished phases, as in `status`.
    pub total_secs: Option<u64>,
    /// Cost of the run, `None` when no phase reported one.
    pub total_cost: Option<f64>,
    /// Files changed from the start of the run to the last checkpoint.
    pub total_files: Option<usize>,
}

/// One line of text.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A table cell: one line, `|` escaped.
fn md_cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

/// An inline code span on one line; a longer fence when the text has backticks.
fn md_code(text: &str) -> String {
    let text = one_line(text);
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

/// The run as Markdown for a pull request description. Pure and stable: it
/// uses only the data it is given.
pub fn report_markdown(report: &Report) -> String {
    let mixed = report.phases.iter().any(|p| p.agent != report.agent);
    let title = report
        .title
        .as_deref()
        .map(one_line)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "phasewise run".to_string());
    let mut out = format!("# {title}\n\n");
    out.push_str(&format!(
        "Run {} on branch {} (from {}), agent: {}\n\n",
        md_code(&report.run_id),
        md_code(&report.run_branch),
        md_code(&report.original_branch),
        report.agent
    ));
    let (head, rule) = if mixed {
        (
            "| # | Phase | Agent | Status | Time | Cost | Files |",
            "|--:|---|---|---|--:|--:|--:|",
        )
    } else {
        (
            "| # | Phase | Status | Time | Cost | Files |",
            "|--:|---|---|--:|--:|--:|",
        )
    };
    out.push_str(&format!("{head}\n{rule}\n"));
    for p in &report.phases {
        let mut cells = vec![p.number.to_string(), md_cell(&p.title)];
        if mixed {
            cells.push(p.agent.to_string());
        }
        cells.push(status_label(p.status).to_string());
        cells.push(p.secs.map_or_else(|| "-".to_string(), duration));
        cells.push(
            p.cost_usd
                .map_or_else(|| "-".to_string(), |c| format!("${c:.4}")),
        );
        cells.push(p.files.map_or_else(|| "-".to_string(), |n| n.to_string()));
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out.push_str("\n## Checks\n\n");
    let mut checks: Vec<String> = report
        .verify
        .iter()
        .map(|c| format!("- {}", md_code(c)))
        .collect();
    for p in &report.phases {
        for c in &p.verify {
            checks.push(format!("- Phase {}: {}", p.number, md_code(c)));
        }
    }
    if checks.is_empty() {
        out.push_str("The plan has no checks.\n");
    } else {
        for line in checks {
            out.push_str(&format!("{line}\n"));
        }
    }
    let done = report
        .phases
        .iter()
        .filter(|p| p.status == PhaseStatus::Done)
        .count();
    let mut totals = vec![format!(
        "{done} of {} done",
        phase_count(report.phases.len())
    )];
    if let Some(secs) = report.total_secs {
        totals.push(duration(secs));
    }
    if let Some(cost) = report.total_cost {
        totals.push(money(cost));
    }
    if let Some(n) = report.total_files {
        let noun = if n == 1 { "file" } else { "files" };
        totals.push(format!("{n} {noun} changed"));
    }
    out.push_str(&format!("\n## Totals\n\n{}\n", totals.join(", ")));
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn model_names() {
        let c = AgentKind::Claude;
        assert_eq!(model_name(c, "claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name(c, "claude-haiku-5-5"), "Haiku 5.5");
        assert_eq!(
            model_name(c, "claude-sonnet-5-5[1m]"),
            "Sonnet 5.5 · 1M context"
        );
        assert_eq!(model_name(c, "opus"), "opus");
        assert_eq!(model_name(c, "opus[1m]"), "opus[1m]");
        assert_eq!(model_name(c, "claude-unknown-1"), "claude-unknown-1");
        assert_eq!(
            model_name(AgentKind::Codex, "claude-opus-5-5"),
            "claude-opus-5-5"
        );
        assert_eq!(model_name(AgentKind::Codex, "gpt-5"), "gpt-5");
    }

    #[test]
    fn paths_are_shortened() {
        assert_eq!(
            path_in(Path::new("/a/b"), Path::new("/a/b/c/d.md")),
            Path::new("c/d.md")
        );
        assert_eq!(
            path_in(Path::new("/a/b"), Path::new("/x/d.md")),
            Path::new("/x/d.md")
        );
        let home = Some(Path::new("/home/u"));
        assert_eq!(path_with_home(home, Path::new("/home/u/bin/x")), "~/bin/x");
        assert_eq!(path_with_home(home, Path::new("/home/u")), "~");
        assert_eq!(path_with_home(home, Path::new("/usr/bin/x")), "/usr/bin/x");
        assert_eq!(path_with_home(None, Path::new("/home/u/x")), "/home/u/x");
    }

    use super::*;
    use crate::state::Failure;

    // 12:00:00 UTC on a Saturday; windows below are relative to it.
    const NOW: i64 = 1_794_657_600;

    fn rate(name: &str, u: Option<f64>, resets: Option<i64>, status: &str) -> RateWindow {
        RateWindow {
            name: name.into(),
            utilization: u,
            resets_at: resets,
            status: status.into(),
        }
    }

    fn limits(seen_at: i64, windows: Vec<RateWindow>) -> RateLimits {
        RateLimits {
            agent: AgentKind::Claude,
            seen_at,
            windows,
        }
    }

    fn at(secs: i64, tz: &TimeZone) -> String {
        let l = limits(
            secs,
            vec![
                rate("five_hour", Some(0.36), Some(NOW + 11 * 3600), "allowed"),
                rate("seven_day", Some(0.49), Some(NOW + 4 * 86400), "allowed"),
            ],
        );
        limits_text(&l, Timestamp::from_second(secs).unwrap(), tz)
    }

    #[test]
    fn duration_text_scales_from_seconds_to_hours() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(61), "1m 01s");
        assert_eq!(duration(252), "4m 12s");
        assert_eq!(duration(3599), "59m 59s");
        assert_eq!(duration(3600), "1h 00m");
        assert_eq!(duration(3780), "1h 03m");
        // Seconds past the minute are dropped, not rounded up.
        assert_eq!(duration(3659), "1h 00m");
        assert_eq!(duration(7199), "1h 59m");
    }

    #[test]
    fn limits_line_shows_percent_and_local_reset_times() {
        assert_eq!(
            at(NOW, &TimeZone::UTC),
            "5 h 36 % · resets 23:00   7 d 49 % · resets Wed 12:00"
        );
        let plus_two = TimeZone::fixed(jiff::tz::offset(2));
        assert_eq!(
            at(NOW, &plus_two),
            "5 h 36 % · resets Sun 01:00   7 d 49 % · resets Wed 14:00"
        );
    }

    #[test]
    fn a_reset_on_the_same_day_shows_only_the_time() {
        let l = limits(
            NOW,
            vec![rate("five_hour", Some(0.1), Some(NOW + 3600), "allowed")],
        );
        let now = Timestamp::from_second(NOW).unwrap();
        assert_eq!(
            limits_text(&l, now, &TimeZone::UTC),
            "5 h 10 % · resets 13:00"
        );
    }

    #[test]
    fn a_window_that_has_reset_hides_its_stale_percentage() {
        let l = limits(
            NOW - 60,
            vec![
                rate("five_hour", Some(0.9), Some(NOW - 1), "allowed_warning"),
                rate("seven_day", Some(0.2), Some(NOW + 3600), "allowed"),
            ],
        );
        let now = Timestamp::from_second(NOW).unwrap();
        assert_eq!(
            limits_text(&l, now, &TimeZone::UTC),
            "5 h reset   7 d 20 % · resets 13:00"
        );
    }

    #[test]
    fn a_status_other_than_allowed_is_shown_and_flagged() {
        let l = limits(
            NOW,
            vec![
                rate("five_hour", Some(0.91), Some(NOW + 3600), "allowed_warning"),
                rate("opus", None, None, "rejected"),
            ],
        );
        let now = Timestamp::from_second(NOW).unwrap();
        let parts = limit_parts(&l, now, &TimeZone::UTC);
        assert_eq!(parts[0].text, "5 h 91 % · allowed_warning · resets 13:00");
        assert_eq!(parts[1].text, "opus · rejected");
        assert!(parts[0].warn && parts[1].warn);
    }

    #[test]
    fn old_limits_get_a_seen_note_and_recent_ones_do_not() {
        let w = vec![rate("five_hour", Some(0.5), Some(NOW + 3600), "allowed")];
        let now = Timestamp::from_second(NOW).unwrap();
        let recent = limits(NOW - 600, w.clone());
        assert!(!limits_text(&recent, now, &TimeZone::UTC).contains("seen"));
        let old = limits(NOW - 601, w);
        assert_eq!(
            limits_text(&old, now, &TimeZone::UTC),
            "5 h 50 % · resets 13:00   seen 11:49"
        );
    }

    fn line(event: &EngineEvent) -> EventLine {
        event_line(event, 30).expect("a line")
    }

    fn agent(event: AgentEvent) -> EventLine {
        agent_event_line(&event).expect("a line")
    }

    fn call(name: &str, subagent: bool) -> EventLine {
        agent(AgentEvent::ToolCall {
            name: name.into(),
            detail: "x".into(),
            subagent,
        })
    }

    fn parts(l: &EventLine) -> (&'static str, &str, &str, Role) {
        (l.icon, l.label.as_str(), l.detail.as_str(), l.role)
    }

    #[test]
    fn tool_calls_get_their_icon_and_the_tool_role() {
        for (name, icon) in [
            ("Read", "◦"),
            ("Grep", "◦"),
            ("Glob", "◦"),
            ("Write", "✎"),
            ("Edit", "✎"),
            ("Bash", "$"),
            ("Shell", "$"),
            ("WebSearch", "·"),
            ("MCP", "·"),
        ] {
            let l = call(name, false);
            assert_eq!(parts(&l), (icon, name, "x", Role::Tool), "{name}");
        }
        assert_eq!(call("Read", true).icon, "↳");
        assert_eq!(call("Subagent", true).icon, "↳");
    }

    #[test]
    fn agent_events_map_to_lines() {
        let m = agent(AgentEvent::Message {
            text: "\n  Created it.\nmore".into(),
            subagent: false,
        });
        assert_eq!(parts(&m), ("›", "", "Created it.", Role::Default));
        let m = agent(AgentEvent::Message {
            text: "hi".into(),
            subagent: true,
        });
        assert_eq!(m.detail, "[subagent] hi");
        let long = agent(AgentEvent::Message {
            text: "é".repeat(500),
            subagent: false,
        });
        assert_eq!(long.detail.chars().count(), MAX_MESSAGE_CHARS);
        assert_eq!(
            parts(&agent(AgentEvent::Retry {
                detail: "slow".into()
            })),
            ("!", "Retry", "slow", Role::Warn)
        );
        assert_eq!(
            parts(&agent(AgentEvent::PermissionDenied {
                detail: "Bash rm".into()
            })),
            ("!", "Denied", "Bash rm", Role::Warn)
        );
        assert_eq!(
            parts(&agent(AgentEvent::Error {
                detail: "boom\nsecond".into()
            })),
            ("✗", "Error", "boom", Role::Fail)
        );
        assert!(
            agent_event_line(&AgentEvent::SessionStarted {
                session_id: "s".into(),
                model: None
            })
            .is_none()
        );
        assert!(
            agent_event_line(&AgentEvent::Finished {
                ok: true,
                summary: String::new(),
                cost_usd: None
            })
            .is_none()
        );
        let empty = AgentEvent::Message {
            text: "  \n".into(),
            subagent: false,
        };
        assert!(agent_event_line(&empty).is_none());
    }

    #[test]
    fn engine_events_map_to_lines() {
        let exited = |code| EngineEvent::AgentExited {
            phase: 1,
            exit_code: code,
        };
        assert_eq!(
            parts(&line(&exited(Some(0)))),
            ("✓", "Agent finished", "", Role::Ok)
        );
        assert_eq!(
            parts(&line(&exited(Some(2)))),
            ("✗", "Agent", "exited with code 2", Role::Fail)
        );
        assert_eq!(
            parts(&line(&exited(None))),
            ("!", "Agent", "stopped", Role::Warn)
        );
        let started = EngineEvent::VerifyStarted {
            phase: 1,
            command: "cargo test".into(),
        };
        assert_eq!(
            parts(&line(&started)),
            ("⚙", "Check", "cargo test", Role::Dim)
        );
        let finished = |ok, exit_code, timed_out| EngineEvent::VerifyFinished {
            phase: 1,
            command: "c".into(),
            ok,
            exit_code,
            timed_out,
        };
        assert_eq!(
            parts(&line(&finished(true, Some(0), false))),
            ("✓", "Passed", "c", Role::Ok)
        );
        assert_eq!(
            parts(&line(&finished(false, Some(1), false))),
            ("✗", "Failed", "c (exit code 1)", Role::Fail)
        );
        assert_eq!(
            parts(&line(&finished(false, None, true))),
            ("✗", "Failed", "c (timed out after 30 s)", Role::Fail)
        );
        assert_eq!(
            parts(&line(&finished(false, None, false))),
            ("✗", "Failed", "c (stopped)", Role::Fail)
        );
        let retry = EngineEvent::Retrying {
            phase: 1,
            reason: "`c` failed".into(),
        };
        assert_eq!(
            parts(&line(&retry)),
            ("!", "Retry", "`c` failed", Role::Warn)
        );
        let checkpoint = EngineEvent::CheckpointCreated {
            phase: 1,
            sha: "821c664abcdef".into(),
        };
        assert_eq!(
            parts(&line(&checkpoint)),
            ("●", "Checkpoint", "821c664", Role::Accent)
        );
    }

    #[test]
    fn started_and_finished_events_are_not_lines() {
        let started = EngineEvent::PhaseStarted {
            agent: AgentKind::Claude,
            phase: 1,
            total: 2,
            title: "T".into(),
            attempt: 1,
            resumed: false,
            model: None,
            effort: None,
        };
        let finished = EngineEvent::PhaseFinished {
            phase: 1,
            status: PhaseStatus::AwaitingReview,
            failure: None,
            files: None,
            cost_usd: None,
        };
        assert!(event_line(&started, 30).is_none());
        assert!(event_line(&finished, 30).is_none());
    }

    #[test]
    fn lines_render_with_time_icon_padded_label_and_detail() {
        let plain = Styler { color: false };
        let write = call("Write", false).at(1);
        assert_eq!(write.render(&plain), "  00:01  ✎ Write    x");
        let msg = agent(AgentEvent::Message {
            text: "Created it.".into(),
            subagent: false,
        })
        .at(125);
        assert_eq!(msg.render(&plain), "  02:05  › Created it.");
        let done = line(&EngineEvent::AgentExited {
            phase: 1,
            exit_code: Some(0),
        })
        .at(3);
        assert_eq!(done.render(&plain), "  00:03  ✓ Agent finished");
        assert_eq!(call("Bash", false).render(&plain), "  $ Bash     x");
        let coloured = write.render(&Styler { color: true });
        assert!(coloured.contains("\x1b["), "{coloured:?}");
    }

    #[test]
    fn header_shows_attempt_agent_and_optional_settings() {
        let plain = Styler { color: false };
        let started = |attempt, resumed, agent, model: Option<&str>, effort: Option<&str>| {
            EngineEvent::PhaseStarted {
                phase: 1,
                total: 2,
                title: "Create a file".into(),
                attempt,
                resumed,
                agent,
                model: model.map(str::to_string),
                effort: effort.map(str::to_string),
            }
        };
        let h = phase_header(&started(1, false, AgentKind::Claude, None, None), &plain)
            .expect("header");
        assert!(h.starts_with("▶ Phase 1/2 · Create a file  "), "{h}");
        assert!(h.ends_with("attempt 1 · claude"), "{h}");
        assert_eq!(h.chars().count(), HEADER_WIDTH);
        let h = phase_header(
            &started(2, true, AgentKind::Codex, Some("opus"), Some("high")),
            &plain,
        )
        .expect("header");
        assert!(
            h.ends_with("attempt 2 · resumed · codex · opus · high"),
            "{h}"
        );
        let finished = EngineEvent::PhaseFinished {
            phase: 1,
            status: PhaseStatus::Done,
            failure: None,
            files: None,
            cost_usd: None,
        };
        assert!(phase_header(&finished, &plain).is_none());
    }

    #[test]
    fn summary_has_files_and_cost_when_known() {
        let plain = Styler { color: false };
        let finished = |files, cost_usd| EngineEvent::PhaseFinished {
            phase: 3,
            status: PhaseStatus::AwaitingReview,
            failure: None,
            files,
            cost_usd,
        };
        let text = |e: &EngineEvent| phase_summary(e, &plain).expect("summary");
        assert_eq!(
            text(&finished(Some(1), Some(0.06))),
            "✓ Phase 3 ready for review · 1 file · $0.06"
        );
        assert_eq!(
            text(&finished(Some(4), None)),
            "✓ Phase 3 ready for review · 4 files"
        );
        assert_eq!(text(&finished(None, None)), "✓ Phase 3 ready for review");
        assert_eq!(
            text(&finished(Some(0), Some(0.004))),
            "✓ Phase 3 ready for review · 0 files · $0.0040"
        );
        let failed = EngineEvent::PhaseFinished {
            phase: 2,
            status: PhaseStatus::Failed,
            failure: Some(Failure {
                kind: FailureKind::VerifyFailed,
                detail: "d".into(),
            }),
            files: None,
            cost_usd: None,
        };
        assert_eq!(text(&failed), "✗ Phase 2 failed (verify_failed)");
        assert!(
            phase_summary(&failed, &Styler { color: true }).is_some_and(|s| s.contains("\x1b["))
        );
    }

    #[test]
    fn plan_event_lines() {
        let plain = Styler { color: false };
        let render = |e: PlanEvent| plan_event_line(&e).map(|l| l.render(&plain));
        let message = |text: &str| PlanEvent::Agent {
            event: AgentEvent::Message {
                text: text.into(),
                subagent: false,
            },
        };
        assert_eq!(
            render(message("<<<PHASEWISE-PLAN\n## A\n\nDo.\nPHASEWISE-PLAN>>>")).as_deref(),
            Some("  › Plan draft (3 lines)")
        );
        assert_eq!(
            render(message("Reading the project.")).as_deref(),
            Some("  › Reading the project.")
        );
        // An unfinished draft is only an ordinary message.
        assert_eq!(
            render(message("<<<PHASEWISE-PLAN\n## A")).as_deref(),
            Some("  › <<<PHASEWISE-PLAN")
        );
        assert_eq!(
            render(PlanEvent::Repairing {
                round: 2,
                problems: vec!["first\nmore".into(), "second".into()],
            })
            .as_deref(),
            Some("  ! Repair   round 2: first; second")
        );
        assert_eq!(
            render(PlanEvent::Written {
                path: "phasewise.md".into(),
            })
            .as_deref(),
            Some("  ✓ Written  phasewise.md")
        );
        assert_eq!(
            render(PlanEvent::AttemptFinished {
                attempt: 1,
                exit_code: Some(0),
            })
            .as_deref(),
            Some("  ✓ Agent finished")
        );
        let started = PlanEvent::Started {
            plan_id: "id".into(),
            agent: crate::plan::AgentKind::Codex,
            model: Some("gpt-x".into()),
            effort: None,
        };
        assert!(plan_event_line(&started).is_none());
        assert_eq!(
            plan_header(&started, &plain).as_deref(),
            Some("▶ Planning · codex · gpt-x")
        );
    }

    #[test]
    fn phase_count_is_singular_for_one() {
        assert_eq!(phase_count(0), "0 phases");
        assert_eq!(phase_count(1), "1 phase");
        assert_eq!(phase_count(2), "2 phases");
    }

    fn sample_report() -> Report {
        let phase = |number, title: &str, status, secs, cost_usd, files| ReportPhase {
            number,
            title: title.to_string(),
            status,
            agent: AgentKind::Claude,
            secs,
            cost_usd,
            files,
            verify: Vec::new(),
        };
        Report {
            title: Some("Add | pipes\nand lines".to_string()),
            run_id: "20260101-000000-abcd".to_string(),
            run_branch: "phasewise/20260101-000000-abcd".to_string(),
            original_branch: "main".to_string(),
            agent: AgentKind::Claude,
            verify: vec!["cargo test".to_string(), "echo `x` | grep x".to_string()],
            phases: vec![
                phase(
                    1,
                    "Parse | split",
                    PhaseStatus::Done,
                    Some(125),
                    Some(0.5),
                    Some(3),
                ),
                ReportPhase {
                    verify: vec!["test -f a\n  && test -f b".to_string()],
                    ..phase(
                        2,
                        "Second",
                        PhaseStatus::AwaitingReview,
                        Some(5),
                        None,
                        Some(1),
                    )
                },
                phase(3, "Third", PhaseStatus::Pending, None, None, None),
            ],
            total_secs: Some(130),
            total_cost: Some(0.5),
            total_files: Some(4),
        }
    }

    #[test]
    fn report_snapshot() {
        insta::assert_snapshot!(report_markdown(&sample_report()));
    }

    #[test]
    fn report_shows_the_agent_only_when_the_plan_mixes_them() {
        let mut report = sample_report();
        assert!(!report_markdown(&report).contains("| Agent |"));
        report.phases[1].agent = AgentKind::Codex;
        let text = report_markdown(&report);
        assert!(text.contains("| # | Phase | Agent | Status | Time | Cost | Files |"));
        assert!(text.contains("| 2 | Second | codex | awaiting review | 5s | - | 1 |"));
    }

    #[test]
    fn report_without_cost_time_or_checks() {
        let mut report = sample_report();
        report.title = None;
        report.verify.clear();
        report.phases.iter_mut().for_each(|p| {
            p.cost_usd = None;
            p.secs = None;
            p.verify.clear();
        });
        report.total_cost = None;
        report.total_secs = None;
        report.total_files = None;
        let text = report_markdown(&report);
        assert!(text.starts_with("# phasewise run\n"));
        assert!(text.contains("The plan has no checks."));
        assert!(text.ends_with("## Totals\n\n1 of 3 phases done\n"));
    }

    #[test]
    fn report_escapes_pipes_and_newlines_in_table_cells() {
        let text = report_markdown(&sample_report());
        assert!(text.contains("| 1 | Parse \\| split | done | 2m 05s | $0.5000 | 3 |"));
        assert!(text.starts_with("# Add | pipes and lines\n"));
    }
}
