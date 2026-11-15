//! Claude Code adapter (`claude -p --output-format stream-json --verbose`).

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use serde_json::Value;

use super::{
    Agent, AgentEvent, AgentParser, LaunchOptions, ModelInfo, RateWindow, program_from_env,
    program_logged_in, program_version, tidy_detail,
};
use crate::plan::{self, AgentKind};

const TOOL_DETAIL_KEYS: [&str; 6] = [
    "file_path",
    "path",
    "command",
    "pattern",
    "url",
    "description",
];

pub struct ClaudeAgent;

impl ClaudeAgent {
    fn base_command(&self, root: &Path) -> Command {
        let mut cmd = Command::new(self.program());
        cmd.current_dir(root)
            .args(["-p", "--output-format", "stream-json", "--verbose"])
            .args(["--permission-mode", "acceptEdits"])
            .args(["--permission-prompts", "none"])
            .args(["--allowedTools", "Read,Edit,Write,Bash"])
            .args(["--disallowedTools", "Bash(git push *)"]);
        cmd
    }

    /// Fixed flags, then `--model`/`--effort`, so a resume and the plan's
    /// `agent_args` come after them.
    fn command_with_options(&self, root: &Path, options: &LaunchOptions) -> Command {
        let mut cmd = self.base_command(root);
        if let Some(model) = &options.model {
            cmd.args(["--model", model]);
        }
        if let Some(effort) = &options.effort {
            cmd.args(["--effort", effort]);
        }
        cmd
    }
}

/// The models offered for `claude --model`: (name, full ID, description).
/// The full IDs pin the version shown. Aliases and `[1m]` variants stay valid
/// when typed. The CLI has no non-interactive listing.
pub const CLAUDE_MODELS: [(&str, &str, &str); 4] = [
    (
        "Fable 5.2",
        "claude-fable-5-2",
        "Most capable, for the hardest work",
    ),
    ("Opus 5.5", "claude-opus-5-5", "Complex, multi-step work"),
    (
        "Sonnet 5.5",
        "claude-sonnet-5-5",
        "Fast and capable, for most coding",
    ),
    (
        "Haiku 5.5",
        "claude-haiku-5-5",
        "Fastest and cheapest, for simple phases",
    ),
];

impl Agent for ClaudeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn program(&self) -> OsString {
        program_from_env("PHASEWISE_CLAUDE_BIN", "claude")
    }

    fn version(&self) -> Result<String> {
        program_version(
            &self.program(),
            "`claude` was not found. Install Claude Code or set PHASEWISE_CLAUDE_BIN.",
        )
    }

    fn logged_in(&self) -> Option<bool> {
        program_logged_in(&self.program(), &["auth", "status"])
    }

    fn login_hint(&self) -> &'static str {
        "claude auth login"
    }

    fn new_session_command(
        &self,
        root: &Path,
        options: &LaunchOptions,
        extra_args: &[String],
    ) -> Command {
        let mut cmd = self.command_with_options(root, options);
        cmd.args(extra_args);
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
        cmd.args(["--resume", session_id]).args(extra_args);
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
            .args(["-p", "--output-format", "stream-json", "--verbose"])
            .args(["--permission-mode", "dontAsk"])
            .args(["--permission-prompts", "none"])
            // Only these built-in tools exist in the session, and no MCP server
            // is loaded (`--strict-mcp-config` without `--mcp-config`): tools
            // that need no permission, such as worktrees or subagents, are gone.
            .args(["--tools", "Read,Grep,Glob"])
            .arg("--strict-mcp-config")
            .args(["--allowedTools", "Read,Grep,Glob"])
            .args([
                "--disallowedTools",
                "Edit,Write,NotebookEdit,Bash,WebFetch,WebSearch",
            ]);
        if let Some(model) = &options.model {
            cmd.args(["--model", model]);
        }
        if let Some(effort) = &options.effort {
            cmd.args(["--effort", effort]);
        }
        if let Some(session) = resume_session {
            cmd.args(["--resume", session]);
        }
        cmd
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(CLAUDE_MODELS
            .iter()
            .map(|(name, id, description)| ModelInfo {
                id: (*id).to_string(),
                name: (*name).to_string(),
                description: (*description).to_string(),
                efforts: plan::EFFORTS.iter().map(|e| (*e).to_string()).collect(),
            })
            .collect())
    }

    fn parser(&self, root: &Path) -> Box<dyn AgentParser> {
        Box::new(ClaudeParser::new(root))
    }
}

pub struct ClaudeParser {
    root: PathBuf,
    session_seen: bool,
    denied: HashSet<String>,
}

impl ClaudeParser {
    pub fn new(root: &Path) -> Self {
        ClaudeParser {
            root: root.to_path_buf(),
            session_seen: false,
            denied: HashSet::new(),
        }
    }

    /// First useful string among the known keys of a tool input.
    fn input_detail(&self, input: &Value) -> String {
        TOOL_DETAIL_KEYS
            .iter()
            .filter_map(|key| input.get(*key).and_then(Value::as_str))
            .map(|s| tidy_detail(s, &self.root))
            .find(|s| !s.is_empty())
            .unwrap_or_default()
    }

    /// Reports a denied tool use once per tool use id.
    fn denial(&mut self, value: &Value, out: &mut Vec<AgentEvent>) {
        if let Some(id) = value.get("tool_use_id").and_then(Value::as_str)
            && !self.denied.insert(id.to_string())
        {
            return;
        }
        let name = value
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let detail = value
            .get("tool_input")
            .map(|input| self.input_detail(input))
            .unwrap_or_default();
        let detail = if detail.is_empty() {
            name.to_string()
        } else {
            format!("{name} {detail}")
        };
        out.push(AgentEvent::PermissionDenied { detail });
    }

    fn assistant(&mut self, value: &Value, out: &mut Vec<AgentEvent>) {
        let subagent = value
            .get("parent_tool_use_id")
            .is_some_and(Value::is_string);
        let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        out.push(AgentEvent::Message {
                            text: text.to_string(),
                            subagent,
                        });
                    }
                }
                Some("tool_use") => {
                    let Some(name) = block.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let detail = block
                        .get("input")
                        .map(|input| self.input_detail(input))
                        .unwrap_or_default();
                    out.push(AgentEvent::ToolCall {
                        name: name.to_string(),
                        detail,
                        subagent,
                    });
                }
                _ => {}
            }
        }
    }

    /// Windows of a `rate_limit_event`: `unifiedWindows` when present, else
    /// the single window of `rateLimitType` (no utilization).
    fn rate_limits(value: &Value) -> Vec<RateWindow> {
        let Some(info) = value.get("rate_limit_info").filter(|i| i.is_object()) else {
            return Vec::new();
        };
        let text = |v: &Value, key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let default_status = text(info, "status").unwrap_or_else(|| "allowed".to_string());
        let resets = |v: &Value| v.get("resetsAt").and_then(Value::as_f64).map(|s| s as i64);
        let mut windows: Vec<RateWindow> = info
            .get("unifiedWindows")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter(|(_, w)| w.is_object())
                    .map(|(name, w)| RateWindow {
                        name: name.clone(),
                        utilization: w.get("utilization").and_then(Value::as_f64),
                        resets_at: resets(w),
                        status: text(w, "status").unwrap_or_else(|| default_status.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if windows.is_empty()
            && let Some(name) = text(info, "rateLimitType")
        {
            windows.push(RateWindow {
                name,
                utilization: None,
                resets_at: resets(info),
                status: default_status,
            });
        }
        let rank = |name: &str| match name {
            "five_hour" => 0,
            "seven_day" => 1,
            _ => 2,
        };
        windows.sort_by(|a, b| (rank(&a.name), &a.name).cmp(&(rank(&b.name), &b.name)));
        windows
    }

    fn retry_detail(value: &Value) -> String {
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("API error");
        match value.get("attempt").and_then(Value::as_u64) {
            Some(attempt) => match value.get("max_retries").and_then(Value::as_u64) {
                Some(max) => format!("attempt {attempt}/{max}: {error}"),
                None => format!("attempt {attempt}: {error}"),
            },
            None => error.to_string(),
        }
    }
}

impl AgentParser for ClaudeParser {
    fn feed(&mut self, line: &str) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return out;
        };
        if !value.is_object() {
            return out;
        }
        if !self.session_seen
            && let Some(id) = value.get("session_id").and_then(Value::as_str)
            && !id.is_empty()
        {
            self.session_seen = true;
            let model = value
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_string);
            out.push(AgentEvent::SessionStarted {
                session_id: id.to_string(),
                model,
            });
        }
        let kind = value.get("type").and_then(Value::as_str);
        let subtype = value.get("subtype").and_then(Value::as_str);
        match (kind, subtype) {
            (Some("system"), Some("api_retry")) => out.push(AgentEvent::Retry {
                detail: Self::retry_detail(&value),
            }),
            (Some("system"), Some("permission_denied")) | (Some("permission_denied"), _) => {
                self.denial(&value, &mut out)
            }
            (Some("assistant"), _) => self.assistant(&value, &mut out),
            (Some("rate_limit_event"), _) => {
                let windows = Self::rate_limits(&value);
                if !windows.is_empty() {
                    out.push(AgentEvent::RateLimits(windows));
                }
            }
            (Some("result"), _) => {
                if let Some(denials) = value.get("permission_denials").and_then(Value::as_array) {
                    for denial in denials {
                        self.denial(denial, &mut out);
                    }
                }
                let is_error = value
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // Success results carry `result`; error subtypes carry
                // `errors: string[]` instead (Agent SDK reference).
                let summary = match value.get("result").and_then(Value::as_str) {
                    Some(text) => text.to_string(),
                    None => value
                        .get("errors")
                        .and_then(Value::as_array)
                        .map(|errors| {
                            errors
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .unwrap_or_default(),
                };
                out.push(AgentEvent::Finished {
                    ok: !is_error && subtype == Some("success"),
                    summary,
                    cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
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

    const BASIC: &str = include_str!("../../tests/fixtures/claude-basic.synthetic.jsonl");
    const ERROR: &str = include_str!("../../tests/fixtures/claude-error.synthetic.jsonl");
    const SID: &str = "11111111-2222-3333-4444-555555555555";

    fn parse_all(text: &str) -> Vec<AgentEvent> {
        let mut parser = ClaudeParser::new(Path::new("/work/repo"));
        text.lines().flat_map(|l| parser.feed(l)).collect()
    }

    fn call(name: &str, detail: &str, subagent: bool) -> AgentEvent {
        AgentEvent::ToolCall {
            name: name.into(),
            detail: detail.into(),
            subagent,
        }
    }

    fn window(name: &str, utilization: Option<f64>, resets_at: i64, status: &str) -> RateWindow {
        RateWindow {
            name: name.into(),
            utilization,
            resets_at: Some(resets_at),
            status: status.into(),
        }
    }

    #[test]
    fn rate_limit_event_gives_the_unified_windows() {
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1794678600,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.36,"resetsAt":1794678600,"status":"allowed"},"seven_day":{"utilization":0.49,"resetsAt":1795176000,"status":"allowed_warning"}}},"uuid":"u","session_id":"s"}"#;
        let events = ClaudeParser::new(Path::new("/work/repo")).feed(line);
        assert_eq!(
            events[1..],
            [AgentEvent::RateLimits(vec![
                window("five_hour", Some(0.36), 1794678600, "allowed"),
                window("seven_day", Some(0.49), 1795176000, "allowed_warning"),
            ])]
        );
    }

    #[test]
    fn rate_limit_event_without_unified_windows_uses_the_single_window() {
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1794678600,"rateLimitType":"five_hour"}}"#;
        assert_eq!(
            ClaudeParser::new(Path::new("/work/repo")).feed(line),
            [AgentEvent::RateLimits(vec![window(
                "five_hour",
                None,
                1794678600,
                "rejected"
            )])]
        );
    }

    #[test]
    fn malformed_rate_limit_events_are_ignored() {
        let mut parser = ClaudeParser::new(Path::new("/work/repo"));
        for line in [
            r#"{"type":"rate_limit_event"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":"x"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":[1]}}"#,
            r#"{"type":"rate_limit_event","info":{"remaining":5}}"#,
        ] {
            assert_eq!(parser.feed(line), []);
        }
    }

    #[test]
    fn error_result_uses_errors_array() {
        let mut parser = ClaudeParser::new(Path::new("/work/repo"));
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"s","total_cost_usd":0.5,"permission_denials":[],"errors":["Tool failed","Aborted"]}"#;
        assert_eq!(
            parser.feed(line),
            vec![
                AgentEvent::SessionStarted {
                    session_id: "s".into(),
                    model: None
                },
                AgentEvent::Finished {
                    ok: false,
                    summary: "Tool failed; Aborted".into(),
                    cost_usd: Some(0.5)
                }
            ]
        );
    }

    #[test]
    fn basic_fixture_events() {
        let want = vec![
            AgentEvent::SessionStarted {
                session_id: SID.into(),
                model: Some("fake-model".into()),
            },
            AgentEvent::Message {
                text: "I'll create the file.".into(),
                subagent: false,
            },
            call("Write", "src/hello.txt", false),
            call("Bash", "echo one", false),
            AgentEvent::Message {
                text: "Subagent checking the README.".into(),
                subagent: true,
            },
            call("Read", "README.md", true),
            AgentEvent::Retry {
                detail: "attempt 1/3: overloaded".into(),
            },
            AgentEvent::PermissionDenied {
                detail: "Bash git push origin main".into(),
            },
            // The result repeats toolu_denied_1: only the new denial appears.
            AgentEvent::PermissionDenied {
                detail: "Edit .env".into(),
            },
            AgentEvent::Finished {
                ok: true,
                summary: "Created hello.txt and ran the tests.\nAll good.".into(),
                cost_usd: Some(0.0421),
            },
        ];
        assert_eq!(parse_all(BASIC), want);
    }

    #[test]
    fn error_fixture_finishes_not_ok() {
        let events = parse_all(ERROR);
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Finished {
                ok: false,
                summary: "API Error: 500 internal server error".into(),
                cost_usd: Some(0.0),
            })
        );
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn garbage_is_ignored() {
        let mut parser = ClaudeParser::new(Path::new("/work/repo"));
        for line in ["", "not json", "[1,2]", "42", "{}", r#"{"type":7}"#] {
            assert!(parser.feed(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn non_success_subtype_is_not_ok() {
        let mut parser = ClaudeParser::new(Path::new("/work/repo"));
        let events =
            parser.feed(r#"{"type":"result","subtype":"error_max_turns","is_error":false}"#);
        assert_eq!(
            events,
            vec![AgentEvent::Finished {
                ok: false,
                summary: String::new(),
                cost_usd: None
            }]
        );
    }

    #[test]
    fn null_parent_is_not_subagent_and_detail_priority() {
        let mut parser = ClaudeParser::new(Path::new("/work/repo"));
        let events = parser.feed(
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","name":"Grep","input":{"pattern":"fn main","description":"x"}}]}}"#,
        );
        assert_eq!(events, vec![call("Grep", "fn main", false)]);
    }

    #[test]
    fn commands_match_the_notes() {
        let agent = ClaudeAgent;
        let extra = vec!["--max-budget-usd".to_string(), "5".to_string()];
        let none = LaunchOptions::default();
        let new = agent.new_session_command(Path::new("/work/repo"), &none, &extra);
        let args: Vec<_> = new
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--permission-prompts",
                "none",
                "--allowedTools",
                "Read,Edit,Write,Bash",
                "--disallowedTools",
                "Bash(git push *)",
                "--max-budget-usd",
                "5"
            ]
        );
        assert_eq!(new.get_current_dir(), Some(Path::new("/work/repo")));
        let resume = agent.resume_command(Path::new("/work/repo"), "abc", &none, &extra);
        let args: Vec<_> = resume
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[12..], ["--resume", "abc", "--max-budget-usd", "5"]);
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn plan_command_is_read_only() {
        let agent = ClaudeAgent;
        let root = Path::new("/work/repo");
        let fixed = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--tools",
            "Read,Grep,Glob",
            "--strict-mcp-config",
            "--allowedTools",
            "Read,Grep,Glob",
            "--disallowedTools",
            "Edit,Write,NotebookEdit,Bash,WebFetch,WebSearch",
        ];
        let both = LaunchOptions {
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        let none = LaunchOptions::default();
        let new = agent.plan_command(root, &none, None);
        assert_eq!(argv(&new), fixed);
        assert_eq!(new.get_current_dir(), Some(root));
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.extend(["--model", "opus", "--effort", "high"]);
        assert_eq!(argv(&agent.plan_command(root, &both, None)), expected);
        expected.extend(["--resume", "sid"]);
        assert_eq!(
            argv(&agent.plan_command(root, &both, Some("sid"))),
            expected
        );
        let mut expected: Vec<&str> = fixed.to_vec();
        expected.extend(["--resume", "sid"]);
        assert_eq!(
            argv(&agent.plan_command(root, &none, Some("sid"))),
            expected
        );
    }

    #[test]
    fn model_and_effort_go_after_the_fixed_flags_and_before_resume_and_extras() {
        let agent = ClaudeAgent;
        let root = Path::new("/work/repo");
        let extra = vec!["--max-budget-usd".to_string(), "5".to_string()];
        let both = LaunchOptions {
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        let new = argv(&agent.new_session_command(root, &both, &extra));
        assert_eq!(new[11], "Bash(git push *)");
        assert_eq!(
            new[12..],
            [
                "--model",
                "opus",
                "--effort",
                "high",
                "--max-budget-usd",
                "5"
            ]
        );
        let resume = argv(&agent.resume_command(root, "abc", &both, &extra));
        assert_eq!(
            resume[12..],
            [
                "--model",
                "opus",
                "--effort",
                "high",
                "--resume",
                "abc",
                "--max-budget-usd",
                "5"
            ]
        );
        let only_effort = LaunchOptions {
            model: None,
            effort: Some("low".into()),
        };
        assert_eq!(
            argv(&agent.new_session_command(root, &only_effort, &[]))[12..],
            ["--effort", "low"]
        );
        let only_model = LaunchOptions {
            model: Some("sonnet[1m]".into()),
            effort: None,
        };
        assert_eq!(
            argv(&agent.resume_command(root, "s", &only_model, &[]))[12..],
            ["--model", "sonnet[1m]", "--resume", "s"]
        );
    }

    #[test]
    fn lists_the_catalog_with_full_ids() {
        let models = ClaudeAgent.list_models().expect("static list");
        let pairs: Vec<_> = models
            .iter()
            .map(|m| (m.name.as_str(), m.id.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("Fable 5.2", "claude-fable-5-2"),
                ("Opus 5.5", "claude-opus-5-5"),
                ("Sonnet 5.5", "claude-sonnet-5-5"),
                ("Haiku 5.5", "claude-haiku-5-5"),
            ]
        );
        assert!(models.iter().all(|m| !m.description.is_empty()));
        assert!(models.iter().all(|m| m.efforts.len() == 5));
    }

    #[test]
    fn init_event_reports_the_model() {
        let mut parser = ClaudeParser::new(Path::new("/work/repo"));
        let line =
            r#"{"type":"system","subtype":"init","session_id":"s","model":"claude-haiku-5-5"}"#;
        assert_eq!(
            parser.feed(line),
            vec![AgentEvent::SessionStarted {
                session_id: "s".into(),
                model: Some("claude-haiku-5-5".into())
            }]
        );
    }
}
