//! Command-line interface: clap definitions, dispatch and exit codes.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

use crate::agent::LaunchOptions;
use crate::agent::{ModelInfo, agent_for};
use crate::display::{
    event_line, failure_kind_label, model_name, money, path_in, phase_count, phase_header,
    phase_summary, plan_event_line, plan_header, status_label,
};
use crate::engine::{self, EngineEvent};
use crate::git::Git;
use crate::ops::{self, NextOutcome, Overrides, PlanEvent, PlanRequest, PlanResult, Run};
use crate::plan::{self, AgentKind};
use crate::process;
use crate::settings;
use crate::state::{PhaseStatus, RunState, RunStatus};
use crate::style::{Role, Styler};

// Output to stdout goes through these macros instead of `println!`/`print!`,
// which panic when stdout is closed (for example `| head`). A closed stdout
// must not abort the command, so write errors are ignored.
macro_rules! outln {
    () => {{
        let _ = writeln!(std::io::stdout().lock());
    }};
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

macro_rules! out {
    ($($arg:tt)*) => {{
        let _ = write!(std::io::stdout().lock(), $($arg)*);
    }};
}

const INTERRUPT_NOTICE: &str = "Interrupting… (press Ctrl-C again to quit immediately)";
const PLAN_FILE: &str = plan::DEFAULT_PLAN_FILE;
const PLAN_TEMPLATE: &str = include_str!("templates/phasewise.md");

/// Agent selectable from the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentArg {
    Claude,
    Codex,
}

impl From<AgentArg> for AgentKind {
    fn from(arg: AgentArg) -> Self {
        match arg {
            AgentArg::Claude => AgentKind::Claude,
            AgentArg::Codex => AgentKind::Codex,
        }
    }
}

/// `--model` and `--effort` of `run`, `next` and `retry`.
#[derive(Debug, Clone, clap::Args)]
pub struct SettingsArgs {
    /// Model for this run from now on (`default` removes the override)
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    /// Effort for this run from now on: low, medium, high, xhigh or max (`default` removes the override)
    #[arg(long, value_name = "EFFORT")]
    effort: Option<String>,
}

impl SettingsArgs {
    fn overrides(&self) -> Result<Overrides> {
        Overrides::parse(self.model.as_deref(), self.effort.as_deref())
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "phasewise",
    version,
    about = "Run big plans with Claude Code or Codex one phase at a time — verified, checkpointed, and reviewable."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create an example phasewise.md (never overwrites) and exclude .phasewise/ from git
    #[command(after_help = "Examples:\n  phasewise init")]
    Init,

    /// Validate the plan and list its phases and verification commands; runs nothing
    #[command(after_help = "Examples:\n  phasewise check\n  phasewise check docs/plan.md")]
    Check {
        /// Path to the plan file (default: phasewise.md at the repository root)
        plan: Option<PathBuf>,
    },

    /// Check git, claude and codex: presence, version and whether this directory is a suitable repository
    #[command(after_help = "Examples:\n  phasewise doctor")]
    Doctor,

    /// Start a run and launch phase 1; fails if a run is already active
    #[command(
        after_help = "Examples:\n  phasewise run phasewise.md --auto\n  phasewise run --until 2\n  phasewise run --agent codex"
    )]
    Run {
        /// Path to the plan file (default: phasewise.md at the repository root)
        plan: Option<PathBuf>,
        /// Agent to use; takes priority over the plan header
        #[arg(long, value_enum)]
        agent: Option<AgentArg>,
        /// Chain phases until the plan ends or the first failure
        #[arg(long)]
        auto: bool,
        /// Like --auto, but stop when phase N is awaiting review
        #[arg(long, value_name = "N")]
        until: Option<usize>,
        #[command(flatten)]
        settings: SettingsArgs,
    },

    /// Ask the agent to write a plan for a goal (read-only; the agent changes nothing)
    #[command(
        after_help = "Examples:\n  phasewise plan \"Add a stats subcommand\"\n  phasewise plan \"Migrate to clap 4\" --agent codex --output docs/plan.md"
    )]
    Plan {
        /// What you want to do (asked on a terminal when missing)
        goal: Option<String>,
        /// Agent that writes the plan and will run it (default: claude)
        #[arg(long, value_enum)]
        agent: Option<AgentArg>,
        /// Model for the planning session (the plan chooses its own per phase)
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Effort for the planning session: low, medium, high, xhigh or max
        #[arg(long, value_name = "EFFORT")]
        effort: Option<String>,
        /// Where to write the plan (default: phasewise.md at the repository root)
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Overwrite the output file if it exists
        #[arg(long)]
        force: bool,
    },

    /// Approve the phase awaiting review and launch the next one
    #[command(
        after_help = "Examples:\n  phasewise next\n  phasewise next --auto\n  phasewise next --until 4"
    )]
    Next {
        /// Chain phases until the plan ends or the first failure
        #[arg(long)]
        auto: bool,
        /// Like --auto, but stop when phase N is awaiting review
        #[arg(long, value_name = "N")]
        until: Option<usize>,
        #[command(flatten)]
        settings: SettingsArgs,
    },

    /// Go back to the checkpoint before the current phase and rerun it in a new session
    #[command(after_help = "Examples:\n  phasewise retry --note \"Keep the public API unchanged\"")]
    Retry {
        /// Note appended to the phase prompt
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        #[command(flatten)]
        settings: SettingsArgs,
    },

    /// Go back to the checkpoint of phase N (0 = initial state)
    #[command(after_help = "Examples:\n  phasewise rollback 2\n  phasewise rollback 0 --yes")]
    Rollback {
        /// Phase number to return to (0 = initial state)
        n: u32,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Integrate a completed run: squash its branch into the original branch and commit
    #[command(after_help = "Examples:\n  phasewise merge\n  phasewise merge --yes")]
    Merge {
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Show a table of phases: state, attempts, changed files and cost if known
    #[command(after_help = "Examples:\n  phasewise status\n  phasewise status --json")]
    Status {
        /// Print the run as JSON, for scripts (`null` when there is no run)
        #[arg(long)]
        json: bool,
    },

    /// Print the diff of phase N (default: the last one with a checkpoint)
    #[command(after_help = "Examples:\n  phasewise diff\n  phasewise diff 2")]
    Diff {
        /// Phase number
        n: Option<u32>,
    },

    /// Print what happened in phase N: its result, the agent's summary and where the logs are
    #[command(after_help = "Examples:\n  phasewise logs\n  phasewise logs 2")]
    Logs {
        /// Phase number (default: the last phase that started)
        n: Option<u32>,
    },

    /// Open the review TUI
    #[command(after_help = "Examples:\n  phasewise review")]
    Review,

    /// List the models an agent offers (for `--model` and the plan's `model` setting)
    #[command(after_help = "Examples:\n  phasewise models\n  phasewise models --agent codex")]
    Models {
        /// Only this agent (default: both)
        #[arg(long, value_enum)]
        agent: Option<AgentArg>,
    },

    /// Print a completion script for a shell (bash, zsh, fish, elvish or powershell)
    #[command(
        after_help = "Examples:\n  phasewise completions zsh > ~/.zfunc/_phasewise\n  phasewise completions bash > ~/.local/share/bash-completion/completions/phasewise"
    )]
    Completions {
        /// Shell to generate the script for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Show or change the user settings (limits, bell, colour, home screen defaults)
    #[command(
        after_help = "Examples:\n  phasewise settings\n  phasewise settings set bell on\n  phasewise settings reset bell"
    )]
    Settings {
        #[command(subcommand)]
        action: Option<SettingsAction>,
    },

    /// Mark the run as abandoned and return to the original branch (the run branch is kept)
    #[command(after_help = "Examples:\n  phasewise abort")]
    Abort,
}

/// What `phasewise settings` does.
#[derive(Debug, Subcommand)]
pub enum SettingsAction {
    /// Change one setting and save it
    Set {
        /// Setting name (see `phasewise settings`)
        key: String,
        /// New value
        value: String,
    },
    /// Go back to the default of one setting, or of all of them
    Reset {
        /// Setting name (default: all)
        key: Option<String>,
    },
}

/// Entry point called from `main`.
pub fn main() -> ExitCode {
    process::install_panic_hook();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let ok = matches!(
                err.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            );
            // Printing can only fail on a closed stream; nothing useful to do then.
            let _ = err.print();
            return if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            };
        }
    };

    match dispatch(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            for cause in err.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            ExitCode::from(1)
        }
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        None => {
            let cwd = current_dir()?;
            // On a terminal inside a git repository: the TUI, which opens on the
            // run screen for an active run and on the home screen otherwise;
            // outside one, show the help.
            if std::io::stdout().is_terminal() && Git::discover(&cwd).is_ok() {
                return crate::tui::run_home(&cwd);
            }
            Cli::command().print_long_help()?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Init) => init(),
        Some(Command::Check { plan }) => check(plan),
        Some(Command::Doctor) => doctor(),
        Some(Command::Run {
            plan,
            agent,
            auto,
            until,
            settings,
        }) => run_command(plan, agent, ops::Chain::from_flags(auto, until), &settings),
        Some(Command::Plan {
            goal,
            agent,
            model,
            effort,
            output,
            force,
        }) => plan_command(goal, agent, model, effort, output, force),
        Some(Command::Next {
            auto,
            until,
            settings,
        }) => next_command(ops::Chain::from_flags(auto, until), &settings),
        Some(Command::Retry { note, settings }) => retry_command(note, &settings),
        Some(Command::Models { agent }) => models_command(agent),
        Some(Command::Completions { shell }) => completions(shell),
        Some(Command::Rollback { n, yes }) => rollback_command(n as usize, yes),
        Some(Command::Merge { yes }) => merge_command(yes),
        Some(Command::Status { json }) => status(json),
        Some(Command::Diff { n }) => diff(n),
        Some(Command::Logs { n }) => logs(n),
        Some(Command::Review) => crate::tui::run(&current_dir()?),
        Some(Command::Settings { action }) => settings_command(action),
        Some(Command::Abort) => abort_command(),
    }
}

/// Explicit path as given, else `phasewise.md` at the repository root (or in
/// the current directory outside a repository).
fn resolve_plan_path(arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = arg {
        return Ok(path);
    }
    let cwd = std::env::current_dir().context("Cannot read the current directory")?;
    let dir = match Git::discover(&cwd) {
        Ok(git) => git.root().to_path_buf(),
        Err(_) => cwd,
    };
    Ok(dir.join(PLAN_FILE))
}

/// A path as shown to the user: relative to the current directory when it is inside it.
fn shown_path(path: &Path) -> PathBuf {
    match std::env::current_dir() {
        Ok(cwd) => path_in(&cwd, path),
        Err(_) => path.to_path_buf(),
    }
}

fn check(arg: Option<PathBuf>) -> Result<ExitCode> {
    let path = resolve_plan_path(arg)?;
    let loaded = match plan::load(&path) {
        Ok(plan) => plan,
        Err(err) => {
            let message = err.to_string();
            // Parse problems are already rendered as `<path>:<line>: <message>`.
            if message.starts_with(&format!("{}:", path.display())) {
                let shown = shown_path(&path);
                let message = message
                    .lines()
                    .map(|l| match l.strip_prefix(&format!("{}:", path.display())) {
                        Some(rest) => format!("{}:{rest}", shown.display()),
                        None => l.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                eprintln!("{message}");
                eprintln!("error: the plan has {} problem(s)", message.lines().count());
                return Ok(ExitCode::from(1));
            }
            return Err(err);
        }
    };
    print_plan(&loaded, &shown_path(&path));
    Ok(ExitCode::SUCCESS)
}

fn print_plan(plan: &plan::Plan, path: &Path) {
    let st = Styler::for_stdout();
    let config = &plan.config;
    outln!(
        "{} · {}",
        st.heading(plan.title.as_deref().unwrap_or("(untitled)")),
        st.dim(&path.display().to_string())
    );
    outln!("Agent: {}", config.agent);
    if let Some(model) = &config.model {
        outln!("Model: {model}");
    }
    if let Some(effort) = &config.effort {
        outln!("Effort: {effort}");
    }
    if config.verify.is_empty() {
        outln!("Checks after every phase: (none)");
    } else {
        outln!("Checks after every phase:");
        for command in &config.verify {
            outln!("  {} {command}", st.dim("⚙"));
        }
    }
    outln!("Max retries: {}", config.max_retries);
    outln!("Phase timeout: {} min", config.phase_timeout_minutes);
    outln!("Check timeout: {} s", config.verify_timeout_seconds);
    if let Some(limit) = config.max_cost_usd {
        outln!("Spending limit: ${limit:.2}");
        let uses_codex = config.agent == AgentKind::Codex
            || plan
                .phases
                .iter()
                .any(|p| p.agent == Some(AgentKind::Codex));
        if uses_codex {
            outln!("  Codex reports no cost, so the limit only counts the phases that report one.");
        }
    }
    outln!();
    for phase in &plan.phases {
        outln!(
            "  {}",
            st.heading(&format!("{}. {}", phase.number, phase.title))
        );
        let agent = phase.agent.map(|a| a.to_string());
        let timeout = phase.phase_timeout_minutes.map(|m| format!("{m} min"));
        let settings: Vec<String> = [
            ("agent", &agent),
            ("model", &phase.model),
            ("effort", &phase.effort),
            ("timeout", &timeout),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_ref().map(|v| format!("{key}: {v}")))
        .collect();
        if !settings.is_empty() {
            outln!("     {}", st.dim(&settings.join(" · ")));
        }
        if phase.verify.is_empty() {
            outln!("     {}", st.dim("(no phase-specific checks)"));
        } else {
            for command in &phase.verify {
                outln!("     {} {command}", st.dim("⚙"));
            }
        }
    }
    outln!();
    outln!(
        "{}",
        st.ok(&format!(
            "{}. The plan is valid.",
            phase_count(plan.phases.len())
        ))
    );
}

/// What `phasewise models` prints for one agent, or why it could not.
fn models_command(agent: Option<AgentArg>) -> Result<ExitCode> {
    let kinds = match agent {
        Some(a) => vec![AgentKind::from(a)],
        None => vec![AgentKind::Claude, AgentKind::Codex],
    };
    let st = Styler::for_stdout();
    let mut listed = 0;
    let mut first = true;
    for kind in &kinds {
        let a = agent_for(*kind);
        let result = a.list_models();
        let program = a.program().to_string_lossy().into_owned();
        let flag = match kind {
            AgentKind::Claude => "--model",
            AgentKind::Codex => "-m",
        };
        match result {
            Ok(models) => {
                listed += 1;
                if !first {
                    outln!();
                }
                first = false;
                print_models(*kind, &models, &st);
            }
            Err(err) => eprintln!(
                "Could not list the {kind} models: {err:#}. \
Any model name accepted by `{program} {flag}` still works."
            ),
        }
    }
    Ok(if listed == 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn print_models(kind: AgentKind, models: &[ModelInfo], st: &Styler) {
    let (title, own_choice) = match kind {
        AgentKind::Claude => ("Claude Code", "Claude Code's own choice"),
        AgentKind::Codex => ("Codex", "Codex's own choice"),
    };
    outln!("{}", st.heading(title));
    let name_width = models
        .iter()
        .map(|m| m.name.chars().count())
        .chain(["Default".len()])
        .max()
        .unwrap_or(0);
    let id_width = models
        .iter()
        .map(|m| m.id.chars().count())
        .max()
        .unwrap_or(0);
    outln!(
        "  {:<name_width$}  {:<id_width$}  {}",
        "Default",
        "",
        st.dim(own_choice)
    );
    for m in models {
        // Codex lists its own efforts per model; Claude's are one note below.
        let mut parts = Vec::new();
        if !m.description.is_empty() {
            parts.push(m.description.clone());
        }
        if kind == AgentKind::Codex && !m.efforts.is_empty() {
            parts.push(format!("effort: {}", m.efforts.join(", ")));
        }
        let detail = parts.join(" · ");
        let line = format!(
            "  {:<name_width$}  {}  {}",
            m.name,
            st.accent(&format!("{:<id_width$}", m.id)),
            st.dim(&detail)
        );
        outln!("{}", line.trim_end());
    }
    if models.is_empty() {
        outln!("  {}", st.dim("(no models listed)"));
    }
    outln!();
    match kind {
        AgentKind::Claude => {
            outln!("  {}", st.dim("Effort: low, medium, high, xhigh, max"));
            outln!(
                "  {}",
                st.dim(
                    "Other names `claude --model` accepts work too, such as opus or \
claude-opus-5-5[1m]. Your account decides which models you can use."
                )
            );
        }
        AgentKind::Codex => outln!(
            "  {}",
            st.dim("Any model name accepted by `codex -m` also works.")
        ),
    }
}

fn doctor() -> Result<ExitCode> {
    let report = crate::doctor::run(&current_dir()?);
    for line in &report.lines {
        outln!("{line}");
    }
    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn init() -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("Cannot read the current directory")?;
    let git = Git::discover(&cwd)?;
    let path = git.root().join(PLAN_FILE);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&path, PLAN_TEMPLATE)
                .with_context(|| format!("Cannot write {}", path.display()))?;
            outln!("Created {}.", shown_path(&path).display());
        }
        Err(e) => {
            return Err(anyhow!(
                "Cannot inspect {PLAN_FILE}: {e}. Check its permissions and try again."
            ));
        }
        Ok(meta) => match std::fs::metadata(&path) {
            Ok(target) if target.is_file() => {
                outln!("{PLAN_FILE} already exists; left untouched.");
            }
            Ok(target) if target.is_dir() => {
                return Err(anyhow!(
                    "{PLAN_FILE} is a directory, not a plan. Move it away and run `phasewise init` again."
                ));
            }
            Ok(_) => {
                return Err(anyhow!(
                    "{PLAN_FILE} is not a regular file. Move it away and run `phasewise init` again."
                ));
            }
            Err(_) if meta.file_type().is_symlink() => {
                return Err(anyhow!(
                    "{PLAN_FILE} is a symbolic link that points nowhere. Remove or fix the link and run `phasewise init` again."
                ));
            }
            Err(e) => {
                return Err(anyhow!(
                    "Cannot inspect {PLAN_FILE}: {e}. Check its permissions and try again."
                ));
            }
        },
    }
    if git.ensure_excluded(".phasewise/")? {
        outln!("Added .phasewise/ to the git exclude file.");
    }
    outln!();
    outln!("Next steps:");
    outln!("  1. Edit {PLAN_FILE} to describe your phases.");
    outln!("  2. Commit it (`phasewise run` needs a clean working tree).");
    outln!("  3. phasewise check");
    outln!("  4. phasewise run");
    outln!();
    outln!("Or run `phasewise` with no arguments to open the home screen:");
    outln!("commit the plan and start the run from there.");
    Ok(ExitCode::SUCCESS)
}

const NO_RUN: &str = "No run yet. Start one with `phasewise run`.";
const EXIT_PHASE_FAILED: u8 = 3;

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().context("Cannot read the current directory")
}

fn run_command(
    plan: Option<PathBuf>,
    agent: Option<AgentArg>,
    chain: ops::Chain,
    settings: &SettingsArgs,
) -> Result<ExitCode> {
    let overrides = settings.overrides()?;
    let path = resolve_plan_path(plan)?;
    // Before the run exists, so a bad `--until` creates nothing.
    if let ops::Chain::Until(stop) = chain {
        ops::check_until(stop, 1, crate::plan::load(&path)?.phases.len())?;
    }
    let run = ops::start(
        &current_dir()?,
        &path,
        agent.map(AgentKind::from),
        &overrides,
    )?;
    outln!(
        "Run {} on branch {} (original branch: {}, agent: {})",
        run.state.run_id,
        run.state.run_branch,
        run.state.original_branch,
        run.state.agent
    );
    drive(run, move |run, events, cancel| {
        ops::execute(run, chain, events, cancel).map(NextOutcome::Ran)
    })
}

/// `--model` / `--effort` of `plan`; `default` means no flag.
fn plan_options(model: Option<String>, effort: Option<String>) -> Result<LaunchOptions> {
    let pick = |value: Option<String>, check: fn(&str) -> Result<(), String>, flag: &str| {
        value
            .filter(|v| v != "default")
            .map(|v| {
                check(&v)
                    .map(|()| v)
                    .map_err(|e| anyhow!("Invalid --{flag}: {e}."))
            })
            .transpose()
    };
    Ok(LaunchOptions {
        model: pick(model, plan::validate_model, "model")?,
        effort: pick(effort, plan::validate_effort, "effort")?,
    })
}

/// The goal from the command line, or asked on a terminal.
fn read_goal(goal: Option<String>) -> Result<String> {
    let goal = match goal {
        Some(goal) => goal,
        None => {
            if !std::io::stdin().is_terminal() {
                return Err(anyhow!(
                    "No goal given and stdin is not a terminal. Pass it as an argument: \
`phasewise plan \"what you want to do\"`."
                ));
            }
            out!("What do you want to do? ");
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            std::io::stdin()
                .read_line(&mut answer)
                .context("Cannot read the goal from stdin. Pass it as an argument.")?;
            answer
        }
    };
    let goal = goal.trim().to_string();
    if goal.is_empty() {
        return Err(anyhow!(
            "The goal is empty. Say what you want to do: `phasewise plan \"what you want to do\"`."
        ));
    }
    Ok(goal)
}

fn plan_command(
    goal: Option<String>,
    agent: Option<AgentArg>,
    model: Option<String>,
    effort: Option<String>,
    output: Option<PathBuf>,
    force: bool,
) -> Result<ExitCode> {
    let options = plan_options(model, effort)?;
    let timeout = ops::plan_timeout()?;
    let cwd = current_dir()?;
    // Refuse an existing output before asking anything.
    let target = ops::prepare_plan(&cwd, output.as_deref(), force)?;
    let goal = read_goal(goal)?;
    let request = PlanRequest {
        goal,
        agent: agent.map_or(AgentKind::Claude, AgentKind::from),
        options,
        output: Some(target.output.clone()),
        force,
        timeout,
    };
    let default_output = target.git.root().join(plan::DEFAULT_PLAN_FILE);
    let cancel = process::install_signal_handler(Some(INTERRUPT_NOTICE))?;
    let (tx, rx) = mpsc::channel::<PlanEvent>();
    let shown_from = cwd.clone();
    let shown = |path: &Path| path_in(&shown_from, path);
    let worker = thread::spawn(move || ops::create_plan(&cwd, &request, &tx, &cancel));
    let started = Instant::now();
    let st = Styler::for_stdout();
    for event in rx {
        let event = match event {
            PlanEvent::Written { path } => PlanEvent::Written { path: shown(&path) },
            other => other,
        };
        if let Some(header) = plan_header(&event, &st) {
            outln!("{header}");
        } else if let Some(line) = plan_event_line(&event) {
            outln!("{}", line.at(started.elapsed().as_secs()).render(&st));
        }
    }
    let outcome = worker
        .join()
        .map_err(|_| anyhow!("The planning thread panicked. This is a bug; please report it."))??;
    let cost = outcome
        .cost_usd
        .map(|c| st.dim(&format!("Cost: {}", money(c))));
    match outcome.result {
        PlanResult::Written { path, plan } => {
            let is_default = path == default_output;
            let path = shown(&path);
            outln!();
            print_plan(&plan, &path);
            if let Some(cost) = cost {
                outln!("{cost}");
            }
            let run_cmd = if is_default {
                "phasewise run".to_string()
            } else {
                format!("phasewise run {}", shell_arg(&path))
            };
            outln!();
            outln!("Next steps:");
            outln!("  1. Review and edit {}.", path.display());
            outln!("  2. Commit it (`phasewise run` needs a clean working tree).");
            outln!("  3. {run_cmd}");
            Ok(ExitCode::SUCCESS)
        }
        PlanResult::Failed {
            problems,
            last_draft,
            cancelled,
        } => {
            outln!();
            let head = if cancelled {
                "Planning was interrupted. No plan was written."
            } else {
                "phasewise could not get a valid plan. Nothing was written to the output path."
            };
            eprintln!("{head}");
            for problem in &problems {
                eprintln!("  - {problem}");
            }
            if let Some(draft) = last_draft {
                let draft = shown(&draft);
                eprintln!(
                    "Last draft: {} (fix it by hand and check it with `phasewise check {}`).",
                    draft.display(),
                    shell_arg(&draft)
                );
            }
            eprintln!(
                "Agent output and drafts: {}. Run `phasewise plan` again to retry.",
                shown(&outcome.dir).display()
            );
            if let Some(cost) = cost {
                eprintln!("{cost}");
            }
            Ok(ExitCode::from(1))
        }
    }
}

/// A path as one shell word, in single quotes when it has other characters
/// than letters, digits and `_ . / -`.
pub(crate) fn shell_arg(path: &Path) -> String {
    let text = path.display().to_string();
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'));
    if plain {
        text
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn next_command(chain: ops::Chain, settings: &SettingsArgs) -> Result<ExitCode> {
    let overrides = settings.overrides()?;
    let run = load_run()?;
    drive(run, move |run, events, cancel| {
        ops::next(run, chain, &overrides, events, cancel)
    })
}

/// Loads the latest run; says once if a phase left running was marked failed.
fn load_run() -> Result<Run> {
    let mut run = ops::load_current(&current_dir()?)?.ok_or_else(|| anyhow!(NO_RUN))?;
    announce_recovery(&run);
    // Operations may recover more phases under the run lock; only those are announced later.
    run.recovered.clear();
    Ok(run)
}

fn announce_recovery(run: &Run) {
    for number in &run.recovered {
        eprintln!(
            "Phase {number} was left running by a previous phasewise process and is now marked failed (interrupted)."
        );
    }
}

fn retry_command(note: Option<String>, settings: &SettingsArgs) -> Result<ExitCode> {
    let overrides = settings.overrides()?;
    let run = load_run()?;
    drive(run, move |run, events, cancel| {
        ops::retry(run, note.as_deref(), &overrides, events, cancel).map(NextOutcome::Ran)
    })
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn rollback_command(k: usize, yes: bool) -> Result<ExitCode> {
    let mut run = load_run()?;
    let sha = ops::prepare_rollback(&run, k)?;
    let target = if k == 0 {
        "the initial state (before phase 1)".to_string()
    } else {
        format!("the checkpoint of phase {k}")
    };
    outln!(
        "This goes back to {target} ({}) on {}:",
        short_sha(&sha),
        run.state.run_branch
    );
    outln!("  git reset --hard {} and git clean -fd", short_sha(&sha));
    outln!(
        "Changes after it are discarded and untracked files are deleted (ignored files are kept)."
    );
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(anyhow!(
                "Rollback needs confirmation and stdin is not a terminal. Pass --yes to confirm."
            ));
        }
        out!("Continue? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("Cannot read the answer from stdin. Pass --yes to confirm.")?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            outln!("Nothing changed.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    // Counted before the rollback: phases that were already pending are not reset.
    let later = run
        .state
        .phases
        .iter()
        .filter(|p| p.number > k && p.status != PhaseStatus::Pending)
        .count();
    ops::rollback(&mut run, k, Some(&sha))?;
    announce_recovery(&run);
    outln!("Rolled back to {target} ({}).", short_sha(&sha));
    if later > 0 {
        outln!("{later} later phase(s) are pending again.");
    }
    outln!("Next: `phasewise next` runs the next phase.");
    Ok(ExitCode::SUCCESS)
}

fn merge_command(yes: bool) -> Result<ExitCode> {
    let mut run = load_run()?;
    ops::prepare_merge(&run)?;
    let state = &run.state;
    outln!(
        "This squashes {} into {} and commits the result there:",
        state.run_branch,
        state.original_branch
    );
    outln!(
        "  git switch {} && git merge --squash {} && git commit",
        state.original_branch,
        state.run_branch
    );
    outln!("The run branch is kept. Nothing is pushed.");
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(anyhow!(
                "Merge needs confirmation and stdin is not a terminal. Pass --yes to confirm."
            ));
        }
        out!("Continue? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("Cannot read the answer from stdin. Pass --yes to confirm.")?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            outln!("Nothing changed.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    let outcome = ops::merge(&mut run)?;
    announce_recovery(&run);
    match &outcome.commit {
        Some(sha) => outln!(
            "{}",
            Styler::for_stdout().ok(&format!(
                "Committed {} on {}.",
                short_sha(sha),
                outcome.branch
            ))
        ),
        None => outln!(
            "Nothing to merge: the work of {} is already in {}.",
            outcome.run_branch,
            outcome.branch
        ),
    }
    outln!(
        "The run branch {} is kept; delete it with `git branch -D {}` when you no longer need it.",
        outcome.run_branch,
        outcome.run_branch
    );
    Ok(ExitCode::SUCCESS)
}

fn abort_command() -> Result<ExitCode> {
    let mut run = load_run()?;
    let outcome = ops::abort(&mut run)?;
    announce_recovery(&run);
    let state = &run.state;
    outln!("Run {} aborted.", state.run_id);
    if let Some(sha) = &outcome.leftovers_sha {
        outln!(
            "Uncommitted changes were saved in commit {} on {}.",
            short_sha(sha),
            state.run_branch
        );
    }
    if outcome.switched {
        outln!("You are back on {}.", outcome.current_branch);
    } else {
        outln!(
            "You are on {}; it was not changed because you were not on the run branch.",
            outcome.current_branch
        );
    }
    outln!(
        "The run branch {} is kept. To use or remove it:",
        state.run_branch
    );
    outln!(
        "  git switch {} && git merge --squash {} && git commit   integrate its work",
        state.original_branch,
        state.run_branch
    );
    outln!("  git branch -D {}   delete it", state.run_branch);
    Ok(ExitCode::SUCCESS)
}

/// Runs the engine work in a thread that owns the run and prints its events here.
fn drive<F>(run: Run, work: F) -> Result<ExitCode>
where
    F: FnOnce(&mut Run, &Sender<EngineEvent>, &AtomicBool) -> Result<NextOutcome> + Send + 'static,
{
    let cancel = process::install_signal_handler(Some(INTERRUPT_NOTICE))?;
    let verify_secs = ops::settings(&run)?.verify_timeout.as_secs();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut run = run;
        let result = work(&mut run, &tx, &cancel);
        (run, result)
    });
    let started = Instant::now();
    let st = Styler::for_stdout();
    for event in rx {
        // A closed stdout (for example `| head`) must not abort the run, so
        // write errors are ignored by `outln!`.
        if let Some(header) = phase_header(&event, &st) {
            outln!();
            outln!("{header}");
        } else if let Some(line) = event_line(&event, verify_secs) {
            outln!("{}", line.at(started.elapsed().as_secs()).render(&st));
        } else if let Some(summary) = phase_summary(&event, &st) {
            outln!("{summary}");
        }
    }
    let (run, result) = worker
        .join()
        .map_err(|_| anyhow!("The engine thread panicked. This is a bug; please report it."))?;
    announce_recovery(&run);
    Ok(report_outcome(&run, result?))
}

fn report_outcome(run: &Run, outcome: NextOutcome) -> ExitCode {
    // Every outcome is a stop that needs the user: review, failure or the end.
    ring_bell();
    match outcome {
        NextOutcome::Completed => {
            let state = &run.state;
            outln!("{}", Styler::for_stdout().ok("All phases done."));
            outln!("To integrate the work, run `phasewise merge` or:");
            outln!(
                "  git switch {} && git merge --squash {} && git commit",
                state.original_branch,
                state.run_branch
            );
            ExitCode::SUCCESS
        }
        NextOutcome::Ran(PhaseStatus::Failed) => {
            print_failure(run);
            ExitCode::from(EXIT_PHASE_FAILED)
        }
        NextOutcome::Ran(_) => {
            let st = Styler::for_stdout();
            outln!();
            outln!("{}", st.dim("  phasewise diff      show what changed"));
            outln!(
                "{}",
                st.dim("  phasewise review    inspect it interactively")
            );
            outln!(
                "{}",
                st.dim("  phasewise next      approve it and continue")
            );
            ExitCode::SUCCESS
        }
    }
}

fn print_failure(run: &Run) {
    let Some(phase) = run
        .state
        .phases
        .iter()
        .find(|p| p.status == PhaseStatus::Failed)
    else {
        return;
    };
    let st = Styler::for_stdout();
    outln!();
    if let Some(failure) = &phase.failure {
        outln!(
            "{}",
            st.fail(&format!(
                "Phase {} failed ({}): {}",
                phase.number,
                failure_kind_label(failure.kind),
                failure.detail
            ))
        );
    } else {
        outln!("{}", st.fail(&format!("Phase {} failed.", phase.number)));
    }
    outln!("Options:");
    outln!(
        "{}",
        st.dim("  phasewise retry [--note \"...\"]   rerun the phase in a new session")
    );
    outln!(
        "{}",
        st.dim(&format!(
            "  phasewise rollback {}              go back to the previous checkpoint",
            phase.number - 1
        ))
    );
    outln!(
        "{}",
        st.dim("  or inspect the working tree; it was left as the agent left it")
    );
}

/// Prints the completion script. It is built in memory first: the generator
/// panics when stdout is closed early (`| head`).
fn completions(shell: clap_complete::Shell) -> Result<ExitCode> {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "phasewise", &mut script);
    match std::io::stdout().write_all(&script) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(ExitCode::SUCCESS),
    }
}

/// The run as JSON: the stable shape scripts can rely on.
fn status_json(run: &Run) -> Result<String> {
    let state = &run.state;
    let now = jiff::Timestamp::now();
    let phases: Vec<serde_json::Value> = state
        .phases
        .iter()
        .map(|p| {
            serde_json::json!({
                "number": p.number,
                "title": p.title,
                "status": p.status,
                "attempts": p.attempts,
                "cost_usd": p.cost_usd,
                "duration_secs": p.duration_secs(now),
                "agent": engine::phase_agent(state, &run.plan, p.number).to_string(),
                "model": p.agent_model.as_ref().or(p.model.as_ref()),
                "effort": p.effort,
                "checkpoint_sha": p.checkpoint_sha,
                "failure": p.failure.as_ref().map(|f| serde_json::json!({
                    "kind": f.kind,
                    "detail": f.detail,
                })),
            })
        })
        .collect();
    let any_cost = state.phases.iter().any(|p| p.cost_usd.is_some());
    let limit = ops::spending_limit(run).unwrap_or(run.plan.config.max_cost_usd);
    let limits = run_limits(run);
    let value = serde_json::json!({
        "run_id": state.run_id,
        "status": state.status,
        "plan_title": state.plan_title,
        "agent": state.agent.to_string(),
        "original_branch": state.original_branch,
        "run_branch": state.run_branch,
        "cost_usd": any_cost.then(|| state.cost_usd()),
        "duration_secs": state.duration_secs(),
        "max_cost_usd": limit,
        "limits": limits.as_ref().map(|l| l
            .windows
            .iter()
            .map(|w| serde_json::json!({
                "name": w.name,
                "utilization": w.utilization,
                "resets_at": w.resets_at,
                "status": w.status,
            }))
            .collect::<Vec<_>>()),
        "limits_seen_at": limits.as_ref().map(|l| l.seen_at),
        "phases": phases,
    });
    Ok(serde_json::to_string_pretty(&value)?)
}

/// The usage limits last reported by the run's agent, if any and if the
/// `limits` setting is on.
fn run_limits(run: &Run) -> Option<crate::state::RateLimits> {
    if !settings::current().limits() {
        return None;
    }
    crate::state::RateLimits::load(run.git.root()).filter(|l| l.agent == run.state.agent)
}

fn status(json: bool) -> Result<ExitCode> {
    let Some(run) = ops::load_current(&current_dir()?)? else {
        if json {
            outln!("null");
        } else {
            outln!("{NO_RUN}");
        }
        return Ok(ExitCode::SUCCESS);
    };
    announce_recovery(&run);
    if json {
        outln!("{}", status_json(&run)?);
        return Ok(ExitCode::SUCCESS);
    }
    let state = &run.state;
    let run_status = match state.status {
        RunStatus::Active => "active",
        RunStatus::Completed => "completed",
        RunStatus::Aborted => "aborted",
    };
    outln!("Run:    {} ({run_status})", state.run_id);
    outln!(
        "Plan:   {}",
        state.plan_title.as_deref().unwrap_or("(untitled)")
    );
    let other_agents: Vec<String> = state
        .phases
        .iter()
        .map(|p| (p.number, engine::phase_agent(state, &run.plan, p.number)))
        .filter(|(_, agent)| *agent != state.agent)
        .map(|(number, agent)| format!("phase {number}: {agent}"))
        .collect();
    if other_agents.is_empty() {
        outln!("Agent:  {}", state.agent);
    } else {
        outln!("Agent:  {} ({})", state.agent, other_agents.join(", "));
    }
    outln!(
        "Branch: {} (from {})",
        state.run_branch,
        state.original_branch
    );
    if state.phases.iter().any(|p| p.cost_usd.is_some()) {
        let spent = crate::display::money(state.cost_usd());
        match ops::spending_limit(&run).unwrap_or(run.plan.config.max_cost_usd) {
            Some(limit) => outln!("Cost:   {spent} of {} limit", crate::display::money(limit)),
            None => outln!("Cost:   {spent}"),
        }
    }
    if let Some(secs) = state.duration_secs() {
        outln!("Time:   {}", crate::display::duration(secs));
    }
    if let Some(limits) = run_limits(&run) {
        let (now, tz) = crate::display::limits_clock();
        let parts: Vec<String> = crate::display::limit_parts(&limits, now, &tz)
            .into_iter()
            .map(|p| {
                if p.warn {
                    Styler::for_stdout().paint(Role::Warn, &p.text)
                } else {
                    p.text
                }
            })
            .collect();
        outln!("Limits: {}", parts.join("   "));
    }
    outln!();

    let now = jiff::Timestamp::now();
    let rows: Vec<[String; 7]> = state
        .phases
        .iter()
        .map(|p| {
            let files = match (state.checkpoint(p.number - 1), p.checkpoint_sha.as_deref()) {
                (Some(from), Some(to)) => run
                    .git
                    .changed_file_count(from, to)
                    .map_or_else(|_| "-".to_string(), |n| n.to_string()),
                _ => "-".to_string(),
            };
            let cost = p
                .cost_usd
                .map_or_else(|| "-".to_string(), |c| format!("${c:.4}"));
            [
                p.number.to_string(),
                status_label(p.status).to_string(),
                p.attempts.to_string(),
                files,
                cost,
                p.duration_secs(now)
                    .map_or_else(|| "-".to_string(), crate::display::duration),
                p.agent_model.as_ref().or(p.model.as_ref()).map_or_else(
                    || "-".to_string(),
                    |m| model_name(engine::phase_agent(state, &run.plan, p.number), m),
                ),
            ]
        })
        .collect();
    let headers = ["#", "STATUS", "TRIES", "FILES", "COST", "TIME", "MODEL"];
    let mut widths = headers.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let st = Styler::for_stdout();
    // Cells are padded before colouring so the ANSI codes do not affect widths.
    let line = |cells: [&str; 7], title: &str, status_role: Option<Role>| {
        let mut out = String::new();
        for (i, (cell, width)) in cells.iter().zip(widths).enumerate() {
            let padded = format!("{cell:<width$}");
            match (i, status_role) {
                (1, Some(role)) => out.push_str(&st.paint(role, &padded)),
                (_, None) => out.push_str(&st.heading(&padded)),
                _ => out.push_str(&padded),
            }
            out.push_str("  ");
        }
        match status_role {
            None => out.push_str(&st.heading(title)),
            Some(_) => out.push_str(title),
        }
        outln!("{out}");
    };
    line(headers, "TITLE", None);
    for (row, phase) in rows.iter().zip(&state.phases) {
        line(
            [
                &row[0], &row[1], &row[2], &row[3], &row[4], &row[5], &row[6],
            ],
            &phase.title,
            Some(status_role(phase.status)),
        );
        if let (PhaseStatus::Failed, Some(failure)) = (phase.status, &phase.failure) {
            outln!(
                "{}",
                st.fail(&format!(
                    "    {}: {}",
                    failure_kind_label(failure.kind),
                    failure.detail
                ))
            );
        }
    }
    outln!();
    outln!("{}", st.dim(&next_step_hint(&run)));
    Ok(ExitCode::SUCCESS)
}

/// `phasewise settings [set|reset]`. Plain output: no colour.
fn settings_command(action: Option<SettingsAction>) -> Result<ExitCode> {
    let path = settings::require_path()?;
    let loaded = settings::load_from(&path);
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    let mut current = loaded.settings;
    match action {
        None => {
            print_settings(&current, &path);
            return Ok(ExitCode::SUCCESS);
        }
        Some(SettingsAction::Set { key, value }) => {
            current.set(&key, &value).map_err(|e| anyhow!(e))?;
            save_settings(&path, &current)?;
            if let Some(spec) = settings::spec(&key) {
                let value = settings::shown(spec, &current.get(&key));
                outln!("{key} = {value}");
            }
        }
        Some(SettingsAction::Reset { key }) => {
            if let Some(key) = &key
                && settings::spec(key).is_none()
            {
                return Err(anyhow!(settings::unknown_key(key)));
            }
            // A file with problems is replaced by the defaults (its copy is kept).
            let had_problems = !loaded.warnings.is_empty();
            if current.reset(key.as_deref()) || had_problems {
                save_settings(&path, &current)?;
            }
            match key {
                Some(key) => outln!("{key} is back to its default."),
                None => outln!("All settings are back to their defaults."),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn save_settings(path: &Path, current: &settings::Settings) -> Result<()> {
    if let Some(backup) = settings::save_to(path, current)? {
        eprintln!(
            "warning: the previous settings file was not valid; a copy is kept as {}.",
            backup.display()
        );
    }
    Ok(())
}

fn print_settings(current: &settings::Settings, path: &Path) {
    let rows: Vec<[String; 4]> = settings::REGISTRY
        .iter()
        .map(|spec| {
            [
                spec.key.to_string(),
                settings::shown(spec, &current.get(spec.key)),
                settings::shown(spec, spec.default),
                spec.description.to_string(),
            ]
        })
        .collect();
    let headers = ["KEY", "VALUE", "DEFAULT", "DESCRIPTION"];
    let mut widths = headers.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |mark: &str, cells: [&str; 4]| {
        let [key, value, default, description] = cells;
        outln!(
            "{mark} {key:<k$}  {value:<v$}  {default:<d$}  {description}",
            k = widths[0],
            v = widths[1],
            d = widths[2]
        );
    };
    let state = if path.exists() {
        ""
    } else {
        " (not created yet: every setting has its default)"
    };
    outln!("Settings file: {}{state}", path.display());
    outln!("A * marks a setting that differs from its default.");
    outln!();
    line(" ", headers);
    for (spec, row) in settings::REGISTRY.iter().zip(&rows) {
        let mark = if current.is_changed(spec.key) {
            "*"
        } else {
            " "
        };
        line(mark, [&row[0], &row[1], &row[2], &row[3]]);
    }
    outln!();
    outln!(
        "Change one with `phasewise settings set <key> <value>`; `phasewise settings reset [key]` removes it."
    );
}

/// Rings the terminal bell on stderr when the setting is on and stderr is a
/// terminal; the phase that ended needs the user.
fn ring_bell() {
    let stderr = std::io::stderr();
    if settings::bell_wanted(settings::current().bell(), stderr.is_terminal()) {
        let mut stderr = stderr.lock();
        let _ = stderr.write_all(settings::BELL.as_bytes());
        let _ = stderr.flush();
    }
}

fn status_role(status: PhaseStatus) -> Role {
    match status {
        PhaseStatus::Done => Role::Ok,
        PhaseStatus::AwaitingReview => Role::Accent,
        PhaseStatus::Failed => Role::Fail,
        PhaseStatus::Running | PhaseStatus::Verifying => Role::Warn,
        PhaseStatus::Pending => Role::Dim,
    }
}

fn next_step_hint(run: &Run) -> String {
    let state = &run.state;
    match state.status {
        RunStatus::Completed => format!(
            "Run completed. To integrate: `phasewise merge` or git switch {} && git merge --squash {} && git commit",
            state.original_branch, state.run_branch
        ),
        RunStatus::Aborted => "Run aborted. Start a new one with `phasewise run`.".to_string(),
        RunStatus::Active => {
            let find = |status| state.phases.iter().find(|p| p.status == status);
            if let Some(p) = find(PhaseStatus::Failed) {
                format!(
                    "Next: `phasewise retry` or `phasewise rollback {}` (phase {} failed).",
                    p.number - 1,
                    p.number
                )
            } else if let Some(p) = find(PhaseStatus::AwaitingReview) {
                format!(
                    "Next: review phase {} (`phasewise diff`, `phasewise review`), then `phasewise next`.",
                    p.number
                )
            } else if find(PhaseStatus::Running).is_some() || find(PhaseStatus::Verifying).is_some()
            {
                "A phase is running.".to_string()
            } else {
                "Next: `phasewise next` runs the next phase.".to_string()
            }
        }
    }
}

/// `n` as a phase number of the run, or an error that says which exist.
fn existing_phase(state: &RunState, n: u32) -> Result<usize> {
    let n = n as usize;
    if n == 0 || n > state.phases.len() {
        return Err(anyhow!(
            "Phase {n} does not exist; this run has phases 1 to {}. Run `phasewise status`.",
            state.phases.len()
        ));
    }
    Ok(n)
}

fn logs(n: Option<u32>) -> Result<ExitCode> {
    let run = load_run()?;
    let state = &run.state;
    let number = match n {
        Some(n) => existing_phase(state, n)?,
        None => state
            .phases
            .iter()
            .rev()
            .find(|p| p.status != PhaseStatus::Pending)
            .map(|p| p.number)
            .ok_or_else(|| {
                anyhow!("No phase has started yet. Run `phasewise run` or `phasewise next` first.")
            })?,
    };
    let phase = &state.phases[number - 1];
    let tries = match phase.attempts {
        1 => "1 attempt".to_string(),
        n => format!("{n} attempts"),
    };
    outln!(
        "Phase {number} · {} ({}, {tries})",
        phase.title,
        status_label(phase.status)
    );
    if let Some(failure) = &phase.failure {
        outln!();
        outln!("Failure: {}", failure.detail);
    }
    if let Some(summary) = &phase.summary {
        outln!();
        outln!("Agent summary:\n{summary}");
    }
    let mut files: Vec<PathBuf> = (1..=phase.attempts)
        .flat_map(|k| {
            [
                run.paths.agent_stdout_log(number, k),
                run.paths.agent_stderr_log(number, k),
            ]
        })
        .collect();
    files.push(run.paths.verify_log(number));
    files.retain(|f| f.is_file());
    if !files.is_empty() {
        outln!();
        outln!("Logs:");
        for file in files {
            outln!("  {}", file.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn diff(n: Option<u32>) -> Result<ExitCode> {
    let run = load_run()?;
    let state = &run.state;
    let number = match n {
        Some(n) => existing_phase(state, n)?,
        None => state
            .phases
            .iter()
            .rev()
            .find(|p| p.checkpoint_sha.is_some())
            .map(|p| p.number)
            .ok_or_else(|| {
                anyhow!(
                    "No phase has a checkpoint yet. Run `phasewise run` or `phasewise next` first."
                )
            })?,
    };
    let (Some(from), Some(to)) = (state.checkpoint(number - 1), state.checkpoint(number)) else {
        return Err(anyhow!(
            "Phase {number} has no checkpoint yet. Run `phasewise status` to see which phases have one."
        ));
    };
    let text = run.git.diff(from, to)?;
    if text.trim().is_empty() {
        outln!("Phase {number} made no changes.");
        return Ok(ExitCode::SUCCESS);
    }
    let color = std::io::stdout().is_terminal();
    let mut out = String::new();
    for line in text.lines() {
        out.push_str(&colorize(line, color));
        out.push('\n');
    }
    // A closed stdout (for example `| head`) is not an error worth reporting.
    let _ = std::io::stdout().write_all(out.as_bytes());
    Ok(ExitCode::SUCCESS)
}

fn colorize(line: &str, color: bool) -> String {
    if !color {
        return line.to_string();
    }
    let code = if line.starts_with("+++") || line.starts_with("---") {
        None
    } else if line.starts_with('+') {
        Some("32")
    } else if line.starts_with('-') {
        Some("31")
    } else if line.starts_with("@@") {
        Some("36")
    } else {
        None
    };
    match code {
        Some(code) => format!("\x1b[{code}m{line}\x1b[0m"),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_arg_quotes_only_when_needed() {
        assert_eq!(shell_arg(Path::new("docs/plan-2.md")), "docs/plan-2.md");
        assert_eq!(shell_arg(Path::new("my plan.md")), "'my plan.md'");
        assert_eq!(shell_arg(Path::new("it's.md")), "'it'\\''s.md'");
        assert_eq!(shell_arg(Path::new("")), "''");
    }
}
