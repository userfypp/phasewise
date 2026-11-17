//! Property tests for the plan parser: it never panics, what it reports points
//! inside the file, and a rendered plan parses back to what was rendered.

use phasewise::plan::{self, Plan};
use proptest::prelude::*;

/// A fixed number of cases keeps the run fast and the same on every machine.
fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

/// Lines that matter to the parser, mixed with ordinary text.
const FRAGMENTS: &[&str] = &[
    "+++",
    "+++ ",
    "# Title",
    "Title\n=====",
    "## Phase 1: Build",
    "## Second",
    "## ",
    "##",
    "Setup\n-----",
    "---",
    "```verify",
    "```phasewise",
    "```",
    "~~~",
    "```verify extra",
    "cargo test",
    "# a comment",
    "model = \"opus\"",
    "agent = \"codex\"",
    "verify = [\"x\"]",
    "max_retries = -1",
    "phase_timeout_minutes = 0",
    "> ## quoted",
    "> ```verify",
    "- item",
    "    indented",
    "text",
    "é ü 日本語",
    "## 日本語日本",
    "## ééé 1: é",
    "## Phasé 1: x",
    "\u{feff}",
    "",
];

/// Plan-like text: lines from `FRAGMENTS`, joined with LF or CRLF, perhaps
/// starting with a byte order mark.
fn plan_like() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(prop::sample::select(FRAGMENTS), 0..40),
        prop::bool::ANY,
        prop::bool::ANY,
        prop::bool::ANY,
    )
        .prop_map(|(lines, crlf, bom, final_newline)| {
            let eol = if crlf { "\r\n" } else { "\n" };
            let mut text = String::new();
            if bom {
                text.push('\u{feff}');
            }
            text.push_str(&lines.join(eol));
            if final_newline {
                text.push_str(eol);
            }
            text
        })
}

fn line_count(text: &str) -> usize {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .count()
}

/// What a plan the parser accepted must satisfy, whatever the input was.
fn assert_valid_plan(plan: &Plan, text: &str) {
    let lines = line_count(text);
    assert!(!plan.phases.is_empty(), "no phases in {text:?}");
    let mut previous_line = 0;
    for (i, phase) in plan.phases.iter().enumerate() {
        assert_eq!(phase.number, i + 1, "numbering in {text:?}");
        assert!(!phase.title.is_empty(), "empty title in {text:?}");
        assert!(!phase.body.is_empty(), "empty body in {text:?}");
        assert!(
            (1..=lines).contains(&phase.line),
            "phase line {} outside 1..={lines} in {text:?}",
            phase.line
        );
        assert!(
            phase.line > previous_line,
            "lines not increasing in {text:?}"
        );
        previous_line = phase.line;
    }
}

fn assert_errors_inside_file(errors: &[plan::PlanError], text: &str) {
    let lines = line_count(text).max(1);
    assert!(!errors.is_empty(), "Err without errors for {text:?}");
    for e in errors {
        assert!(
            (1..=lines).contains(&e.line),
            "error line {} outside 1..={lines}: {} (in {text:?})",
            e.line,
            e.message
        );
        assert!(!e.message.is_empty(), "empty message for {text:?}");
    }
    assert!(
        errors.windows(2).all(|w| w[0].line <= w[1].line),
        "errors not sorted by line for {text:?}"
    );
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn arbitrary_text_never_panics(text in any::<String>()) {
        let _ = plan::parse(&text);
    }

    #[test]
    fn plan_like_text_never_panics_and_reports_lines_inside_the_file(text in plan_like()) {
        match plan::parse(&text) {
            Ok(plan) => assert_valid_plan(&plan, &text),
            Err(errors) => assert_errors_inside_file(&errors, &text),
        }
    }
}

/// A phase as the test renders it.
#[derive(Debug, Clone)]
struct Spec {
    title: String,
    body: Vec<String>,
    verify: Vec<String>,
    numbered: bool,
}

fn phase_specs() -> impl Strategy<Value = Vec<Spec>> {
    prop::collection::vec(
        (
            "[A-Za-z]{1,8}( [A-Za-z]{1,8}){0,2}",
            prop::collection::vec("[A-Za-z][A-Za-z ,.]{0,30}[A-Za-z.]", 1..4),
            prop::collection::vec("[a-z][a-z0-9]{0,8}( -?[a-z0-9]{1,6}){0,3}", 0..4),
            prop::bool::ANY,
        ),
        1..6,
    )
    .prop_map(|phases| {
        phases
            .into_iter()
            .enumerate()
            .map(|(i, (title, body, verify, numbered))| Spec {
                // A letter suffix keeps the titles different without digits,
                // which could make a title look like a numbered prefix.
                title: format!("{title} {}", char::from(b'a' + i as u8)),
                body,
                verify,
                numbered,
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn a_rendered_plan_parses_back_to_the_same_phases(
        specs in phase_specs(),
        context in prop::collection::vec("[A-Za-z][A-Za-z ,.]{0,30}[A-Za-z.]", 0..3),
        title in prop::option::of("[A-Za-z]{1,8}( [A-Za-z]{1,8}){0,2}"),
        shared_verify in prop::collection::vec("[a-z][a-z0-9]{0,8}", 0..3),
        crlf in prop::bool::ANY,
    ) {
        let eol = if crlf { "\r\n" } else { "\n" };
        let mut lines: Vec<String> = Vec::new();
        if !shared_verify.is_empty() {
            let list: Vec<String> = shared_verify.iter().map(|c| format!("\"{c}\"")).collect();
            lines.extend(["+++".into(), format!("verify = [{}]", list.join(", ")), "+++".into()]);
        }
        if let Some(title) = &title {
            lines.extend([format!("# {title}"), String::new()]);
        }
        for line in &context {
            lines.extend([line.clone(), String::new()]);
        }
        let mut expected_lines = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            expected_lines.push(lines.len() + 1);
            if spec.numbered {
                lines.push(format!("## Phase {}: {}", i + 1, spec.title));
            } else {
                lines.push(format!("## {}", spec.title));
            }
            lines.push(String::new());
            for line in &spec.body {
                lines.extend([line.clone(), String::new()]);
            }
            if !spec.verify.is_empty() {
                lines.push("```verify".into());
                lines.extend(spec.verify.iter().cloned());
                lines.push("```".into());
                lines.push(String::new());
            }
        }
        let text = lines.join(eol);

        let plan = match plan::parse(&text) {
            Ok(plan) => plan,
            Err(errors) => return Err(TestCaseError::fail(format!("{errors:?} for {text:?}"))),
        };
        prop_assert_eq!(&plan.config.verify, &shared_verify);
        prop_assert_eq!(&plan.title, &title);
        prop_assert_eq!(&plan.context.replace("\r\n", "\n"), &context.join("\n\n"));
        prop_assert_eq!(plan.phases.len(), specs.len());
        for (i, (phase, spec)) in plan.phases.iter().zip(&specs).enumerate() {
            prop_assert_eq!(phase.number, i + 1);
            prop_assert_eq!(&phase.title, &spec.title);
            prop_assert_eq!(&phase.body.replace("\r\n", "\n"), &spec.body.join("\n\n"));
            prop_assert_eq!(&phase.verify, &spec.verify);
            prop_assert_eq!(phase.line, expected_lines[i]);
        }
    }
}
