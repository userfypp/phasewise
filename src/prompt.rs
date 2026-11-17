//! The one place with the agent prompt texts. Pure functions.

use crate::agent::ModelInfo;
use crate::plan::{AgentKind, Plan};

const MAX_SUMMARY_CHARS: usize = 200;

/// Builds the prompt of one phase. `completed` holds (number, title, one-line
/// summary) of the phases already done; `note` is an optional reviewer note.
pub fn phase_prompt(
    plan: &Plan,
    phase_number: usize,
    completed: &[(usize, String, String)],
    verify: &[String],
    note: Option<&str>,
) -> String {
    let plan_title = plan.title.as_deref().unwrap_or("(untitled)");
    let total = plan.phases.len();
    let (title, body) = plan
        .phases
        .iter()
        .find(|p| p.number == phase_number)
        .map(|p| (p.title.as_str(), p.body.as_str()))
        .unwrap_or(("", ""));

    let completed_text = if completed.is_empty() {
        "None.".to_string()
    } else {
        completed
            .iter()
            .map(|(n, t, s)| format!("- Phase {n}: {t} — {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let verify_text = if verify.is_empty() {
        "(none)".to_string()
    } else {
        verify
            .iter()
            .map(|c| format!("  {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut out = format!(
        "You are executing one phase of a larger plan. The plan is orchestrated by phasewise,\n\
which runs phases one at a time and checkpoints the repository after each one.\n\
\n\
## Plan: {plan_title}\n\
\n\
{context}\n\
\n\
## Phases already completed\n\
{completed_text}\n\
\n\
## Your task: phase {phase_number} of {total} — {title}\n\
\n\
{body}\n\
\n\
## Rules\n\
- Do only this phase. Do not start work that belongs to later phases.\n\
- Do not create git commits, branches or tags, and never push. phasewise checkpoints your work.\n\
- After you finish, these commands will be run and must succeed:\n\
{verify_text}\n\
- End with a short summary of what you changed.\n",
        context = plan.context,
    );
    if let Some(note) = note {
        out.push_str(&format!("\n## Note from the reviewer\n{note}\n"));
    }
    out
}

/// Report of a failed verification command, sent back to the agent.
pub fn verify_failure_report(
    command: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    timeout_secs: u64,
    too_much_output: bool,
    output_tail: &str,
) -> String {
    let what = if timed_out {
        format!("timed out after {timeout_secs} s")
    } else if too_much_output {
        format!("was stopped: {}", crate::verify::output_limit_text())
    } else {
        match exit_code {
            Some(code) => format!("failed with exit code {code}"),
            None => "was terminated by a signal".to_string(),
        }
    };
    format!(
        "The verification command `{command}` {what}.\n\nLast lines of its output:\n```\n{}\n```\n",
        output_tail.trim_end()
    )
}

/// Prompt sent when continuing the same session after a failed verification.
pub fn resume_prompt(report: &str) -> String {
    format!(
        "Verification failed after your last changes.\n\n{report}\nFix the problem so the verification commands pass, then end with a short summary of what you changed.\n"
    )
}

/// Prompt for a new session when no session id is known: the phase prompt plus
/// the report of the previous failed attempt.
pub fn fresh_retry_prompt(phase_prompt: &str, report: &str) -> String {
    format!("{phase_prompt}\n## Previous attempt failed verification\n{report}")
}

/// First non-empty line of the summary, at most 200 characters, or
/// `(no summary)`.
pub fn one_line_summary(summary: Option<&str>) -> String {
    let line = summary
        .and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    if line.is_empty() {
        return "(no summary)".to_string();
    }
    line.chars().take(MAX_SUMMARY_CHARS).collect()
}

/// Line that opens the plan in the agent's answer.
pub const PLAN_START: &str = "<<<PHASEWISE-PLAN";
/// Line that closes the plan in the agent's answer.
pub const PLAN_END: &str = "PHASEWISE-PLAN>>>";

/// Prompt of `phasewise plan`: the agent reads the project and answers with a
/// complete plan between the marker lines. `models` is the agent's model list,
/// or `None` when it could not be listed.
pub fn plan_prompt(goal: &str, agent: AgentKind, models: Option<&[ModelInfo]>) -> String {
    let models_text = match models {
        Some(models) if !models.is_empty() => {
            let lines: Vec<String> = models
                .iter()
                .map(|m| {
                    let mut line = format!("- {}", m.id);
                    if !m.description.is_empty() {
                        line.push_str(&format!(": {}", m.description));
                    }
                    if !m.efforts.is_empty() {
                        line.push_str(&format!(" (efforts: {})", m.efforts.join(", ")));
                    }
                    line
                })
                .collect();
            lines.join("\n")
        }
        _ => "The list of models could not be read; any model name accepted by the agent's CLI \
works. Use only names you are sure exist."
            .to_string(),
    };
    // The example shows the choice being made: the last listed model for the easy
    // phase and the first for the hard one, when the list has at least two.
    let (easy_settings, hard_settings) = match models {
        Some([first, .., last]) => (
            format!("model = \"{}\"\neffort = \"low\"", last.id),
            format!("model = \"{}\"\neffort = \"high\"", first.id),
        ),
        _ => (
            "effort = \"low\"".to_string(),
            "effort = \"high\"".to_string(),
        ),
    };
    format!(
        r#"You are writing an implementation plan for phasewise. phasewise is a tool that runs a
Markdown plan with a coding agent ({agent}) one phase at a time. For each phase it starts a fresh
agent session with only that phase's instructions, runs the phase's checks (shell commands that must
exit 0), checkpoints the repository with a git commit, and stops so a human can review before the
next phase starts.

## The goal

{goal}

## Your job

1. Explore the project first (read the build files, the layout and the code the goal touches).
   You are read-only: never create, edit or delete files, and do not build, test or install anything.
   Only read and search. Read-only commands such as `ls`, `cat` or `rg` are fine if your tools allow them.
2. Write the plan as ONE Markdown file in the phasewise format described below.
3. Answer with the complete plan between the marker lines described at the end.

## How to write good phases

- Usually 3 to 10 phases, in the order they must be done. Each one is small enough for a human to review.
- Every phase leaves the project in a working state (it builds, tests pass).
- Every phase has concrete checks: commands that really exist in this project (look at the build files,
  scripts and CI config). No destructive checks and no checks that use the network.
- The instructions of a phase are self-contained. The agent of a phase does not see the other phases'
  instructions, only a one-line summary of each completed phase. Name the files, functions and
  conventions it needs.
- Write the text of the phases in the language of the goal above.
- Do not tell the agent to commit, create branches or push; phasewise checkpoints the work.

## Plan format

The file has optional TOML front matter, an optional title and shared context, then phases.

Front matter: only counts if line 1 is exactly `+++`; it ends at the next line that is exactly `+++`.
Unknown keys are errors. Every key is optional:

- agent (string, default "claude"): "claude" or "codex". You MUST set `agent = "{agent}"`.
- verify (array of strings, default []): commands run after EVERY phase.
- max_retries (integer >= 0, default 1): automatic retries when a check fails.
- phase_timeout_minutes (integer >= 1, default 60): time limit for the agent in one phase.
- verify_timeout_seconds (integer >= 1, default 600): time limit for each check command.
- max_cost_usd (number > 0, unset by default): spending limit for the whole run. Leave it unset.
- agent_args (array of strings, default []): extra arguments for the agent command line. Leave it unset.
- model (string, unset by default): model for every phase.
- effort (string, unset by default): low, medium, high, xhigh or max.

Body:

- An optional level-1 heading (`# Title`) is the plan title. Everything between the front matter and
  the first phase is the shared context, sent to the agent in every phase: what the project is, the
  conventions and anything true for the whole plan.
- Every level-2 heading (`## Phase 1: Title`) opens a phase. A leading `Phase <number>:` is removed
  from the title, and so is a word for "phase" or "step" in common languages plus the phase's own
  number (`Fase 2: X`, `Étape 2 — X`, `2. X`). Titles must be unique (ignoring case). Level-3 and
  deeper headings stay inside the phase body. A `##` inside a fenced code block is not a heading.
- Each phase body is the instructions for the agent and must not be empty.
- A fenced block whose info string is exactly `verify` lists the checks of that phase, one command per
  line (empty lines and lines starting with `#` are ignored). They run after the global `verify`
  commands. It must come after a phase heading.
- A fenced block whose info string is exactly `phasewise` sets `model` and/or `effort` for that phase,
  in TOML. Only those two keys are allowed, at most one block per phase, after a phase heading.
  `default` is not a valid value: leave the key out instead.

Example:

~~~
+++
agent = "{agent}"
verify = ["cargo test"]
+++

# Add a stats subcommand

Shared context: a clap-based Rust CLI. Do not add dependencies.

## Phase 1: Define the subcommand

Add `stats` with a `--json` flag that prints `not implemented`.

```phasewise
{easy_settings}
```

```verify
cargo run -q -- stats --help
```

## Phase 2: Implement it

Print a summary, or one JSON object with `--json`.

```phasewise
{hard_settings}
```
~~~

## Models

{models_text}

Choose the model and the effort of every phase yourself, and write them in that phase's `phasewise`
block, so the person reviewing the plan sees the choice. The aim is the cheapest model and the lowest
effort that will do the phase reliably:

- Mechanical or well-specified work (renames, boilerplate, documentation, formatting, a change the
  instructions spell out file by file): the fastest, cheapest model and a low effort.
- Ordinary implementation and tests inside code the instructions describe: a capable mid-range model
  and a medium effort.
- Work that needs design or deep reasoning (architecture, concurrency, security, data migrations,
  a bug whose cause is unknown, changes that touch many modules): the strongest model and a high effort.

Use only model names from the list above, and only efforts that the chosen model supports when the list
shows them. Within a tier prefer the newest model: skip one the list describes as older unless it is
the only one that fits. Do not set `model` or `effort` in the front matter (it would apply to every phase); use
the `phasewise` block of each phase. A phase for which the agent's own default is the right choice may
leave the block out.

## Output protocol

Your final answer must contain the complete plan, and only the plan, between a line that is exactly
`{PLAN_START}` and a line that is exactly `{PLAN_END}`. For example:

{PLAN_START}
+++
agent = "{agent}"
+++

## Phase 1: ...
...
{PLAN_END}

Put nothing inside the markers except the plan file. You may write a short note before the first marker.
"#
    )
}

/// Prompt that sends the problems of a draft back to the same session.
pub fn plan_repair_prompt(problems: &[String]) -> String {
    let list = problems
        .iter()
        .map(|p| format!("- {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "The plan you wrote cannot be used. Problems found:\n{list}\n\n\
Fix them and answer again with the COMPLETE corrected plan, and only the plan, between a line that is \
exactly `{PLAN_START}` and a line that is exactly `{PLAN_END}`. Remember that you are read-only: do not \
modify any file.\n"
    )
}

/// Prompt for a new session when there is no session to resume: the original
/// prompt plus the problems of the previous answer.
pub fn plan_retry_prompt(original: &str, problems: &[String]) -> String {
    format!(
        "{original}\n## Your previous answer was rejected\n{}",
        plan_repair_prompt(problems)
    )
}

/// The plan between the last `PLAN_START` line and the next `PLAN_END` line
/// (lines are matched after trimming). `None` without a complete pair.
pub fn extract_plan(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().rposition(|l| l.trim() == PLAN_START)?;
    let len = lines[start + 1..]
        .iter()
        .position(|l| l.trim() == PLAN_END)?;
    let body = &lines[start + 1..start + 1 + len];
    let mut out = body
        .iter()
        .map(|l| l.trim_end_matches('\r'))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    Some(out)
}

/// The text inside one code fence that wraps the whole draft (an opening line
/// of three or more backticks or tildes, optionally `markdown` or `md`, and
/// the same fence as the last non-empty line). Some models add it around the
/// plan. `None` if the draft is not wrapped that way.
pub fn unwrap_fence(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let first = lines.iter().position(|l| !l.trim().is_empty())?;
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    if last <= first {
        return None;
    }
    let open = lines[first].trim();
    let fence_char = open.chars().next()?;
    if fence_char != '`' && fence_char != '~' {
        return None;
    }
    // The fence characters are ASCII, so the count is also a byte index.
    let fence_len = open.chars().take_while(|&c| c == fence_char).count();
    if fence_len < 3 {
        return None;
    }
    let info = open[fence_len..].trim();
    let info_ok =
        info.is_empty() || info.eq_ignore_ascii_case("markdown") || info.eq_ignore_ascii_case("md");
    if !info_ok || lines[last].trim() != &open[..fence_len] {
        return None;
    }
    let mut out = lines[first + 1..last].join("\n");
    out.push('\n');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::parse;

    fn small_plan() -> Plan {
        let text = "# Demo\n\nShared context.\n\n## Phase 1: First\n\nDo the first thing.\n\n## Phase 2: Second\n\nDo the second thing.\n";
        parse(text).expect("plan parses")
    }

    #[test]
    fn prompt_snapshot() {
        let plan = small_plan();
        let completed = vec![(1, "First".to_string(), "Created a.txt".to_string())];
        let verify = vec!["cargo test".to_string(), "test -f a.txt".to_string()];
        let got = phase_prompt(&plan, 2, &completed, &verify, None);
        let want =
            "You are executing one phase of a larger plan. The plan is orchestrated by phasewise,
which runs phases one at a time and checkpoints the repository after each one.

## Plan: Demo

Shared context.

## Phases already completed
- Phase 1: First — Created a.txt

## Your task: phase 2 of 2 — Second

Do the second thing.

## Rules
- Do only this phase. Do not start work that belongs to later phases.
- Do not create git commits, branches or tags, and never push. phasewise checkpoints your work.
- After you finish, these commands will be run and must succeed:
  cargo test
  test -f a.txt
- End with a short summary of what you changed.
";
        assert_eq!(got, want);
    }

    #[test]
    fn prompt_defaults_and_note() {
        let mut plan = small_plan();
        plan.title = None;
        let got = phase_prompt(&plan, 1, &[], &[], Some("Use tabs."));
        assert!(got.contains("## Plan: (untitled)\n"));
        assert!(got.contains("## Phases already completed\nNone.\n"));
        assert!(got.contains("must succeed:\n(none)\n- End with"));
        assert!(got.ends_with("\n## Note from the reviewer\nUse tabs.\n"));
    }

    #[test]
    fn failure_report_variants() {
        let r = verify_failure_report("make", Some(2), false, 600, false, "boom\n");
        assert!(r.contains("`make` failed with exit code 2"));
        assert!(r.contains("```\nboom\n```"));
        let r = verify_failure_report("make", None, true, 30, false, "");
        assert!(r.contains("timed out after 30 s"));
        let r = verify_failure_report("make", None, false, 30, false, "x");
        assert!(r.contains("terminated by a signal"));
        let r = verify_failure_report("make", None, false, 30, true, "x");
        assert!(
            r.contains("`make` was stopped: its output went over 64 MiB."),
            "{r}"
        );
    }

    #[test]
    fn retry_prompts_include_report() {
        let report = verify_failure_report("make", Some(1), false, 1, false, "err");
        assert!(resume_prompt(&report).contains(&report));
        let fresh = fresh_retry_prompt("PHASE\n", &report);
        assert!(fresh.starts_with("PHASE\n\n## Previous attempt failed verification\n"));
        assert!(fresh.ends_with(&report));
    }

    #[test]
    fn summary_is_one_short_line() {
        assert_eq!(one_line_summary(None), "(no summary)");
        assert_eq!(one_line_summary(Some("  \n\n")), "(no summary)");
        assert_eq!(one_line_summary(Some("\n  first \nsecond")), "first");
        let long = "é".repeat(300);
        assert_eq!(one_line_summary(Some(&long)).chars().count(), 200);
    }

    #[test]
    fn plan_prompt_has_goal_markers_keys_and_agent() {
        let models = vec![ModelInfo {
            id: "opus".into(),
            name: "opus".into(),
            description: "Complex reasoning".into(),
            efforts: Vec::new(),
        }];
        let p = plan_prompt("Add a stats command", AgentKind::Codex, Some(&models));
        assert!(p.contains("Add a stats command"));
        assert!(p.contains("\n<<<PHASEWISE-PLAN\n") && p.contains("\nPHASEWISE-PLAN>>>\n"));
        for key in [
            "agent",
            "verify",
            "max_retries",
            "phase_timeout_minutes",
            "verify_timeout_seconds",
            "max_cost_usd",
            "agent_args",
            "model",
            "effort",
        ] {
            assert!(p.contains(&format!("- {key} (")), "{key}");
        }
        assert!(p.contains("agent = \"codex\""));
        assert!(p.contains("- opus: Complex reasoning"));
        assert!(!p.contains("could not be read"));
        assert!(p.contains("Choose the model and the effort of every phase yourself"));
        assert!(p.contains("prefer the newest model"));
        let p = plan_prompt("x", AgentKind::Claude, None);
        assert!(p.contains("could not be read; any model name accepted by the agent's CLI works"));
    }

    #[test]
    fn plan_prompt_shows_the_efforts_of_each_model() {
        let models = vec![
            ModelInfo {
                id: "a".into(),
                name: "a".into(),
                description: "Fast".into(),
                efforts: vec!["low".into(), "high".into()],
            },
            ModelInfo {
                id: "b".into(),
                name: "b".into(),
                description: String::new(),
                efforts: Vec::new(),
            },
        ];
        let p = plan_prompt("x", AgentKind::Codex, Some(&models));
        assert!(p.contains("\n- a: Fast (efforts: low, high)\n"), "{p}");
        assert!(p.contains("\n- b\n"), "{p}");
    }

    #[test]
    fn plan_prompt_example_shows_the_model_choice() {
        let model = |id: &str| ModelInfo {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            efforts: Vec::new(),
        };
        let models = vec![model("big"), model("mid"), model("small")];
        let p = plan_prompt("x", AgentKind::Claude, Some(&models));
        assert!(p.contains("```phasewise\nmodel = \"small\"\neffort = \"low\"\n```"));
        assert!(p.contains("```phasewise\nmodel = \"big\"\neffort = \"high\"\n```"));
        // Without a list the example names no model.
        let p = plan_prompt("x", AgentKind::Claude, None);
        assert!(p.contains("```phasewise\neffort = \"low\"\n```"));
        assert!(!p.contains("model = \""));
    }

    #[test]
    fn repair_prompts_list_the_problems() {
        let problems = vec!["line 3: duplicate".to_string(), "no markers".to_string()];
        let r = plan_repair_prompt(&problems);
        assert!(r.contains("- line 3: duplicate\n- no markers"));
        assert!(r.contains(PLAN_START) && r.contains(PLAN_END));
        let fresh = plan_retry_prompt("ORIGINAL\n", &problems);
        assert!(fresh.starts_with("ORIGINAL\n\n## Your previous answer was rejected\n"));
        assert!(fresh.contains("- no markers"));
    }

    #[test]
    fn extraction_cases() {
        assert_eq!(extract_plan("no plan here"), None);
        assert_eq!(extract_plan("<<<PHASEWISE-PLAN\nnever closed\n"), None);
        assert_eq!(extract_plan("only end\nPHASEWISE-PLAN>>>\n"), None);
        let two = "<<<PHASEWISE-PLAN\nfirst\nPHASEWISE-PLAN>>>\ntext\n<<<PHASEWISE-PLAN\nsecond\nPHASEWISE-PLAN>>>\n";
        assert_eq!(extract_plan(two).as_deref(), Some("second\n"));
        let fenced =
            "intro\n  <<<PHASEWISE-PLAN  \n## A\n\n```verify\ntrue\n```\nPHASEWISE-PLAN>>>\nbye";
        assert_eq!(
            extract_plan(fenced).as_deref(),
            Some("## A\n\n```verify\ntrue\n```\n")
        );
        let crlf = "<<<PHASEWISE-PLAN\r\n## A\r\n\r\nbody\r\nPHASEWISE-PLAN>>>\r\n";
        assert_eq!(extract_plan(crlf).as_deref(), Some("## A\n\nbody\n"));
    }

    #[test]
    fn plan_prompt_allows_read_only_commands() {
        let p = plan_prompt("x", AgentKind::Codex, None);
        assert!(p.contains("never create, edit or delete files"));
        assert!(p.contains("Read-only commands such as `ls`, `cat` or `rg` are fine"));
        assert!(!p.contains("never run commands"));
    }

    #[test]
    fn unwrap_fence_cases() {
        let plan = "+++\nagent = \"claude\"\n+++\n\n## A\n\n```verify\ntrue\n```\n";
        for open in ["```", "```markdown", "```md", "~~~", "````Markdown"] {
            let close = &open[..open.chars().take_while(|c| *c == '`' || *c == '~').count()];
            let wrapped = format!("\n{open}\n{plan}{close}\n\n");
            assert_eq!(unwrap_fence(&wrapped).as_deref(), Some(plan), "{open}");
        }
        // Not wrapped, another language, or the closing fence differs.
        assert_eq!(unwrap_fence(plan), None);
        assert_eq!(unwrap_fence("```toml\na = 1\n```\n"), None);
        assert_eq!(unwrap_fence("```\nbody\n~~~\n"), None);
        assert_eq!(unwrap_fence("````\nbody\n```\n"), None);
        assert_eq!(unwrap_fence("```\n"), None);
        assert_eq!(unwrap_fence(""), None);
    }
}
