//! Plan model and parser: a Markdown file with optional TOML front matter.
//!
//! Pure except for [`load`]. Line numbers are 1-based and refer to the
//! original file, front matter included.

use std::fmt;
use std::ops::Range;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Result, anyhow};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Name of the plan file at the repository root.
pub const DEFAULT_PLAN_FILE: &str = "phasewise.md";

/// Coding agent that executes the phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        })
    }
}

impl FromStr for AgentKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(AgentKind::Claude),
            "codex" => Ok(AgentKind::Codex),
            other => Err(format!(
                "unknown agent `{other}`; expected `claude` or `codex`"
            )),
        }
    }
}

/// Effort levels accepted by both agents.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const MAX_MODEL_CHARS: usize = 100;

/// Checks a model name: non-empty, at most 100 characters, no whitespace and
/// no leading `-` (so it can never be read as a flag). `default` is not
/// special here; callers decide what it means.
pub fn validate_model(model: &str) -> Result<(), String> {
    if model.is_empty() {
        return Err("the model name is empty; remove the setting or name a model".to_string());
    }
    if model.chars().count() > MAX_MODEL_CHARS {
        return Err(format!(
            "the model name is longer than {MAX_MODEL_CHARS} characters"
        ));
    }
    if model.chars().any(char::is_whitespace) {
        return Err(format!(
            "the model name `{model}` must not contain whitespace"
        ));
    }
    if model.starts_with('-') {
        return Err(format!(
            "the model name `{model}` must not start with `-`, it would be read as a flag"
        ));
    }
    if model.chars().any(char::is_control) {
        return Err("the model name must not contain control characters".to_string());
    }
    Ok(())
}

/// Checks an effort level (`low`, `medium`, `high`, `xhigh` or `max`).
pub fn validate_effort(effort: &str) -> Result<(), String> {
    if EFFORTS.contains(&effort) {
        Ok(())
    } else {
        Err(format!(
            "unknown effort `{effort}`; expected one of {}",
            EFFORTS.join(", ")
        ))
    }
}

/// Validation of a value written in a plan: `default` is refused there.
fn plan_value(key: &str, value: &str) -> Result<String, String> {
    if value == "default" {
        return Err(format!(
            "`{key} = \"default\"` is not allowed in a plan; remove the `{key}` key to use the agent's default"
        ));
    }
    match key {
        "model" => validate_model(value),
        _ => validate_effort(value),
    }
    .map(|()| value.to_string())
}

/// Settings from the TOML front matter.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanConfig {
    pub agent: AgentKind,
    /// Whether the front matter sets `agent` (else it is the default, Claude).
    pub agent_set: bool,
    pub verify: Vec<String>,
    pub max_retries: u32,
    pub phase_timeout_minutes: u64,
    pub verify_timeout_seconds: u64,
    /// Spending limit for the whole run, in US dollars (agents that report cost).
    pub max_cost_usd: Option<f64>,
    pub agent_args: Vec<String>,
    /// Model for every phase (applies only when the run uses the plan's agent).
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Default for PlanConfig {
    fn default() -> Self {
        PlanConfig {
            agent: AgentKind::Claude,
            agent_set: false,
            verify: Vec::new(),
            max_retries: 1,
            phase_timeout_minutes: 60,
            verify_timeout_seconds: 600,
            max_cost_usd: None,
            agent_args: Vec::new(),
            model: None,
            effort: None,
        }
    }
}

/// One phase of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase {
    pub number: usize,
    pub title: String,
    /// Instructions for the agent, without `verify` blocks.
    pub body: String,
    pub verify: Vec<String>,
    /// From the phase's `phasewise` settings block.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Agent for this phase only, from the settings block.
    pub agent: Option<AgentKind>,
    /// Time limit for the agent in this phase, from the settings block; it
    /// overrides the plan's `phase_timeout_minutes`.
    pub phase_timeout_minutes: Option<u64>,
    /// Line of the phase heading.
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub config: PlanConfig,
    pub title: Option<String>,
    /// Shared context sent in every phase.
    pub context: String,
    pub phases: Vec<Phase>,
}

/// A problem found in the plan; `line` is 1-based.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("line {line}: {message}")]
pub struct PlanError {
    pub line: usize,
    pub message: String,
}

fn err(line: usize, message: impl Into<String>) -> PlanError {
    PlanError {
        line,
        message: message.into(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    agent: Option<String>,
    verify: Option<Vec<String>>,
    max_retries: Option<u32>,
    phase_timeout_minutes: Option<u64>,
    verify_timeout_seconds: Option<u64>,
    max_cost_usd: Option<f64>,
    agent_args: Option<Vec<String>>,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPhaseSettings {
    agent: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    phase_timeout_minutes: Option<u64>,
}

/// The longest timeout a plan or an environment variable can ask for: a year
/// is plenty, and it keeps every `Duration` and `Instant` sum far from overflow.
pub const MAX_TIMEOUT_SECONDS: u64 = 365 * 24 * 60 * 60;

/// Reads and parses a plan file. Errors are rendered one per line as
/// `<path>:<line>: <message>`.
pub fn load(path: &Path) -> Result<Plan> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!(
                "Plan file not found: {}. Create one with `phasewise init` or pass its path.",
                path.display()
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Err(anyhow!(
                "The plan file {} is not valid UTF-8. Save it as UTF-8 text and try again.",
                path.display()
            ));
        }
        Err(e) => {
            return Err(anyhow!(
                "Cannot read the plan file {}: {e}. Check the path and its permissions.",
                path.display()
            ));
        }
    };
    parse(&text).map_err(|errors| {
        let rendered: Vec<String> = errors
            .iter()
            .map(|e| format!("{}:{}: {}", path.display(), e.line, e.message))
            .collect();
        anyhow!(rendered.join("\n"))
    })
}

/// 1-based line of a byte offset.
fn line_of(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// Line (1-based, original file) of the TOML key `key` inside the front matter.
fn key_line(text: &str, fm_first_line: usize, fm_lines: usize, key: &str) -> usize {
    text.lines()
        .enumerate()
        .skip(fm_first_line - 1)
        .take(fm_lines)
        .find(|(_, l)| {
            l.trim_start()
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .map_or(fm_first_line, |(i, _)| i + 1)
}

/// Parses plan text. Returns every problem found, sorted by line.
pub fn parse(text: &str) -> Result<Plan, Vec<PlanError>> {
    // Many Windows editors start a UTF-8 file with a byte order mark. It is
    // not part of the first line, and dropping it leaves line numbers intact.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut errors = Vec::new();
    let mut config = PlanConfig::default();
    let mut body_start = 0;

    if text.lines().next().map(|l| l.trim_end_matches('\r')) == Some("+++") {
        // Byte offset of every line start.
        let mut offset = 0;
        let mut end = None;
        let mut fm_start = 0;
        for (i, raw) in text.split_inclusive('\n').enumerate() {
            let line_start = offset;
            offset += raw.len();
            if i == 0 {
                fm_start = offset;
                continue;
            }
            if raw.trim_end_matches(['\n', '\r']) == "+++" {
                end = Some((line_start, offset, i));
                break;
            }
        }
        match end {
            None => {
                return Err(vec![err(
                    1,
                    "the front matter is not closed; add a line with `+++` after the settings",
                )]);
            }
            Some((fm_end, after, closing_idx)) => {
                body_start = after;
                let fm_text = &text[fm_start..fm_end];
                match parse_front_matter(text, fm_text, fm_start, closing_idx - 1) {
                    Ok(c) => config = c,
                    Err(mut es) => errors.append(&mut es),
                }
            }
        }
    }

    match parse_body(text, body_start) {
        Ok((title, context, phases)) => {
            if errors.is_empty() {
                return Ok(Plan {
                    config,
                    title,
                    context,
                    phases,
                });
            }
        }
        Err(mut es) => errors.append(&mut es),
    }
    errors.sort_by_key(|e| e.line);
    Err(errors)
}

fn parse_front_matter(
    text: &str,
    fm_text: &str,
    fm_start: usize,
    fm_lines: usize,
) -> Result<PlanConfig, Vec<PlanError>> {
    let raw: RawConfig = match toml::from_str(fm_text) {
        Ok(raw) => raw,
        Err(e) => {
            let line = e
                .span()
                .map_or(1, |span: Range<usize>| line_of(text, fm_start + span.start));
            return Err(vec![err(line, e.message().to_string())]);
        }
    };
    let mut errors = Vec::new();
    let mut config = PlanConfig::default();
    if let Some(agent) = raw.agent {
        match agent.parse() {
            Ok(a) => {
                config.agent = a;
                config.agent_set = true;
            }
            Err(m) => errors.push(err(key_line(text, 2, fm_lines, "agent"), m)),
        }
    }
    if let Some(v) = raw.verify {
        config.verify = v;
    }
    if let Some(v) = raw.max_retries {
        config.max_retries = v;
    }
    if let Some(v) = raw.phase_timeout_minutes {
        if v < 1 {
            errors.push(err(
                key_line(text, 2, fm_lines, "phase_timeout_minutes"),
                "`phase_timeout_minutes` must be at least 1",
            ));
        } else if v > MAX_TIMEOUT_SECONDS / 60 {
            errors.push(err(
                key_line(text, 2, fm_lines, "phase_timeout_minutes"),
                format!(
                    "`phase_timeout_minutes` must be at most {} (one year)",
                    MAX_TIMEOUT_SECONDS / 60
                ),
            ));
        }
        config.phase_timeout_minutes = v;
    }
    if let Some(v) = raw.verify_timeout_seconds {
        if v < 1 {
            errors.push(err(
                key_line(text, 2, fm_lines, "verify_timeout_seconds"),
                "`verify_timeout_seconds` must be at least 1",
            ));
        } else if v > MAX_TIMEOUT_SECONDS {
            errors.push(err(
                key_line(text, 2, fm_lines, "verify_timeout_seconds"),
                format!(
                    "`verify_timeout_seconds` must be at most {MAX_TIMEOUT_SECONDS} (one year)"
                ),
            ));
        }
        config.verify_timeout_seconds = v;
    }
    if let Some(v) = raw.max_cost_usd {
        if !v.is_finite() || v <= 0.0 {
            errors.push(err(
                key_line(text, 2, fm_lines, "max_cost_usd"),
                "`max_cost_usd` must be greater than 0",
            ));
        }
        config.max_cost_usd = Some(v);
    }
    if let Some(v) = raw.agent_args {
        // A NUL byte cannot be passed to a program: the run would fail at the
        // start of the first phase, after the plan passed `check`.
        if v.iter().any(|a| a.contains('\0')) {
            errors.push(err(
                key_line(text, 2, fm_lines, "agent_args"),
                "`agent_args` must not contain a NUL byte",
            ));
        }
        config.agent_args = v;
    }
    for (key, value, slot) in [
        ("model", raw.model, &mut config.model),
        ("effort", raw.effort, &mut config.effort),
    ] {
        if let Some(value) = value {
            match plan_value(key, &value) {
                Ok(v) => *slot = Some(v),
                Err(m) => errors.push(err(key_line(text, 2, fm_lines, key), m)),
            }
        }
    }
    if errors.is_empty() {
        Ok(config)
    } else {
        Err(errors)
    }
}

/// Heading source text without `#` markers or setext underline, trimmed.
fn heading_text(source: &str) -> String {
    let source = source.trim();
    if source.starts_with('#') {
        let first = source.lines().next().unwrap_or("").trim();
        let rest = first.trim_start_matches('#');
        let stripped = rest.trim_end_matches('#');
        // A closing `#` sequence needs a space before it (`C#` keeps its `#`).
        let text = if stripped.is_empty() || stripped.ends_with(char::is_whitespace) {
            stripped
        } else {
            rest
        };
        text.trim().to_string()
    } else {
        // Setext: every line but the underline.
        let lines: Vec<&str> = source.lines().collect();
        let n = lines.len().saturating_sub(1).max(1);
        lines[..n]
            .iter()
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Words for "phase", "step", "stage" or "part" in common languages, in
/// lowercase. This is data: extend it freely.
const PHASE_WORDS: &[&str] = &[
    "phase",
    "step",
    "stage",
    "part",
    "fase",
    "paso",
    "etapa",
    "parte",
    "passo",
    "étape",
    "etape",
    "partie",
    "schritt",
    "stufe",
    "teil",
    "abschnitt",
    "tappa",
    "stap",
    "deel",
    "fas",
    "steg",
    "del",
    "trin",
    "faza",
    "krok",
    "etap",
    "część",
    "czesc",
    "fáze",
    "faze",
    "část",
    "cast",
    "vaihe",
    "aşama",
    "asama",
    "adım",
    "adim",
    "фаза",
    "этап",
    "шаг",
    "часть",
    "етап",
    "крок",
    "частина",
    "φάση",
    "βήμα",
    "フェーズ",
    "ステップ",
    "段階",
    "阶段",
    "步骤",
    "階段",
    "步驟",
    "단계",
];

/// Removes a leading phase-number prefix from `title` unless nothing would
/// remain. `position` is the 1-based position of the phase.
///
/// In this order: `Phase <digits>:` is removed whatever the number; a word
/// from `PHASE_WORDS` plus the number of `position` and a separator (`:`, `：`,
/// `-`, `–`, `—`, or `.` and `)` followed by whitespace) is removed
/// (`Fase 1: X`, `Paso 4. X`); a bare `position` followed by `.`, `)` or `:`
/// and whitespace (`1. X`) is removed too. Other words are never removed.
fn strip_phase_prefix(title: &str, position: usize) -> String {
    let keep = || title.to_string();
    let non_empty = |rest: &str| {
        let rest = rest.trim();
        (!rest.is_empty()).then(|| rest.to_string())
    };

    // English `Phase <digits>:`, any number.
    if let Some(head) = title.get(..5)
        && head.eq_ignore_ascii_case("phase")
    {
        let rest = title[5..].trim_start();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0
            && let Some(after) = rest[digits..].trim_start().strip_prefix(':')
        {
            return non_empty(after).unwrap_or_else(keep);
        }
    }

    let number = position.to_string();
    // A known word, then the position number and a separator.
    let word_end = title
        .char_indices()
        .find(|(_, c)| !c.is_alphabetic())
        .map_or(title.len(), |(i, _)| i);
    if word_end > 0 {
        if PHASE_WORDS.contains(&title[..word_end].to_lowercase().as_str())
            && let Some(after) = title[word_end..].trim_start().strip_prefix(number.as_str())
            && !after.starts_with(|c: char| c.is_ascii_digit())
        {
            let after = after.trim_start();
            let text = if let Some(rest) = after.strip_prefix([':', '：', '-', '–', '—']) {
                non_empty(rest)
            } else if let Some(rest) = after.strip_prefix(['.', ')'])
                && rest.starts_with(char::is_whitespace)
            {
                non_empty(rest)
            } else {
                None
            };
            if let Some(text) = text {
                return text;
            }
        }
        return keep();
    }

    // A bare position number, a separator and a space.
    if let Some(after) = title.strip_prefix(number.as_str())
        && let Some(after) = after.strip_prefix(['.', ')', ':'])
        && after.starts_with(char::is_whitespace)
        && let Some(text) = non_empty(after)
    {
        return text;
    }
    keep()
}

struct PhaseHeading {
    title: String,
    range: Range<usize>,
}

type Parsed = (Option<String>, String, Vec<Phase>);

fn parse_body(text: &str, body_start: usize) -> Result<Parsed, Vec<PlanError>> {
    let body = &text[body_start..];
    let mut errors = Vec::new();

    let mut h1: Option<Range<usize>> = None;
    let mut h1_title = None;
    let mut headings: Vec<PhaseHeading> = Vec::new();
    // (range, commands) of every `verify` block.
    let mut verify_blocks: Vec<(Range<usize>, Vec<String>)> = Vec::new();

    // (range, TOML text) of every `phasewise` settings block.
    let mut settings_blocks: Vec<(Range<usize>, String)> = Vec::new();

    let mut in_verify: Option<(Range<usize>, String)> = None;
    let mut in_settings: Option<(Range<usize>, String)> = None;
    for (event, range) in Parser::new_ext(body, Options::empty()).into_offset_iter() {
        let range = range.start + body_start..range.end + body_start;
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let title = heading_text(&text[range.clone()]);
                match level {
                    HeadingLevel::H1 if h1.is_none() && headings.is_empty() => {
                        h1 = Some(range);
                        h1_title = Some(title);
                    }
                    HeadingLevel::H2 => headings.push(PhaseHeading {
                        title: strip_phase_prefix(&title, headings.len() + 1),
                        range,
                    }),
                    _ => {}
                }
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if &*info == "verify" => {
                in_verify = Some((range, String::new()));
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if &*info == "phasewise" => {
                in_settings = Some((range, String::new()));
            }
            Event::Text(t) => {
                if let Some((_, content)) = in_verify.as_mut() {
                    content.push_str(&t);
                }
                if let Some((_, content)) = in_settings.as_mut() {
                    content.push_str(&t);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(block) = in_settings.take() {
                    settings_blocks.push(block);
                }
                if let Some((range, content)) = in_verify.take() {
                    verify_blocks.push((range, commands(&content)));
                }
            }
            _ => {}
        }
    }

    let first_phase_start = headings.first().map_or(text.len(), |h| h.range.start);

    for (range, _) in &verify_blocks {
        if range.start < first_phase_start {
            errors.push(err(
                line_of(text, range.start),
                "a `verify` block must come after a phase heading (`## ...`)",
            ));
        }
    }

    for (range, _) in &settings_blocks {
        if range.start < first_phase_start {
            errors.push(err(
                line_of(text, range.start),
                "a `phasewise` settings block must come after a phase heading (`## ...`)",
            ));
        }
    }

    let mut context = String::new();
    context.push_str(&text[body_start..first_phase_start.max(body_start)]);
    if let Some(h1) = &h1 {
        context = format!(
            "{}{}",
            &text[body_start..h1.start],
            &text[h1.end..first_phase_start]
        );
    }
    let context = context.trim().to_string();

    let mut phases: Vec<Phase> = Vec::new();
    for (i, heading) in headings.iter().enumerate() {
        let end = headings.get(i + 1).map_or(text.len(), |h| h.range.start);
        let mut body = String::new();
        let mut cursor = heading.range.end;
        let mut verify = Vec::new();
        let mut settings = PhaseSettings::default();
        // Blocks to cut out of the body, in file order.
        enum Cut<'a> {
            Verify(&'a [String]),
            Settings(&'a str),
        }
        let mut cuts: Vec<(&Range<usize>, Cut)> = Vec::new();
        for (range, cmds) in &verify_blocks {
            if range.start >= heading.range.end && range.end <= end {
                cuts.push((range, Cut::Verify(cmds)));
            }
        }
        let mut first_settings: Option<usize> = None;
        for (range, content) in &settings_blocks {
            if range.start >= heading.range.end && range.end <= end {
                let line = line_of(text, range.start);
                if let Some(first) = first_settings {
                    errors.push(err(
                        line,
                        format!(
                            "a phase can have only one `phasewise` settings block (the first is at line {first})"
                        ),
                    ));
                } else {
                    first_settings = Some(line);
                    cuts.push((range, Cut::Settings(content)));
                }
            }
        }
        cuts.sort_by_key(|(range, _)| range.start);
        for (range, cut) in cuts {
            body.push_str(&text[cursor..range.start]);
            cursor = range.end;
            match cut {
                Cut::Verify(cmds) => verify.extend(cmds.iter().cloned()),
                Cut::Settings(content) => {
                    let line = line_of(text, range.start);
                    match parse_phase_settings(content, line) {
                        Ok(s) => settings = s,
                        Err(mut es) => errors.append(&mut es),
                    }
                }
            }
        }
        body.push_str(&text[cursor..end]);
        phases.push(Phase {
            number: i + 1,
            title: heading.title.clone(),
            body: body.trim().to_string(),
            verify,
            model: settings.model,
            effort: settings.effort,
            agent: settings.agent,
            phase_timeout_minutes: settings.phase_timeout_minutes,
            line: line_of(text, heading.range.start),
        });
    }

    if phases.is_empty() {
        errors.push(err(
            1,
            "the plan has no phases; add at least one `## ` heading",
        ));
    }
    for phase in &phases {
        if phase.title.is_empty() {
            errors.push(err(
                phase.line,
                format!(
                    "phase {} has no title; write the title after `##` on its heading",
                    phase.number
                ),
            ));
        }
        if phase.body.is_empty() {
            errors.push(err(
                phase.line,
                format!(
                    "phase {} (\"{}\") has no instructions; write what the agent must do under its heading",
                    phase.number, phase.title
                ),
            ));
        }
    }
    let mut seen: Vec<(String, usize)> = Vec::new();
    for phase in phases.iter().filter(|p| !p.title.is_empty()) {
        let key = phase.title.to_lowercase();
        match seen.iter().find(|(k, _)| *k == key) {
            Some((_, first)) => errors.push(err(
                phase.line,
                format!(
                    "duplicate phase title \"{}\" (first used at line {first}); titles must be unique",
                    phase.title
                ),
            )),
            None => seen.push((key, phase.line)),
        }
    }

    if errors.is_empty() {
        Ok((h1_title, context, phases))
    } else {
        Err(errors)
    }
}

/// Settings read from a phase's `phasewise` block.
#[derive(Default)]
struct PhaseSettings {
    agent: Option<AgentKind>,
    model: Option<String>,
    effort: Option<String>,
    phase_timeout_minutes: Option<u64>,
}

/// Parses the TOML of a `phasewise` block whose fence is at `fence_line`.
fn parse_phase_settings(content: &str, fence_line: usize) -> Result<PhaseSettings, Vec<PlanError>> {
    let raw: RawPhaseSettings = match toml::from_str(content) {
        Ok(raw) => raw,
        Err(e) => {
            let offset = e.span().map_or(0, |span| span.start).min(content.len());
            let in_block = content.as_bytes()[..offset]
                .iter()
                .filter(|b| **b == b'\n')
                .count();
            return Err(vec![err(
                fence_line + 1 + in_block,
                format!("in the `phasewise` block: {}", e.message()),
            )]);
        }
    };
    let mut errors = Vec::new();
    let mut agent = None;
    if let Some(value) = raw.agent {
        match value.parse() {
            Ok(a) => agent = Some(a),
            Err(m) => {
                let line = key_line(content, 1, content.lines().count(), "agent");
                errors.push(err(fence_line + line, m));
            }
        }
    }
    let mut phase_timeout_minutes = None;
    if let Some(v) = raw.phase_timeout_minutes {
        let key = "phase_timeout_minutes";
        let line = fence_line + key_line(content, 1, content.lines().count(), key);
        if v < 1 {
            errors.push(err(line, format!("`{key}` must be at least 1")));
        } else if v > MAX_TIMEOUT_SECONDS / 60 {
            errors.push(err(
                line,
                format!(
                    "`{key}` must be at most {} (one year)",
                    MAX_TIMEOUT_SECONDS / 60
                ),
            ));
        } else {
            phase_timeout_minutes = Some(v);
        }
    }
    let mut out = [None, None];
    for (i, (key, value)) in [("model", raw.model), ("effort", raw.effort)]
        .into_iter()
        .enumerate()
    {
        if let Some(value) = value {
            match plan_value(key, &value) {
                Ok(v) => out[i] = Some(v),
                Err(m) => {
                    let line = key_line(content, 1, content.lines().count(), key);
                    errors.push(err(fence_line + line, m));
                }
            }
        }
    }
    if errors.is_empty() {
        let [model, effort] = out;
        Ok(PhaseSettings {
            agent,
            model,
            effort,
            phase_timeout_minutes,
        })
    } else {
        Err(errors)
    }
}

fn commands(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Plan {
        parse(text).unwrap_or_else(|e| panic!("unexpected errors: {e:?}"))
    }

    fn errs(text: &str) -> Vec<(usize, String)> {
        match parse(text) {
            Ok(_) => panic!("expected errors"),
            Err(es) => es.into_iter().map(|e| (e.line, e.message)).collect(),
        }
    }

    #[test]
    fn defaults_without_front_matter() {
        let plan = ok("## One\n\nDo it.\n");
        assert_eq!(plan.config, PlanConfig::default());
        assert_eq!(plan.config.agent, AgentKind::Claude);
        assert_eq!(plan.config.max_retries, 1);
        assert_eq!(plan.config.phase_timeout_minutes, 60);
        assert_eq!(plan.config.verify_timeout_seconds, 600);
        assert_eq!(plan.config.max_cost_usd, None);
        assert_eq!(plan.title, None);
        assert_eq!(plan.phases.len(), 1);
    }

    #[test]
    fn phase_block_sets_agent() {
        let plan =
            ok("## A\n\nx\n\n```phasewise\nagent = \"codex\"\nmodel = \"m\"\n```\n\n## B\n\ny\n");
        assert_eq!(plan.phases[0].agent, Some(AgentKind::Codex));
        assert_eq!(plan.phases[0].model.as_deref(), Some("m"));
        assert_eq!(plan.phases[1].agent, None);
    }

    #[test]
    fn phase_block_bad_agent_reports_line() {
        let es = errs("## A\n\nx\n\n```phasewise\nmodel = \"m\"\nagent = \"gpt\"\n```\n");
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].0, 7);
        assert!(es[0].1.contains("unknown agent `gpt`"), "{}", es[0].1);
    }

    #[test]
    fn agent_kind_display_and_from_str() {
        assert_eq!(AgentKind::Codex.to_string(), "codex");
        assert_eq!("claude".parse::<AgentKind>(), Ok(AgentKind::Claude));
        assert!("gpt".parse::<AgentKind>().is_err());
    }

    #[test]
    fn full_front_matter() {
        let plan = ok(
            "+++\nagent = \"codex\"\nverify = [\"cargo test\"]\nmax_retries = 3\nphase_timeout_minutes = 5\nverify_timeout_seconds = 7\nagent_args = [\"--x\"]\n+++\n## A\n\nbody\n",
        );
        assert_eq!(plan.config.agent, AgentKind::Codex);
        assert_eq!(plan.config.verify, vec!["cargo test"]);
        assert_eq!(plan.config.max_retries, 3);
        assert_eq!(plan.config.phase_timeout_minutes, 5);
        assert_eq!(plan.config.verify_timeout_seconds, 7);
        assert_eq!(plan.config.agent_args, vec!["--x"]);
    }

    #[test]
    fn the_plan_records_whether_it_names_an_agent() {
        assert!(!ok("## A\n\nbody\n").config.agent_set);
        assert!(
            ok("+++\nagent = \"claude\"\n+++\n## A\n\nbody\n")
                .config
                .agent_set
        );
    }

    #[test]
    fn a_nul_byte_in_the_model_or_the_agent_args_is_a_plan_error() {
        let e = errs("+++\nmodel = \"a\\u0000b\"\n+++\n## A\n\nbody\n");
        assert!(e[0].1.contains("control characters"), "{e:?}");
        let e = errs("+++\nagent_args = [\"--x\\u0000\"]\n+++\n## A\n\nbody\n");
        assert!(e[0].1.contains("NUL byte"), "{e:?}");
        assert_eq!(e[0].0, 2, "{e:?}");
    }

    #[test]
    fn front_matter_only_on_line_one() {
        // `+++` on line 2 is not front matter: it is plain body text.
        let plan = ok("\n+++\nagent = \"codex\"\n+++\n## A\n\nbody\n");
        assert_eq!(plan.config.agent, AgentKind::Claude);
        assert!(plan.context.contains("agent = \"codex\""));
    }

    #[test]
    fn unterminated_front_matter() {
        let e = errs("+++\nagent = \"claude\"\n## A\n\nbody\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, 1);
    }

    #[test]
    fn unknown_key_names_the_key() {
        let e = errs("+++\nagent = \"claude\"\nbogus = 1\n+++\n## A\n\nbody\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, 3);
        assert!(e[0].1.contains("bogus"), "{}", e[0].1);
        assert!(!e[0].1.contains('\n'));
    }

    #[test]
    fn toml_syntax_error_has_line() {
        let e = errs("+++\nagent = \"claude\"\nverify = [\n+++\n## A\n\nbody\n");
        assert_eq!(e.len(), 1);
        assert!(e[0].0 >= 3 && e[0].0 <= 4, "{e:?}");
        assert!(!e[0].1.contains('\n'));
    }

    #[test]
    fn toml_type_error_has_line() {
        let e = errs("+++\nmax_retries = \"two\"\n+++\n## A\n\nbody\n");
        assert_eq!(e[0].0, 2);
    }

    #[test]
    fn unknown_agent() {
        let e = errs("+++\nverify = []\nagent = \"gpt\"\n+++\n## A\n\nbody\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, 3);
        assert!(e[0].1.contains("claude") && e[0].1.contains("codex"));
    }

    #[test]
    fn timeouts_must_be_positive() {
        let e =
            errs("+++\nphase_timeout_minutes = 0\nverify_timeout_seconds = 0\n+++\n## A\n\nbody\n");
        assert_eq!(e.iter().map(|x| x.0).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn timeouts_have_an_upper_bound_of_a_year() {
        let year = MAX_TIMEOUT_SECONDS;
        let ok_plan = ok(&format!(
            "+++\nphase_timeout_minutes = {}\nverify_timeout_seconds = {year}\n+++\n## A\n\nbody\n",
            year / 60
        ));
        assert_eq!(ok_plan.config.verify_timeout_seconds, year);
        for (key, value) in [
            ("verify_timeout_seconds", year + 1),
            ("verify_timeout_seconds", i64::MAX as u64),
            ("phase_timeout_minutes", year / 60 + 1),
            ("phase_timeout_minutes", i64::MAX as u64),
        ] {
            let e = errs(&format!("+++\n{key} = {value}\n+++\n## A\n\nbody\n"));
            assert_eq!(e.len(), 1, "{key} = {value}: {e:?}");
            assert_eq!(e[0].0, 2);
            assert!(
                e[0].1.contains(key) && e[0].1.contains("at most"),
                "{}",
                e[0].1
            );
        }
    }

    #[test]
    fn the_spending_limit_is_a_positive_amount() {
        let plan = ok("+++\nmax_cost_usd = 1.5\n+++\n## A\n\nbody\n");
        assert_eq!(plan.config.max_cost_usd, Some(1.5));
        let plan = ok("+++\nmax_cost_usd = 2\n+++\n## A\n\nbody\n");
        assert_eq!(plan.config.max_cost_usd, Some(2.0));
        for bad in ["0", "-1.0"] {
            let e = errs(&format!("+++\nmax_cost_usd = {bad}\n+++\n## A\n\nbody\n"));
            assert_eq!(e.len(), 1, "{bad}");
            assert!(e[0].1.contains("max_cost_usd"), "{}", e[0].1);
        }
    }

    #[test]
    fn crlf_input() {
        let plan = ok(
            "+++\r\nagent = \"codex\"\r\n+++\r\n# T\r\n\r\nctx\r\n\r\n## A\r\n\r\nbody\r\n\r\n```verify\r\nmake\r\n```\r\n",
        );
        assert_eq!(plan.config.agent, AgentKind::Codex);
        assert_eq!(plan.title.as_deref(), Some("T"));
        assert_eq!(plan.context, "ctx");
        assert_eq!(plan.phases[0].title, "A");
        assert_eq!(plan.phases[0].body, "body");
        assert_eq!(plan.phases[0].verify, vec!["make"]);
        assert_eq!(plan.phases[0].line, 8);
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_front_matter() {
        let text = "+++\nagent = \"codex\"\nmax_retries = 3\n+++\n# T\n\nctx\n\n## A\n\nbody\n\n```verify\nmake\n```\n";
        let plain = ok(text);
        let marked = ok(&format!("\u{feff}{text}"));
        assert_eq!(marked.config.agent, AgentKind::Codex);
        assert_eq!(marked.config.max_retries, 3);
        assert_eq!(marked.title, plain.title);
        assert_eq!(marked.context, plain.context);
        assert_eq!(marked.phases[0].title, plain.phases[0].title);
        assert_eq!(marked.phases[0].body, plain.phases[0].body);
        assert_eq!(marked.phases[0].verify, plain.phases[0].verify);
        assert_eq!(marked.phases[0].line, plain.phases[0].line);
    }

    #[test]
    fn a_byte_order_mark_keeps_error_lines() {
        let e = errs("\u{feff}+++\nfoo = 1\n+++\n## A\n\nbody\n");
        assert_eq!(e[0].0, 2);
    }

    #[test]
    fn a_byte_order_mark_without_front_matter_is_ignored() {
        let plan = ok("\u{feff}# T\n\n## A\n\nbody\n");
        assert_eq!(plan.title.as_deref(), Some("T"));
        assert_eq!(plan.phases[0].line, 3);
    }

    #[test]
    fn crlf_unknown_key_line() {
        let e = errs("+++\r\nfoo = 1\r\n+++\r\n## A\r\n\r\nbody\r\n");
        assert_eq!(e[0].0, 2);
    }

    #[test]
    fn phases_numbered_by_order_with_lines() {
        let plan = ok("+++\nmax_retries = 0\n+++\n# T\n\n## First\n\na\n\n## Second\n\nb\n");
        assert_eq!(plan.phases[0].number, 1);
        assert_eq!(plan.phases[1].number, 2);
        assert_eq!(plan.phases[0].line, 6);
        assert_eq!(plan.phases[1].line, 10);
        assert_eq!(plan.phases[0].body, "a");
        assert_eq!(plan.phases[1].body, "b");
    }

    #[test]
    fn phase_prefix_stripped() {
        let plan = ok(
            "## Phase 1: Add toolchain\n\na\n\n## PHASE   7 :  Shout\n\nb\n\n## phase 3:x\n\nc\n\n## Phased out\n\nd\n",
        );
        let titles: Vec<_> = plan.phases.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["Add toolchain", "Shout", "x", "Phased out"]);
        assert_eq!(plan.phases[1].number, 2);
    }

    #[test]
    fn localized_prefix_stripped_at_position() {
        let plan = ok(
            "## Fase 1: Primer archivo vacío\n\na\n\n## Étape 2 : X\n\nb\n\n## Step 3 — Y\n\nc\n\n## Paso 4. Z\n\nd\n\n## Stage 5) W\n\ne\n\n## 6. Six\n\nf\n\n## 7) Seven\n\ng\n",
        );
        let titles: Vec<_> = plan.phases.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Primer archivo vacío", "X", "Y", "Z", "W", "Six", "Seven"]
        );
    }

    #[test]
    fn prefix_kept_when_it_is_not_a_phase_number() {
        let plan = ok(
            "## Python 3: migrate\n\na\n\n## Fase 9: X\n\nb\n\n## 2024 roadmap\n\nc\n\n## Fase 4:\n\nd\n\n## 5.x notes\n\ne\n",
        );
        let titles: Vec<_> = plan.phases.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Python 3: migrate",
                "Fase 9: X",
                "2024 roadmap",
                "Fase 4:",
                "5.x notes"
            ]
        );
    }

    #[test]
    fn strip_phase_prefix_table() {
        let cases: &[(&str, usize, &str)] = &[
            ("Python 3.12 upgrade", 3, "Python 3.12 upgrade"),
            ("Web 2.0", 2, "Web 2.0"),
            ("Node 18.x", 18, "Node 18.x"),
            ("Python 3: migrate", 3, "Python 3: migrate"),
            ("v2: X", 2, "v2: X"),
            ("Version 2: API", 2, "Version 2: API"),
            ("Windows 10 - x", 10, "Windows 10 - x"),
            ("Fase 2: Crear", 2, "Crear"),
            ("fase 2 - crear", 2, "crear"),
            ("ÉTAPE 3 — Tests", 3, "Tests"),
            ("Schritt 4) Aufräumen", 4, "Aufräumen"),
            ("Paso 1.5 ajustar", 1, "Paso 1.5 ajustar"),
            ("フェーズ1：設計", 1, "設計"),
            ("Fase 9: X", 2, "Fase 9: X"),
            ("Phase 7: X", 2, "X"),
            ("2. X", 2, "X"),
            ("2.0 release", 2, "2.0 release"),
            ("Fase 2:", 2, "Fase 2:"),
        ];
        for (title, position, expected) in cases {
            assert_eq!(
                strip_phase_prefix(title, *position),
                *expected,
                "{title:?} at {position}"
            );
        }
    }

    #[test]
    fn duplicate_check_uses_stripped_titles() {
        let errors = errs("## Fase 1: Same\n\na\n\n## Phase 2: same\n\nb\n");
        assert!(
            errors
                .iter()
                .any(|(_, m)| m.contains("duplicate phase title"))
        );
    }

    #[test]
    fn phase_prefix_alone_is_kept() {
        let plan = ok("## Phase 1:\n\na\n");
        assert_eq!(plan.phases[0].title, "Phase 1:");
    }

    #[test]
    fn setext_heading_opens_phase() {
        let plan = ok("Setext Title\n------------\n\nbody\n");
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].title, "Setext Title");
        assert_eq!(plan.phases[0].line, 1);
        assert_eq!(plan.phases[0].body, "body");
    }

    #[test]
    fn closing_hashes_removed() {
        let plan = ok("## Title ##\n\na\n\n## C#\n\nb\n");
        assert_eq!(plan.phases[0].title, "Title");
        assert_eq!(plan.phases[1].title, "C#");
    }

    #[test]
    fn heading_in_code_block_is_not_a_phase() {
        let plan = ok("## A\n\n```sh\n## not a heading\n```\n");
        assert_eq!(plan.phases.len(), 1);
        assert!(plan.phases[0].body.contains("## not a heading"));
    }

    #[test]
    fn title_and_context() {
        let plan =
            ok("+++\n+++\n\n# My plan\n\nShared **context**.\n\n### Notes\n\nmore\n\n## A\n\nx\n");
        assert_eq!(plan.title.as_deref(), Some("My plan"));
        assert_eq!(plan.context, "Shared **context**.\n\n### Notes\n\nmore");
    }

    #[test]
    fn only_first_h1_is_title() {
        let plan = ok("# One\n\n# Two\n\n## A\n\nx\n");
        assert_eq!(plan.title.as_deref(), Some("One"));
        assert!(plan.context.contains("# Two"));
    }

    #[test]
    fn h3_stays_in_body() {
        let plan = ok("## A\n\ntext\n\n### Sub\n\nmore\n");
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].body, "text\n\n### Sub\n\nmore");
    }

    #[test]
    fn verify_blocks_removed_and_parsed() {
        let plan = ok(
            "## A\n\nbefore\n\n```verify\n# comment\ncargo build\n\n  cargo test  \n```\n\nafter\n\n```sh\nkeep\n```\n",
        );
        let p = &plan.phases[0];
        assert_eq!(p.verify, vec!["cargo build", "cargo test"]);
        assert!(!p.body.contains("verify"));
        assert!(!p.body.contains("cargo"));
        assert!(p.body.contains("before") && p.body.contains("after"));
        assert!(p.body.contains("```sh\nkeep\n```"));
    }

    #[test]
    fn verify_label_must_be_exact() {
        let plan = ok("## A\n\n```verify extra\nx\n```\n\n```Verify\ny\n```\n");
        assert!(plan.phases[0].verify.is_empty());
        assert!(plan.phases[0].body.contains("```Verify"));
    }

    #[test]
    fn verify_before_first_phase_is_error() {
        let e = errs("# T\n\n```verify\nmake\n```\n\n## A\n\nx\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, 3);
    }

    #[test]
    fn no_phases() {
        let e = errs("# Title\n\njust text\n");
        assert_eq!(e.len(), 1);
        assert!(e[0].1.contains("no phases"));
    }

    #[test]
    fn empty_phase_body() {
        let e = errs("## A\n\n```verify\nmake\n```\n\n## B\n\nx\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, 1);
    }

    #[test]
    fn an_empty_phase_title_is_an_error_at_its_heading() {
        for heading in ["## ", "##", "## ##"] {
            let text = format!("# T\n\n{heading}\n\nbody\n");
            let e = errs(&text);
            assert_eq!(e.len(), 1, "{heading:?}: {e:?}");
            assert_eq!(e[0].0, 3, "{heading:?}");
            assert!(e[0].1.contains("phase 1 has no title"), "{}", e[0].1);
        }
    }

    #[test]
    fn two_empty_phase_titles_are_not_reported_as_duplicates() {
        let e = errs("## \n\na\n\n## \n\nb\n");
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!((e[0].0, e[1].0), (1, 5));
        assert!(e.iter().all(|(_, m)| m.contains("has no title")), "{e:?}");
    }

    #[test]
    fn duplicate_titles_case_insensitive() {
        let e = errs("## Same\n\na\n\n## Phase 2: SAME\n\nb\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, 5);
        assert!(e[0].1.contains("line 1"), "{}", e[0].1);
    }

    #[test]
    fn multiple_errors_sorted_by_line() {
        let e = errs(
            "+++\nagent = \"x\"\nphase_timeout_minutes = 0\n+++\n\n## A\n\n## B\n\nz\n\n## b\n\nw\n",
        );
        let lines: Vec<_> = e.iter().map(|x| x.0).collect();
        let mut sorted = lines.clone();
        sorted.sort_unstable();
        assert_eq!(lines, sorted);
        assert_eq!(lines, vec![2, 3, 6, 12]);
    }

    #[test]
    fn load_missing_file() {
        let e = load(Path::new("/nonexistent/phasewise.md")).unwrap_err();
        let msg = e.to_string();
        assert!(msg.starts_with("Plan file not found: /nonexistent/phasewise.md."));
        assert!(msg.contains("phasewise init"));
    }

    #[test]
    fn load_renders_path_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.md");
        std::fs::write(&path, "## A\n\n## B\n\nx\n").unwrap();
        let msg = load(&path).unwrap_err().to_string();
        assert!(msg.starts_with(&format!("{}:1: ", path.display())), "{msg}");
    }

    #[test]
    fn load_says_when_the_file_is_not_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.md");
        std::fs::write(&path, b"## A\n\ncaf\xe9\n").unwrap();
        let msg = load(&path).unwrap_err().to_string();
        assert!(msg.contains("is not valid UTF-8"), "{msg}");
        assert!(msg.contains("Save it as UTF-8"), "{msg}");
        assert!(!msg.contains("permissions"), "{msg}");
    }

    #[test]
    fn load_still_reports_a_file_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        // A directory exists but cannot be read as a file.
        let msg = load(dir.path()).unwrap_err().to_string();
        assert!(msg.contains("Check the path and its permissions"), "{msg}");
    }

    #[test]
    fn model_and_effort_validation() {
        assert!(validate_model("opus[1m]").is_ok());
        assert!(validate_model("gpt-5.1-codex").is_ok());
        assert!(validate_model(&"a".repeat(100)).is_ok());
        assert!(validate_model("").is_err());
        assert!(validate_model(&"a".repeat(101)).is_err());
        assert!(validate_model("two words").is_err());
        assert!(validate_model("tab\tname").is_err());
        assert!(validate_model("-flag").unwrap_err().contains("flag"));
        for e in EFFORTS {
            assert!(validate_effort(e).is_ok());
        }
        assert!(
            validate_effort("extreme")
                .unwrap_err()
                .contains("low, medium")
        );
        assert!(validate_effort("").is_err());
        assert!(validate_effort("default").is_err());
    }

    #[test]
    fn front_matter_model_and_effort() {
        let plan = ok("+++\nmodel = \"opus\"\neffort = \"xhigh\"\n+++\n## A\n\nbody\n");
        assert_eq!(plan.config.model.as_deref(), Some("opus"));
        assert_eq!(plan.config.effort.as_deref(), Some("xhigh"));
        assert_eq!(plan.phases[0].model, None);
        let es =
            errs("+++\nagent = \"claude\"\nmodel = \"-x\"\neffort = \"huge\"\n+++\n## A\n\nbody\n");
        assert_eq!(es.len(), 2, "{es:?}");
        assert_eq!(es[0].0, 3);
        assert!(es[0].1.contains("must not start with"), "{es:?}");
        assert_eq!(es[1].0, 4);
        assert!(es[1].1.contains("unknown effort"), "{es:?}");
    }

    #[test]
    fn default_is_an_error_in_a_plan() {
        let es = errs("+++\nmodel = \"default\"\n+++\n## A\n\nbody\n");
        assert_eq!(es[0].0, 2);
        assert!(es[0].1.contains("remove the `model` key"), "{es:?}");
        let es = errs("## A\n\nbody\n\n```phasewise\neffort = \"default\"\n```\n");
        assert_eq!(es[0].0, 6);
        assert!(es[0].1.contains("remove the `effort` key"), "{es:?}");
    }

    #[test]
    fn phase_settings_block_is_parsed_and_removed_from_the_body() {
        let plan = ok(
            "## A\n\nBefore.\n\n```phasewise\nmodel = \"sonnet\"\neffort = \"low\"\n```\n\nAfter.\n\n```verify\ntrue\n```\n\n## B\n\nplain\n",
        );
        assert_eq!(plan.phases[0].model.as_deref(), Some("sonnet"));
        assert_eq!(plan.phases[0].effort.as_deref(), Some("low"));
        assert_eq!(plan.phases[0].verify, vec!["true"]);
        let body = &plan.phases[0].body;
        assert!(
            body.contains("Before.") && body.contains("After."),
            "{body}"
        );
        assert!(
            !body.contains("phasewise") && !body.contains("sonnet"),
            "{body}"
        );
        assert_eq!(plan.phases[1].model, None);
        // A block with only one key is fine.
        let plan = ok("## A\n\nx\n\n```phasewise\neffort = \"max\"\n```\n");
        assert_eq!(plan.phases[0].model, None);
        assert_eq!(plan.phases[0].effort.as_deref(), Some("max"));
    }

    #[test]
    fn phase_settings_block_sets_its_own_timeout() {
        let max = MAX_TIMEOUT_SECONDS / 60;
        let plan = ok(&format!(
            "+++\nphase_timeout_minutes = 5\n+++\n## A\n\nx\n\n```phasewise\nphase_timeout_minutes = {max}\n```\n\n## B\n\ny\n"
        ));
        assert_eq!(plan.config.phase_timeout_minutes, 5);
        assert_eq!(plan.phases[0].phase_timeout_minutes, Some(max));
        assert_eq!(plan.phases[1].phase_timeout_minutes, None);
    }

    #[test]
    fn phase_settings_block_timeout_is_validated() {
        let block = |value: &str| {
            format!(
                "## A\n\nx\n\n```phasewise\nmodel = \"m\"\nphase_timeout_minutes = {value}\n```\n"
            )
        };
        let es = errs(&block("0"));
        assert_eq!(es.len(), 1, "{es:?}");
        assert_eq!(es[0].0, 7);
        assert!(es[0].1.contains("at least 1"), "{es:?}");
        let es = errs(&block(&(MAX_TIMEOUT_SECONDS / 60 + 1).to_string()));
        assert_eq!(es[0].0, 7);
        assert!(es[0].1.contains("at most 525600"), "{es:?}");
        let es = errs(&block("\"soon\""));
        assert_eq!(es[0].0, 7);
        let es = errs(&block("1.5"));
        assert_eq!(es[0].0, 7);
    }

    #[test]
    fn phase_settings_block_errors_have_line_numbers() {
        // Unknown key, named; line of the key.
        let es = errs("## A\n\nx\n\n```phasewise\nmodel = \"m\"\ntemperature = 1\n```\n");
        assert_eq!(es.len(), 1, "{es:?}");
        assert_eq!(es[0].0, 7);
        assert!(es[0].1.contains("temperature"), "{es:?}");
        // Invalid value; line of the key.
        let es = errs("## A\n\nx\n\n```phasewise\nmodel = \"m\"\neffort = \"huge\"\n```\n");
        assert_eq!(es[0].0, 7);
        assert!(es[0].1.contains("unknown effort"), "{es:?}");
        // Block before the first phase.
        let es = errs("```phasewise\nmodel = \"m\"\n```\n\n## A\n\nx\n");
        assert_eq!(es[0].0, 1);
        assert!(es[0].1.contains("after a phase heading"), "{es:?}");
        // Second block in one phase; the first one is at line 5.
        let es = errs(
            "## A\n\nx\n\n```phasewise\nmodel = \"a\"\n```\n\n```phasewise\nmodel = \"b\"\n```\n",
        );
        assert_eq!(es.len(), 1, "{es:?}");
        assert_eq!(es[0].0, 9);
        assert!(
            es[0].1.contains("only one") && es[0].1.contains("line 5"),
            "{es:?}"
        );
        // Blocks in different phases are fine.
        ok(
            "## A\n\nx\n\n```phasewise\nmodel = \"a\"\n```\n\n## B\n\ny\n\n```phasewise\nmodel = \"b\"\n```\n",
        );
        // Invalid TOML.
        let es = errs("## A\n\nx\n\n```phasewise\nmodel = \n```\n");
        assert_eq!(es[0].0, 6);
        assert!(es[0].1.starts_with("in the `phasewise` block"), "{es:?}");
    }
}
