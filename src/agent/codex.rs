//! Codex adapter (`codex exec --json`).

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use serde_json::Value;

use super::{
    Agent, AgentEvent, AgentParser, LaunchOptions, ModelInfo, program_from_env, program_logged_in,
    program_version, run_listing, tidy_detail,
};
use crate::plan::AgentKind;

pub struct CodexAgent;

impl CodexAgent {
    fn base_command(&self, root: &Path) -> Command {
        let mut cmd = Command::new(self.program());
        cmd.current_dir(root)
            .args(["exec", "--json", "--sandbox", "workspace-write"]);
        cmd
    }

    /// Fixed flags, then `-m`/`-c model_reasoning_effort`; both must come
    /// before the `resume` subcommand.
    fn command_with_options(&self, root: &Path, options: &LaunchOptions) -> Command {
        let mut cmd = self.base_command(root);
        if let Some(model) = &options.model {
            cmd.args(["-m", model]);
        }
        if let Some(effort) = &options.effort {
            cmd.arg("-c")
                .arg(format!("model_reasoning_effort=\"{effort}\""));
        }
        cmd
    }
}

/// Models with `"visibility": "list"` from the output of `codex debug models`,
/// in catalog order. Unknown fields are ignored; entries without a `slug` are
/// skipped.
pub fn parse_models(json: &str) -> Result<Vec<ModelInfo>> {
    let value: Value = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("the output is not valid JSON ({e})"))?;
    let entries = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("the output has no `models` list"))?;
    let text = |entry: &Value, key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Ok(entries
        .iter()
        .filter(|e| e.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|e| {
            let id = text(e, "slug");
            if id.is_empty() {
                return None;
            }
            let label = match text(e, "display_name") {
                l if l.is_empty() => id.clone(),
                l => l,
            };
            let efforts = e
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|l| {
                            l.as_str()
                                .or_else(|| l.get("effort").and_then(Value::as_str))
                        })
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some(ModelInfo {
                id,
                name: label,
                description: text(e, "description"),
                efforts,
            })
        })
        .collect())
}

impl Agent for CodexAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn program(&self) -> OsString {
        program_from_env("PHASEWISE_CODEX_BIN", "codex")
    }

    fn version(&self) -> Result<String> {
        program_version(
            &self.program(),
            "`codex` was not found. Install Codex or set PHASEWISE_CODEX_BIN.",
        )
    }

    fn logged_in(&self) -> Option<bool> {
        program_logged_in(&self.program(), &["login", "status"])
    }

    fn login_hint(&self) -> &'static str {
        "codex login"
    }

    fn new_session_command(
        &self,
        root: &Path,
        options: &LaunchOptions,
        extra_args: &[String],
    ) -> Command {
        let mut cmd = self.command_with_options(root, options);
        cmd.arg("-").args(extra_args);
        cmd
    }

    fn resume_command(
        &self,
        root: &Path,
        session_id: &str,
        options: &LaunchOptions,
        extra_args: &[String],
    ) -> Command {
        let mut cmd = self.command_with_options(root, options);
        cmd.args(["resume", session_id, "-"]).args(extra_args);
        cmd
    }

    fn plan_command(
        &self,
        root: &Path,
        options: &LaunchOptions,
        resume_session: Option<&str>,
    ) -> Command {
        let mut cmd = Command::new(self.program());
        cmd.current_dir(root)
            .args(["exec", "--json", "--sandbox", "read-only"]);
        if let Some(model) = &options.model {
            cmd.args(["-m", model]);
        }
        if let Some(effort) = &options.effort {
            cmd.arg("-c")
                .arg(format!("model_reasoning_effort=\"{effort}\""));
        }
        if let Some(session) = resume_session {
            cmd.args(["resume", session]);
        }
        cmd.arg("-");
        cmd
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let output = run_listing(
            &self.program(),
            &["debug", "models"],
            "`codex` was not found. Install Codex or set PHASEWISE_CODEX_BIN.",
        )?;
        parse_models(&output)
    }

    fn parser(&self, root: &Path) -> Box<dyn AgentParser> {
        Box::new(CodexParser::new(root))
    }
}

pub struct CodexParser {
    root: PathBuf,
    session_seen: bool,
    seen_items: HashSet<String>,
    last_message: String,
}

impl CodexParser {
    pub fn new(root: &Path) -> Self {
        CodexParser {
            root: root.to_path_buf(),
            session_seen: false,
            seen_items: HashSet::new(),
            last_message: String::new(),
        }
    }

    /// `completed` is true for `item.completed`, false for `item.started` and
    /// `item.updated`.
    fn item(&mut self, item: &Value, completed: bool, out: &mut Vec<AgentEvent>) {
        let kind = item.get("type").and_then(Value::as_str);
        let text_of = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
        let id = item.get("id").and_then(Value::as_str);

        // A message is only reported when completed (earlier sightings may be
        // partial) and once it has text; the other items on first sight.
        if kind == Some("agent_message") {
            let text = text_of("text");
            if !completed || text.trim().is_empty() {
                return;
            }
            self.last_message = text.to_string();
            if self.first_time(id) {
                out.push(AgentEvent::Message {
                    text: text.to_string(),
                    subagent: false,
                });
            }
            return;
        }
        let (name, detail) = match kind {
            Some("command_execution") => ("Shell", tidy_detail(text_of("command"), &self.root)),
            Some("file_change") => {
                let paths: Vec<String> = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|changes| {
                        changes
                            .iter()
                            .filter_map(|c| c.get("path").and_then(Value::as_str))
                            .map(|p| tidy_detail(p, &self.root))
                            .collect()
                    })
                    .unwrap_or_default();
                ("Edit", paths.join(", ").chars().take(200).collect())
            }
            Some("mcp_tool_call") => (
                "MCP",
                tidy_detail(
                    &format!("{}/{}", text_of("server"), text_of("tool")),
                    &self.root,
                ),
            ),
            Some("web_search") => ("WebSearch", tidy_detail(text_of("query"), &self.root)),
            // Codex subagents ("collab" tools): the call itself is the only
            // trace of their activity in the main thread's stream.
            Some("collab_tool_call") => {
                let prompt = text_of("prompt").lines().next().unwrap_or_default();
                let detail = format!("{} {prompt}", text_of("tool"));
                ("Subagent", tidy_detail(detail.trim(), &self.root))
            }
            Some("error") => {
                if self.first_time(id) {
                    out.push(AgentEvent::Error {
                        detail: text_of("message").to_string(),
                    });
                }
                return;
            }
            _ => return,
        };
        if self.first_time(id) {
            out.push(AgentEvent::ToolCall {
                name: name.to_string(),
                detail,
                subagent: kind == Some("collab_tool_call"),
            });
        }
    }

    /// True the first time an item id is seen (items without id always count).
    fn first_time(&mut self, id: Option<&str>) -> bool {
        match id {
            Some(id) => self.seen_items.insert(id.to_string()),
            None => true,
        }
    }
}

impl AgentParser for CodexParser {
    fn feed(&mut self, line: &str) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return out;
        };
        if !value.is_object() {
            return out;
        }
        if !self.session_seen
            && let Some(id) = value.get("thread_id").and_then(Value::as_str)
            && !id.is_empty()
        {
            self.session_seen = true;
            out.push(AgentEvent::SessionStarted {
                session_id: id.to_string(),
                model: None,
            });
        }
        match value.get("type").and_then(Value::as_str) {
            Some(kind @ ("item.started" | "item.updated" | "item.completed")) => {
                if let Some(item) = value.get("item") {
                    self.item(item, kind == "item.completed", &mut out);
                }
            }
            Some("turn.completed") => out.push(AgentEvent::Finished {
                ok: true,
                summary: self.last_message.clone(),
                cost_usd: None,
            }),
            Some("turn.failed") => {
                let summary = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("The turn failed.");
                out.push(AgentEvent::Finished {
                    ok: false,
                    summary: summary.to_string(),
                    cost_usd: None,
                });
            }
            Some("error") => {
                let detail = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown error reported by Codex.");
                out.push(AgentEvent::Error {
                    detail: detail.to_string(),
                });
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = include_str!("../../tests/fixtures/codex-basic.synthetic.jsonl");
    const FAILED: &str = include_str!("../../tests/fixtures/codex-failed.synthetic.jsonl");
    const TID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    fn parse_all(text: &str) -> Vec<AgentEvent> {
        let mut parser = CodexParser::new(Path::new("/work/repo"));
        text.lines().flat_map(|l| parser.feed(l)).collect()
    }

    fn call(name: &str, detail: &str) -> AgentEvent {
        AgentEvent::ToolCall {
            name: name.into(),
            detail: detail.into(),
            subagent: false,
        }
    }

    #[test]
    fn basic_fixture_events() {
        let want = vec![
            AgentEvent::SessionStarted {
                session_id: TID.into(),
                model: None,
            },
            call("Shell", "/bin/zsh -lc 'ls src'"),
            call("Edit", "src/hello.txt, README.md"),
            AgentEvent::Message {
                text: "Created hello.txt.\nDone.".into(),
                subagent: false,
            },
            AgentEvent::Finished {
                ok: true,
                summary: "Created hello.txt.\nDone.".into(),
                cost_usd: None,
            },
        ];
        assert_eq!(parse_all(BASIC), want);
    }

    #[test]
    fn failed_fixture_events() {
        let want = vec![
            AgentEvent::SessionStarted {
                session_id: TID.into(),
                model: None,
            },
            AgentEvent::Error {
                detail: "stream disconnected, retrying".into(),
            },
            AgentEvent::Finished {
                ok: false,
                summary: "model refused the request".into(),
                cost_usd: None,
            },
        ];
        assert_eq!(parse_all(FAILED), want);
    }

    #[test]
    fn mcp_and_web_search_items() {
        let mut parser = CodexParser::new(Path::new("/work/repo"));
        let mcp = r#"{"type":"item.started","item":{"id":"i1","type":"mcp_tool_call","server":"docs","tool":"search"}}"#;
        let web = r#"{"type":"item.completed","item":{"id":"i2","type":"web_search","query":"rust process groups"}}"#;
        assert_eq!(parser.feed(mcp), vec![call("MCP", "docs/search")]);
        assert!(
            parser
                .feed(&mcp.replace("item.started", "item.completed"))
                .is_empty()
        );
        assert_eq!(
            parser.feed(web),
            vec![call("WebSearch", "rust process groups")]
        );
    }

    #[test]
    fn message_started_empty_then_completed() {
        let mut parser = CodexParser::new(Path::new("/work/repo"));
        let started =
            r#"{"type":"item.started","item":{"id":"m","type":"agent_message","text":""}}"#;
        let done =
            r#"{"type":"item.completed","item":{"id":"m","type":"agent_message","text":"hi"}}"#;
        assert!(parser.feed(started).is_empty());
        assert_eq!(
            parser.feed(done),
            vec![AgentEvent::Message {
                text: "hi".into(),
                subagent: false
            }]
        );
        assert!(parser.feed(done).is_empty());
    }

    #[test]
    fn turn_completed_without_message_has_empty_summary() {
        let mut parser = CodexParser::new(Path::new("/work/repo"));
        assert_eq!(
            parser.feed(r#"{"type":"turn.completed"}"#),
            vec![AgentEvent::Finished {
                ok: true,
                summary: String::new(),
                cost_usd: None
            }]
        );
    }

    #[test]
    fn updated_items_subagents_and_error_items() {
        let mut parser = CodexParser::new(Path::new("/work/repo"));
        let partial =
            r#"{"type":"item.updated","item":{"id":"m","type":"agent_message","text":"Work"}}"#;
        assert!(parser.feed(partial).is_empty());
        let cmd = r#"{"type":"item.updated","item":{"id":"c","type":"command_execution","command":"ls","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#;
        let cmd_done = r#"{"type":"item.completed","item":{"id":"c","type":"command_execution","command":"ls","aggregated_output":"a","exit_code":0,"status":"completed"}}"#;
        assert_eq!(
            parser.feed(cmd),
            vec![AgentEvent::ToolCall {
                name: "Shell".into(),
                detail: "ls".into(),
                subagent: false
            }]
        );
        assert!(parser.feed(cmd_done).is_empty());
        let collab = r#"{"type":"item.started","item":{"id":"s","type":"collab_tool_call","tool":"spawn_agent","sender_thread_id":"t0","receiver_thread_ids":[],"prompt":"Write the tests\nin detail","agents_states":{},"status":"in_progress"}}"#;
        assert_eq!(
            parser.feed(collab),
            vec![AgentEvent::ToolCall {
                name: "Subagent".into(),
                detail: "spawn_agent Write the tests".into(),
                subagent: true
            }]
        );
        let error = r#"{"type":"item.completed","item":{"id":"e","type":"error","message":"model overloaded"}}"#;
        assert_eq!(
            parser.feed(error),
            vec![AgentEvent::Error {
                detail: "model overloaded".into()
            }]
        );
        let done = r#"{"type":"item.completed","item":{"id":"m","type":"agent_message","text":"Work done."}}"#;
        assert_eq!(
            parser.feed(done),
            vec![AgentEvent::Message {
                text: "Work done.".into(),
                subagent: false
            }]
        );
    }

    #[test]
    fn garbage_is_ignored() {
        let mut parser = CodexParser::new(Path::new("/work/repo"));
        for line in ["", "nope", "[]", "{}", r#"{"type":"item.completed"}"#] {
            assert!(parser.feed(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn commands_match_the_notes() {
        let agent = CodexAgent;
        let extra = vec!["-c".to_string(), "x=1".to_string()];
        let none = LaunchOptions::default();
        let new = agent.new_session_command(Path::new("/work/repo"), &none, &extra);
        let args: Vec<_> = new
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "exec",
                "--json",
                "--sandbox",
                "workspace-write",
                "-",
                "-c",
                "x=1"
            ]
        );
        assert_eq!(new.get_current_dir(), Some(Path::new("/work/repo")));
        let resume = agent.resume_command(Path::new("/work/repo"), "tid", &none, &extra);
        let args: Vec<_> = resume
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "exec",
                "--json",
                "--sandbox",
                "workspace-write",
                "resume",
                "tid",
                "-",
                "-c",
                "x=1"
            ]
        );
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn plan_command_is_read_only() {
        let agent = CodexAgent;
        let root = Path::new("/work/repo");
        let fixed = ["exec", "--json", "--sandbox", "read-only"];
        let both = LaunchOptions {
            model: Some("gpt-fast".into()),
            effort: Some("high".into()),
        };
        let none = LaunchOptions::default();
        let new = agent.plan_command(root, &none, None);
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.push("-");
        assert_eq!(argv(&new), expected);
        assert_eq!(new.get_current_dir(), Some(root));
        let opts = ["-m", "gpt-fast", "-c", "model_reasoning_effort=\"high\""];
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.extend(opts);
        expected.push("-");
        assert_eq!(argv(&agent.plan_command(root, &both, None)), expected);
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.extend(opts);
        expected.extend(["resume", "tid", "-"]);
        assert_eq!(
            argv(&agent.plan_command(root, &both, Some("tid"))),
            expected
        );
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.extend(["resume", "tid", "-"]);
        assert_eq!(
            argv(&agent.plan_command(root, &none, Some("tid"))),
            expected
        );
    }

    #[test]
    fn model_and_effort_go_before_the_resume_subcommand() {
        let agent = CodexAgent;
        let root = Path::new("/work/repo");
        let extra = vec!["-c".to_string(), "x=1".to_string()];
        let both = LaunchOptions {
            model: Some("gpt-fast".into()),
            effort: Some("high".into()),
        };
        let fixed = ["exec", "--json", "--sandbox", "workspace-write"];
        let opts = ["-m", "gpt-fast", "-c", "model_reasoning_effort=\"high\""];
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.extend(opts);
        expected.extend(["-", "-c", "x=1"]);
        assert_eq!(
            argv(&agent.new_session_command(root, &both, &extra)),
            expected
        );
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.extend(opts);
        expected.extend(["resume", "tid", "-", "-c", "x=1"]);
        assert_eq!(
            argv(&agent.resume_command(root, "tid", &both, &extra)),
            expected
        );
        let effort = LaunchOptions {
            model: None,
            effort: Some("low".into()),
        };
        assert_eq!(
            argv(&agent.resume_command(root, "t", &effort, &[]))[4..],
            ["-c", "model_reasoning_effort=\"low\"", "resume", "t", "-"]
        );
        let model = LaunchOptions {
            model: Some("m".into()),
            effort: None,
        };
        assert_eq!(
            argv(&agent.new_session_command(root, &model, &[]))[4..],
            ["-m", "m", "-"]
        );
    }

    #[test]
    fn catalog_keeps_listed_models_in_order() {
        let json = include_str!("../../tests/fixtures/codex-models.synthetic.json");
        let models = parse_models(json).expect("catalog");
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        // The hidden entry and the one without a slug are dropped.
        assert_eq!(ids, ["gpt-fast", "gpt-deep"]);
        assert_eq!(models[0].name, "GPT-Fast");
        assert_eq!(models[0].description, "Fast model for simple tasks.");
        assert_eq!(models[0].efforts, ["low", "medium", "high"]);
        // Plain string efforts are accepted too.
        assert_eq!(models[1].efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(models[1].description, "");
    }

    #[test]
    fn bad_catalogs_are_errors() {
        assert!(parse_models("not json").is_err());
        assert!(parse_models("{}").is_err());
        assert!(parse_models(r#"{"models": []}"#).expect("empty").is_empty());
    }
}
