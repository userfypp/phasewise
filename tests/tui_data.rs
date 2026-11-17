//! TUI loaders against real runs created with the fake agent.

mod common;

use std::time::Duration;

use common::{TestRepo, isolated_git_env};
use phasewise::ops::{self, Run};
use phasewise::plan::AgentKind;
use phasewise::state::PhaseStatus;
use phasewise::tui::data::{
    LogLine, PlanProblem, PlanState, load_diff, load_home, load_home_from, load_log, load_run_view,
    load_verify,
};
use tempfile::TempDir;

const FAKE_CLAUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fakes/fake-claude.sh");
const PLAN: &str = "+++\nverify = [\"! grep -q broken phase-*.txt\"]\n+++\n\n# Demo plan\n\n\
Shared context.\n\n## Phase 1: First\n\nDo one.\n\n## Phase 2: Second\n\nDo two.\n\n\
## Phase 3: Third\n\nDo three.\n";

struct Env {
    repo: TestRepo,
    logs: TempDir,
}

impl Env {
    fn new() -> Env {
        let repo = TestRepo::new();
        repo.write("phasewise.md", PLAN);
        repo.commit_all("add plan");
        Env {
            repo,
            logs: tempfile::tempdir().expect("create log dir"),
        }
    }

    /// Runs a phasewise command with the fake agent and returns the exit code.
    fn exec(&self, args: &[&str], envs: &[(&str, &str)]) -> i32 {
        let output = self
            .repo
            .phasewise()
            .args(args)
            .env("PHASEWISE_CLAUDE_BIN", FAKE_CLAUDE)
            .env("FAKE_LOG_DIR", self.logs.path())
            .envs(envs.iter().copied())
            .timeout(Duration::from_secs(60))
            .output()
            .expect("run phasewise");
        output.status.code().expect("exit code")
    }

    fn load(&self) -> Run {
        let mut run = ops::load_current(self.repo.path())
            .expect("load run")
            .expect("a run exists");
        run.git = run.git.with_env(isolated_git_env());
        run
    }
}

#[test]
fn checkpointed_phase_shows_its_diff_log_and_verify_output() {
    let env = Env::new();
    assert_eq!(env.exec(&["run"], &[]), 0);
    let run = env.load();

    let view = load_run_view(&run);
    assert_eq!(view.phases.len(), 3);
    assert_eq!(view.phases[0].status, PhaseStatus::AwaitingReview);
    assert!(view.phases[0].has_checkpoint);
    assert!(!view.phases[1].has_checkpoint);
    assert_eq!(view.plan_title, "Demo plan");
    assert!(view.active);

    let diff = load_diff(&run, 1).expect("diff");
    assert_eq!(diff.files, [("phase-1.txt".to_string(), Some(1), Some(0))]);
    assert!(diff.text.contains("+phase 1"), "{}", diff.text);
    assert!(diff.untracked.is_empty());
    assert_eq!(diff.note, None);

    let log = load_log(&run, 1);
    assert_eq!(
        log[0],
        LogLine::Attempt {
            number: 1,
            resumed: false
        }
    );
    let events: Vec<(&str, String, String)> = log
        .iter()
        .filter_map(|l| match l {
            LogLine::Event(e) => Some((e.icon, e.label.clone(), e.detail.clone())),
            LogLine::Attempt { .. } => None,
        })
        .collect();
    assert!(
        events.contains(&("›", String::new(), "Working on phase 1.".to_string())),
        "{log:?}"
    );
    assert!(
        events.contains(&("✎", "Write".to_string(), "phase-1.txt".to_string())),
        "{log:?}"
    );

    // The logs that exist, relative to the repository: phase 3 has none yet.
    let id = &view.run_id;
    assert_eq!(
        view.phases[0].logs,
        [
            format!(".phasewise/runs/{id}/phase-01.attempt-1.jsonl"),
            format!(".phasewise/runs/{id}/phase-01.attempt-1.stderr.log"),
            format!(".phasewise/runs/{id}/phase-01.verify.log"),
        ]
    );
    assert!(view.phases[2].logs.is_empty());

    assert_ne!(load_verify(&run, 1), "No verification output yet.");
    assert_eq!(load_verify(&run, 3), "No verification output yet.");
    assert_eq!(
        load_diff(&run, 3).expect("diff").note.as_deref(),
        Some("No changes yet.")
    );
    assert!(load_log(&run, 3).is_empty());
}

#[test]
fn failed_phase_shows_the_worktree_diff_and_untracked_files() {
    let env = Env::new();
    assert_eq!(env.exec(&["run"], &[]), 0);
    assert_eq!(env.exec(&["next"], &[("FAKE_MODE_2", "never_fixes")]), 3);
    env.repo.write("README.md", "# test repo\nedited\n");
    let run = env.load();

    let view = load_run_view(&run);
    assert_eq!(view.phases[1].status, PhaseStatus::Failed);
    assert!(!view.phases[1].has_checkpoint);
    assert!(view.phases[1].failure.is_some());

    let diff = load_diff(&run, 2).expect("diff");
    assert_eq!(diff.files, [("README.md".to_string(), Some(1), Some(0))]);
    assert!(diff.text.contains("+edited"), "{}", diff.text);
    assert!(diff.untracked.contains(&"phase-2.txt".to_string()));
    assert!(!diff.untracked.contains(&"README.md".to_string()));

    let attempts = run.state.phase(2).expect("phase 2").attempts;
    let log = load_log(&run, 2);
    let headings: Vec<(u32, bool)> = log
        .iter()
        .filter_map(|l| match l {
            LogLine::Attempt { number, resumed } => Some((*number, *resumed)),
            LogLine::Event(_) => None,
        })
        .collect();
    assert_eq!(headings.len(), attempts as usize);
    assert!(attempts >= 2, "the phase must have been retried");
    // The first attempt starts a session; the retries resume it.
    assert_eq!(headings[0], (1, false));
    assert!(
        headings[1..].iter().all(|(_, resumed)| *resumed),
        "{headings:?}"
    );
}

const HOME_PLAN: &str = "+++\nagent = \"codex\"\nmodel = \"gpt-x\"\neffort = \"low\"\n+++\n\n# Home plan\n\n\
## Phase 1: First\n\nDo one.\n\n```verify\ntrue\ntest -f a\n```\n\n\
## Phase 2: Second\n\nDo two.\n\n```phasewise\nmodel = \"special\"\neffort = \"high\"\n```\n";

#[test]
fn home_without_a_plan_shows_the_repository_and_a_missing_plan() {
    let repo = TestRepo::new();
    let home = load_home_from(&repo.git_handle()).expect("load home");
    assert_eq!(home.plan, PlanState::Missing);
    assert!(!home.plan_uncommitted);
    assert_eq!(home.branch, "main");
    let name = repo.path().file_name().expect("dir name").to_string_lossy();
    assert_eq!(home.repo_name, name);
    assert_eq!(
        home.plan_path.file_name().and_then(|n| n.to_str()),
        Some("phasewise.md")
    );
    assert_eq!(
        home.root.canonicalize().ok(),
        repo.path().canonicalize().ok()
    );
}

#[test]
fn home_shows_the_plan_path_relative_to_the_directory_it_started_in() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", HOME_PLAN);
    repo.write("src/lib.rs", "");
    let at_root = load_home(repo.path()).expect("load home");
    assert_eq!(at_root.plan_rel, "phasewise.md");
    let in_src = load_home(&repo.path().join("src")).expect("load home");
    assert_eq!(in_src.plan_rel, "../phasewise.md");
}

#[test]
fn home_with_a_committed_valid_plan_lists_phases_checks_and_models() {
    let repo = TestRepo::new();
    repo.write("phasewise.md", HOME_PLAN);
    repo.commit_all("add plan");
    let home = load_home_from(&repo.git_handle()).expect("load home");
    assert!(!home.plan_uncommitted);
    let PlanState::Valid(plan) = home.plan else {
        panic!("expected a valid plan, got {:?}", home.plan);
    };
    assert_eq!(plan.title.as_deref(), Some("Home plan"));
    assert_eq!(plan.agent, AgentKind::Codex);
    assert_eq!(plan.model.as_deref(), Some("gpt-x"));
    assert_eq!(plan.effort.as_deref(), Some("low"));
    assert_eq!(plan.phases.len(), 2);
    let first = &plan.phases[0];
    assert_eq!(
        (first.number, first.title.as_str(), first.checks),
        (1, "First", 2)
    );
    // Effective values: the plan's, then the phase's own.
    assert_eq!(first.model.as_deref(), Some("gpt-x"));
    assert_eq!(first.effort.as_deref(), Some("low"));
    let second = &plan.phases[1];
    assert_eq!((second.number, second.checks), (2, 0));
    assert_eq!(second.model.as_deref(), Some("special"));
    assert_eq!(second.effort.as_deref(), Some("high"));
}

#[test]
fn home_flags_a_plan_that_is_not_committed() {
    let repo = TestRepo::new();
    // Untracked.
    repo.write("phasewise.md", HOME_PLAN);
    let home = load_home_from(&repo.git_handle()).expect("load home");
    assert!(matches!(home.plan, PlanState::Valid(_)));
    assert!(home.plan_uncommitted);
    // Committed, then modified.
    repo.commit_all("add plan");
    assert!(
        !load_home_from(&repo.git_handle())
            .expect("home")
            .plan_uncommitted
    );
    repo.write("phasewise.md", &format!("{HOME_PLAN}\nMore.\n"));
    assert!(
        load_home_from(&repo.git_handle())
            .expect("home")
            .plan_uncommitted
    );
    // Other dirty files do not count.
    repo.git(&["checkout", "--", "phasewise.md"]);
    repo.write("notes.txt", "x");
    assert!(
        !load_home_from(&repo.git_handle())
            .expect("home")
            .plan_uncommitted
    );
}

#[test]
fn home_with_an_invalid_plan_lists_the_errors_with_their_lines() {
    let repo = TestRepo::new();
    repo.write(
        "phasewise.md",
        "+++\nbogus = 1\n+++\n\n# Bad\n\n## Phase 1: A\n\nx\n\n## Phase 2: A\n\ny\n",
    );
    repo.commit_all("add plan");
    let home = load_home_from(&repo.git_handle()).expect("load home");
    let PlanState::Invalid(problems) = home.plan else {
        panic!("expected an invalid plan, got {:?}", home.plan);
    };
    assert!(!problems.is_empty());
    assert!(
        problems
            .iter()
            .all(|p: &PlanProblem| p.line >= 1 && !p.message.is_empty())
    );
    assert!(
        problems
            .iter()
            .any(|p| p.line == 2 && p.message.contains("bogus")),
        "{problems:?}"
    );
    assert!(!home.plan_uncommitted);
}

#[test]
fn home_names_a_detached_head_and_the_current_branch() {
    let repo = TestRepo::new();
    repo.git(&["switch", "-c", "feature/x"]);
    assert_eq!(
        load_home_from(&repo.git_handle()).expect("home").branch,
        "feature/x"
    );
    repo.git(&["checkout", "--detach"]);
    assert_eq!(
        load_home_from(&repo.git_handle()).expect("home").branch,
        "detached HEAD"
    );
}
