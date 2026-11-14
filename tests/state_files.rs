mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};

use phasewise::plan::AgentKind;
use phasewise::process::is_alive;
use phasewise::state::{
    FailureKind, LockGuard, PhaseStatus, PhasewiseDir, RunPaths, RunState, load_with_recovery,
    lock_holder,
};

fn new_state() -> RunState {
    RunState::new(
        "r1",
        Path::new("plan.md"),
        None,
        AgentKind::Codex,
        "main",
        "phasewise/r1",
        "abc123",
        &[(1, "One".to_string()), (2, "Two".to_string())],
    )
}

fn sleeper() -> Child {
    Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "r1");
    let mut state = new_state();
    let created = state.updated_at;
    state.save(&paths).unwrap();
    assert!(paths.state_file().exists());
    let loaded = RunState::load(&paths).unwrap();
    assert_eq!(loaded, state);
    assert!(loaded.updated_at >= created);

    let pw = PhasewiseDir::new(dir.path());
    assert_eq!(pw.read_current().unwrap(), None);
    pw.write_current("r1").unwrap();
    assert_eq!(pw.read_current().unwrap().as_deref(), Some("r1"));
}

#[test]
fn lock_acquire_and_release() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "r1");
    assert_eq!(lock_holder(&paths), None);
    {
        let _guard = LockGuard::acquire(&paths).unwrap();
        assert_eq!(lock_holder(&paths), Some(std::process::id()));
        assert!(LockGuard::acquire(&paths).is_err());
    }
    assert!(!paths.lock_file().exists());
    assert_eq!(lock_holder(&paths), None);
}

#[test]
fn stale_lock_is_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "r1");
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(!is_alive(pid));
    std::fs::create_dir_all(paths.dir()).unwrap();
    std::fs::write(paths.lock_file(), format!("{pid}\n")).unwrap();
    assert_eq!(lock_holder(&paths), None);
    let guard = LockGuard::acquire(&paths).unwrap();
    assert_eq!(lock_holder(&paths), Some(std::process::id()));
    drop(guard);

    std::fs::write(paths.lock_file(), "garbage").unwrap();
    let _guard = LockGuard::acquire(&paths).unwrap();
}

#[test]
fn live_lock_blocks_acquire_and_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "r1");
    let mut state = new_state();
    state.phases[0].status = PhaseStatus::Running;
    state.save(&paths).unwrap();

    let mut child = sleeper();
    std::fs::write(paths.lock_file(), format!("{}\n", child.id())).unwrap();

    let err = LockGuard::acquire(&paths).unwrap_err().to_string();
    assert!(
        err.contains(&format!("in use by phasewise process {}", child.id())),
        "{err}"
    );
    let loaded = load_with_recovery(&paths).unwrap();
    assert_eq!(loaded.phases[0].status, PhaseStatus::Running);
    assert_eq!(
        RunState::load(&paths).unwrap().phases[0].status,
        PhaseStatus::Running
    );

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn recovery_marks_interrupted_phases() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "r1");
    let mut state = new_state();
    state.phases[0].status = PhaseStatus::Running;
    state.phases[1].status = PhaseStatus::Verifying;
    state.save(&paths).unwrap();

    let recovered = load_with_recovery(&paths).unwrap();
    for phase in &recovered.phases {
        assert_eq!(phase.status, PhaseStatus::Failed);
        assert!(phase.finished_at.is_some());
        let failure = phase.failure.as_ref().unwrap();
        assert_eq!(failure.kind, FailureKind::Interrupted);
        assert_eq!(
            failure.detail,
            "phasewise stopped while this phase was running"
        );
    }
    assert_eq!(RunState::load(&paths).unwrap(), recovered);
}

#[test]
fn dropping_a_guard_leaves_a_lock_that_was_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "r1");
    let guard = LockGuard::acquire(&paths).unwrap();
    let mut other = sleeper();
    std::fs::write(paths.lock_file(), format!("{}\n", other.id())).unwrap();
    drop(guard);
    assert_eq!(lock_holder(&paths), Some(other.id()));
    other.kill().unwrap();
    other.wait().unwrap();
}
