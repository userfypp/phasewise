//! Agent adapters: command lines and translation of the agent's stdout into
//! normalized events.

use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::plan::AgentKind;
use crate::process::spawn_in_group;

pub mod claude;
pub mod codex;

const MAX_DETAIL_CHARS: usize = 200;
const LISTING_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-launch settings that become command-line options. `None` passes no flag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// A model an agent can use, as listed by `phasewise models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
}

/// One usage-limit window as the agent reported it (Claude Code only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    /// The agent's name for the window, e.g. `five_hour` or `seven_day`.
    pub name: String,
    /// Share of the window used, from 0 to 1, when the agent says so.
    pub utilization: Option<f64>,
    /// When the window resets, in Unix seconds.
    pub resets_at: Option<i64>,
    /// `allowed`, `allowed_warning` or anything else the agent reports.
    pub status: String,
}

/// What the agent reported, independent of the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        /// The model the agent reports using, when it says so.
        model: Option<String>,
    },
    Message {
        text: String,
        subagent: bool,
    },
    /// `detail` is for example a relative path or the first line of a command.
    ToolCall {
        name: String,
        detail: String,
        subagent: bool,
    },
    Retry {
        detail: String,
    },
    PermissionDenied {
        detail: String,
    },
    /// Non-final error reported by the agent.
    Error {
        detail: String,
    },
    Finished {
        ok: bool,
        summary: String,
        cost_usd: Option<f64>,
    },
    /// The agent's usage-limit windows, as of now.
    RateLimits(Vec<RateWindow>),
}

/// Translates stdout lines into events. Never fails: anything it does not
/// understand is ignored.
pub trait AgentParser: Send {
    fn feed(&mut self, line: &str) -> Vec<AgentEvent>;
}

pub trait Agent: Send + Sync {
    fn kind(&self) -> AgentKind;
    /// `PHASEWISE_CLAUDE_BIN` / `PHASEWISE_CODEX_BIN` when set and non-empty,
    /// else `claude` / `codex`.
    fn program(&self) -> OsString;
    /// First non-empty line of `<program> --version`.
    fn version(&self) -> Result<String>;
    /// Whether the agent says it is logged in, from its own status command
    /// (no model call): `Some(true)` when the command succeeds, `Some(false)`
    /// when it fails or does not answer in time, `None` when the program
    /// cannot be started.
    fn logged_in(&self) -> Option<bool>;
    /// The command that signs in, for the hint when `logged_in` is not true.
    fn login_hint(&self) -> &'static str;
    /// The prompt goes to stdin; stdio is configured by the caller.
    fn new_session_command(
        &self,
        root: &Path,
        options: &LaunchOptions,
        extra_args: &[String],
    ) -> Command;
    fn resume_command(
        &self,
        root: &Path,
        session_id: &str,
        options: &LaunchOptions,
        extra_args: &[String],
    ) -> Command;
    /// Read-only session for `phasewise plan`: the agent may read the project
    /// but not change it. Never carries the plan's `agent_args`.
    fn plan_command(
        &self,
        root: &Path,
        options: &LaunchOptions,
        resume_session: Option<&str>,
    ) -> Command;
    /// Models the agent offers. Claude: a documented static list. Codex: asks
    /// `codex debug models`.
    fn list_models(&self) -> Result<Vec<ModelInfo>>;
    /// `root` is used to shorten absolute paths in tool details.
    fn parser(&self, root: &Path) -> Box<dyn AgentParser>;
}

pub fn agent_for(kind: AgentKind) -> Box<dyn Agent> {
    match kind {
        AgentKind::Claude => Box::new(claude::ClaudeAgent),
        AgentKind::Codex => Box::new(codex::CodexAgent),
    }
}

/// Program name from the value of an override variable (ignored when empty).
fn program_from(value: Option<OsString>, default: &str) -> OsString {
    match value {
        Some(v) if !v.is_empty() => v,
        _ => OsString::from(default),
    }
}

fn program_from_env(var: &str, default: &str) -> OsString {
    program_from(std::env::var_os(var), default)
}

/// Runs `<program> --version` and returns the first non-empty line.
fn program_version(program: &OsString, not_found: &str) -> Result<String> {
    let output = match Command::new(program).arg("--version").output() {
        Ok(output) => output,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(anyhow!("{not_found}")),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Cannot run `{} --version`", program.to_string_lossy()));
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "`{} --version` printed nothing (exit status {}). Check the installation.",
                program.to_string_lossy(),
                output.status
            )
        })
}

/// Runs a status command such as `claude auth status`. See `Agent::logged_in`.
fn program_logged_in(program: &OsString, args: &[&str]) -> Option<bool> {
    match run_listing(program, args, "not found") {
        Ok(_) => Some(true),
        Err(e) if e.to_string() == "not found" => None,
        Err(_) => Some(false),
    }
}

/// Runs `program args` (stdin null, no terminal) in its own process group and
/// returns its stdout; fails on a non-zero exit, on a timeout of 10 s or when
/// the program cannot be started.
fn run_listing(program: &OsString, args: &[&str], not_found: &str) -> Result<String> {
    let name = program.to_string_lossy().into_owned();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match spawn_in_group(&mut cmd) {
        Ok(child) => child,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(anyhow!("{not_found}")),
        Err(e) => {
            return Err(e).with_context(|| format!("Cannot run `{name} {}`", args.join(" ")));
        }
    };
    let Some(mut stdout) = child.take_stdout() else {
        return Err(anyhow!("Cannot read the output of `{name}`"));
    };
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + LISTING_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                child.finish_leftovers(Duration::from_millis(200));
                break status;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.terminate(Duration::from_millis(500));
                return Err(anyhow!(
                    "`{name} {}` did not answer within {} s",
                    args.join(" "),
                    LISTING_TIMEOUT.as_secs()
                ));
            }
            Err(e) => {
                let _ = child.terminate(Duration::from_millis(500));
                return Err(e).with_context(|| format!("Cannot wait for `{name}`"));
            }
        }
    };
    let bytes = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(anyhow!("`{name} {}` failed ({status})", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Shortens a tool detail for display: absolute paths under `root` become
/// relative, only the first line is kept, at most 200 characters.
fn tidy_detail(raw: &str, root: &Path) -> String {
    let mut text = raw.to_string();
    let root_str = root.to_string_lossy();
    let root_str = root_str.trim_end_matches('/');
    if !root_str.is_empty() {
        text = text.replace(&format!("{root_str}/"), "");
        if text == root_str {
            text = ".".to_string();
        }
    }
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    first.unwrap_or("").chars().take(MAX_DETAIL_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_override_ignores_empty() {
        assert_eq!(program_from(None, "claude"), OsString::from("claude"));
        assert_eq!(
            program_from(Some(OsString::new()), "claude"),
            OsString::from("claude")
        );
        assert_eq!(
            program_from(Some(OsString::from("/x/fake")), "claude"),
            OsString::from("/x/fake")
        );
    }

    #[test]
    fn detail_is_relative_first_line_and_short() {
        let root = Path::new("/work/repo");
        assert_eq!(tidy_detail("/work/repo/src/a.rs", root), "src/a.rs");
        assert_eq!(tidy_detail("/work/repo", root), ".");
        assert_eq!(tidy_detail("/other/a.rs", root), "/other/a.rs");
        assert_eq!(tidy_detail("echo one\necho two", root), "echo one");
        assert_eq!(tidy_detail("cat /work/repo/a.txt", root), "cat a.txt");
        let long = "é".repeat(500);
        assert_eq!(tidy_detail(&long, root).chars().count(), 200);
    }

    #[test]
    fn version_reports_missing_binary() {
        let err = program_version(
            &OsString::from("/nonexistent/phasewise-fake-agent"),
            "missing",
        )
        .expect_err("must fail");
        assert_eq!(err.to_string(), "missing");
    }
}
