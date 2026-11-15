//! Run state on disk: `.phasewise/` layout, `state.json`, lock and recovery.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agent::RateWindow;
use crate::plan::AgentKind;
use crate::process;

const SCHEMA_VERSION: u32 = 1;
const SUMMARY_MAX_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    Completed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    Running,
    Verifying,
    AwaitingReview,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    AgentError,
    VerifyFailed,
    Timeout,
    Interrupted,
    CheckpointFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseState {
    pub number: usize,
    pub title: String,
    pub status: PhaseStatus,
    pub attempts: u32,
    pub session_id: Option<String>,
    pub checkpoint_sha: Option<String>,
    /// The agent's final message, at most 2000 characters.
    pub summary: Option<String>,
    pub cost_usd: Option<f64>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub failure: Option<Failure>,
    /// Effective model and effort of the last attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The model the agent reported using in the last attempt, when it says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub schema_version: u32,
    pub run_id: String,
    pub plan_path: PathBuf,
    pub plan_title: Option<String>,
    pub agent: AgentKind,
    pub original_branch: String,
    pub run_branch: String,
    pub base_sha: String,
    pub status: RunStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub phases: Vec<PhaseState>,
    /// Overrides given with `--model` / `--effort`; they beat the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_override: Option<String>,
}

/// The `<repo>/.phasewise` directory.
#[derive(Debug, Clone)]
pub struct PhasewiseDir {
    dir: PathBuf,
}

impl PhasewiseDir {
    pub fn new(repo_root: &Path) -> PhasewiseDir {
        PhasewiseDir {
            dir: repo_root.join(".phasewise"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn current_file(&self) -> PathBuf {
        self.dir.join("current")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.dir.join("runs")
    }

    /// Id of the active run, if any.
    pub fn read_current(&self) -> Result<Option<String>> {
        let path = self.current_file();
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text.trim().to_string()).filter(|id| !id.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("Cannot read {}", path.display())),
        }
    }

    pub fn write_current(&self, run_id: &str) -> Result<()> {
        write_atomic(&self.current_file(), format!("{run_id}\n").as_bytes())
    }

    pub fn rate_limits_file(&self) -> PathBuf {
        self.dir.join("rate-limits.json")
    }
}

/// The latest usage-limit windows an agent reported, kept per repository in
/// `.phasewise/rate-limits.json`. Last values seen during a phase, not a live
/// query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    pub agent: AgentKind,
    /// When the windows were seen, in Unix seconds.
    pub seen_at: i64,
    pub windows: Vec<RateWindow>,
}

impl RateLimits {
    /// The stored limits; `None` when there is no file or it cannot be read.
    pub fn load(repo_root: &Path) -> Option<RateLimits> {
        let text = fs::read_to_string(PhasewiseDir::new(repo_root).rate_limits_file()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Stores `windows` seen at `seen_at` unless they equal the stored ones
    /// and were stored less than a minute ago (the event arrives many times
    /// per phase; the refresh keeps `seen_at` honest).
    /// Errors are for the caller to ignore: limits never fail a phase.
    pub fn save_if_changed(
        repo_root: &Path,
        agent: AgentKind,
        windows: &[RateWindow],
        seen_at: i64,
    ) -> Result<()> {
        if let Some(stored) = RateLimits::load(repo_root)
            && stored.agent == agent
            && stored.windows == windows
            && seen_at.saturating_sub(stored.seen_at) < 60
        {
            return Ok(());
        }
        let limits = RateLimits {
            agent,
            seen_at,
            windows: windows.to_vec(),
        };
        let json = serde_json::to_string_pretty(&limits)?;
        write_atomic(
            &PhasewiseDir::new(repo_root).rate_limits_file(),
            json.as_bytes(),
        )
    }
}

/// Every path of one run.
#[derive(Debug, Clone)]
pub struct RunPaths {
    dir: PathBuf,
}

impl RunPaths {
    pub fn new(repo_root: &Path, run_id: &str) -> RunPaths {
        RunPaths {
            dir: PhasewiseDir::new(repo_root).runs_dir().join(run_id),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn state_file(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn plan_copy(&self) -> PathBuf {
        self.dir.join("plan.md")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.dir.join("lock")
    }

    pub fn agent_stdout_log(&self, phase: usize, attempt: u32) -> PathBuf {
        self.dir
            .join(format!("phase-{phase:02}.attempt-{attempt}.jsonl"))
    }

    pub fn agent_stderr_log(&self, phase: usize, attempt: u32) -> PathBuf {
        self.dir
            .join(format!("phase-{phase:02}.attempt-{attempt}.stderr.log"))
    }

    pub fn verify_log(&self, phase: usize) -> PathBuf {
        self.dir.join(format!("phase-{phase:02}.verify.log"))
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` through a temp file in the same directory,
/// `sync_all` and `rename`. Creates the parent directory if needed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "Cannot write {}: it has no parent directory",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| format!("Cannot create {}", parent.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("Cannot write {}", path.display()));
    }
    Ok(())
}

/// Truncates `s` to at most 2000 characters (never splits a character).
pub fn truncate_summary(s: &str) -> String {
    match s.char_indices().nth(SUMMARY_MAX_CHARS) {
        Some((idx, _)) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

impl RunState {
    /// What the agents of every phase reported spending so far, in US dollars.
    pub fn cost_usd(&self) -> f64 {
        self.phases.iter().filter_map(|p| p.cost_usd).sum()
    }

    /// A new active run with every phase pending.
    // The run is described by these independent fields; a parameter struct would
    // only move the same list to the call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: &str,
        plan_path: &Path,
        plan_title: Option<String>,
        agent: AgentKind,
        original_branch: &str,
        run_branch: &str,
        base_sha: &str,
        phases: &[(usize, String)],
    ) -> RunState {
        let now = Timestamp::now();
        RunState {
            schema_version: SCHEMA_VERSION,
            run_id: run_id.to_string(),
            plan_path: plan_path.to_path_buf(),
            plan_title,
            agent,
            original_branch: original_branch.to_string(),
            run_branch: run_branch.to_string(),
            base_sha: base_sha.to_string(),
            status: RunStatus::Active,
            created_at: now,
            updated_at: now,
            phases: phases
                .iter()
                .map(|(number, title)| PhaseState {
                    number: *number,
                    title: title.clone(),
                    status: PhaseStatus::Pending,
                    attempts: 0,
                    session_id: None,
                    checkpoint_sha: None,
                    summary: None,
                    cost_usd: None,
                    started_at: None,
                    finished_at: None,
                    failure: None,
                    model: None,
                    effort: None,
                    agent_model: None,
                })
                .collect(),
            model_override: None,
            effort_override: None,
        }
    }

    /// Updates `updated_at` and writes `state.json` atomically.
    pub fn save(&mut self, paths: &RunPaths) -> Result<()> {
        self.updated_at = Timestamp::now();
        let mut json = serde_json::to_string_pretty(self).context("Cannot serialize state")?;
        json.push('\n');
        write_atomic(&paths.state_file(), json.as_bytes())
    }

    pub fn load(paths: &RunPaths) -> Result<RunState> {
        let path = paths.state_file();
        let text =
            fs::read_to_string(&path).with_context(|| format!("Cannot read {}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?;
        let version = value.get("schema_version").and_then(|v| v.as_u64());
        if version != Some(u64::from(SCHEMA_VERSION)) {
            let found = version.map_or_else(|| "missing".to_string(), |v| v.to_string());
            bail!(
                "state.json has schema version {found}; this phasewise understands {SCHEMA_VERSION}. \
Use the phasewise version that created the run, or start a new run"
            );
        }
        serde_json::from_value(value)
            .with_context(|| format!("{} has an unexpected format", path.display()))
    }

    /// Checkpoint SHA after phase `n` (`0` is the base commit).
    pub fn checkpoint(&self, n: usize) -> Option<&str> {
        if n == 0 {
            Some(&self.base_sha)
        } else {
            self.phase(n)?.checkpoint_sha.as_deref()
        }
    }

    /// Phase by 1-based number.
    pub fn phase(&self, n: usize) -> Option<&PhaseState> {
        n.checked_sub(1).and_then(|i| self.phases.get(i))
    }

    pub fn phase_mut(&mut self, n: usize) -> Option<&mut PhaseState> {
        n.checked_sub(1).and_then(|i| self.phases.get_mut(i))
    }
}

/// Holds `runs/<id>/lock` while a phase runs; removes it on drop.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
}

/// PID in the lock file, only if that process is alive.
pub fn lock_holder(paths: &RunPaths) -> Option<u32> {
    let text = fs::read_to_string(paths.lock_file()).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    process::is_alive(pid).then_some(pid)
}

impl LockGuard {
    /// Takes the run lock. The lock file is created with `create_new`, so two
    /// processes can never both own it (the one exception to temp + rename).
    pub fn acquire(paths: &RunPaths) -> Result<LockGuard> {
        let path = paths.lock_file();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        for attempt in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let written = file
                        .write_all(format!("{}\n", std::process::id()).as_bytes())
                        .and_then(|()| file.sync_all());
                    if let Err(e) = written {
                        let _ = fs::remove_file(&path);
                        return Err(e).with_context(|| format!("Cannot write {}", path.display()));
                    }
                    return Ok(LockGuard { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    let trimmed = text.trim();
                    let pid: Option<u32> = trimmed.parse().ok();
                    let stale = match pid {
                        Some(pid) => !process::is_alive(pid),
                        // Empty: another process is creating it right now.
                        None => !trimmed.is_empty(),
                    };
                    if !stale || attempt == 1 {
                        return Err(lock_in_use(pid, &path));
                    }
                    if !take_over_stale(&path, trimmed) {
                        return Err(lock_in_use(None, &path));
                    }
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("Cannot create {}", path.display()));
                }
            }
        }
        Err(lock_in_use(None, &path))
    }
}

/// Removes a lock that looked stale (it held `seen`). The file is moved aside
/// first and read again: another process may have replaced the stale lock with
/// its own between our read and now, and deleting that one would leave two
/// processes holding the run. When the moved file is not the one we saw, it is
/// put back and the lock counts as in use. Returns whether the way is clear.
fn take_over_stale(path: &Path, seen: &str) -> bool {
    let aside = path.with_extension(format!("stale-{}", std::process::id()));
    if fs::rename(path, &aside).is_err() {
        // Gone already: released, or taken over by someone else. Try again.
        return true;
    }
    let moved = fs::read_to_string(&aside).unwrap_or_default();
    if moved.trim() == seen {
        let _ = fs::remove_file(&aside);
        return true;
    }
    // Not the lock we judged: put it back (this fails if yet another process
    // created a lock meanwhile, which then stands).
    let _ = fs::hard_link(&aside, path);
    let _ = fs::remove_file(&aside);
    false
}

fn lock_in_use(pid: Option<u32>, path: &Path) -> anyhow::Error {
    let holder = pid.map_or_else(
        || "another phasewise process".to_string(),
        |pid| format!("phasewise process {pid}"),
    );
    anyhow!(
        "This run is in use by {holder} (lock file: {}). Wait for it to finish; \
if no phasewise process is running, delete the lock file.",
        path.display()
    )
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Only a lock that still names this process: if it was replaced, the
        // new holder's lock is not ours to delete.
        let ours = fs::read_to_string(&self.path)
            .is_ok_and(|text| text.trim() == std::process::id().to_string());
        if ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Loads the state; with a live lock (another process, or this one running a
/// phase in a worker thread) the state is returned as is. Without one, phases
/// left `running`/`verifying` become `failed` (`interrupted`) and the state is
/// saved. That write happens under the lock, so it cannot overwrite the state
/// of a process that has just taken the lock and started a phase.
pub fn load_with_recovery(paths: &RunPaths) -> Result<RunState> {
    let state = RunState::load(paths)?;
    let orphans = state
        .phases
        .iter()
        .any(|p| matches!(p.status, PhaseStatus::Running | PhaseStatus::Verifying));
    if !orphans || lock_holder(paths).is_some() {
        return Ok(state);
    }
    match LockGuard::acquire(paths) {
        Ok(lock) => load_locked(paths, &lock),
        // Another process took the lock meanwhile: its phase may really be running.
        Err(_) => Ok(state),
    }
}

/// Like `load_with_recovery` for a caller that holds the run lock: any phase
/// left `running`/`verifying` is orphaned, even though the lock is live.
pub fn load_locked(paths: &RunPaths, _lock: &LockGuard) -> Result<RunState> {
    let mut state = RunState::load(paths)?;
    recover_orphans(&mut state, paths)?;
    Ok(state)
}

fn recover_orphans(state: &mut RunState, paths: &RunPaths) -> Result<()> {
    let now = Timestamp::now();
    let mut changed = false;
    for phase in &mut state.phases {
        if matches!(phase.status, PhaseStatus::Running | PhaseStatus::Verifying) {
            phase.status = PhaseStatus::Failed;
            phase.finished_at = Some(now);
            phase.failure = Some(Failure {
                kind: FailureKind::Interrupted,
                detail: "phasewise stopped while this phase was running".to_string(),
            });
            changed = true;
        }
    }
    if changed {
        state.save(paths)?;
    }
    Ok(())
}

/// UTC `YYYYMMDD-HHMMSS-xxxx` with four random lowercase hex characters.
pub fn new_run_id(now: Timestamp) -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_i128(now.as_nanosecond());
    hasher.write_u32(std::process::id());
    let suffix = hasher.finish() & 0xffff;
    format!("{}-{suffix:04x}", now.strftime("%Y%m%d-%H%M%S"))
}

pub fn run_branch_name(run_id: &str) -> String {
    format!("phasewise/{run_id}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn taking_over_removes_the_lock_that_was_seen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let lock = dir.path().join("lock");
        fs::write(&lock, "999999\n").expect("write");
        assert!(take_over_stale(&lock, "999999"));
        assert!(!lock.exists());
        // Nothing left behind, and a missing lock is fine.
        assert_eq!(fs::read_dir(dir.path()).expect("list").count(), 0);
        assert!(take_over_stale(&lock, "999999"));
    }

    #[test]
    fn taking_over_puts_back_a_lock_that_is_not_the_one_seen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let lock = dir.path().join("lock");
        // Another process replaced the stale lock with its own meanwhile.
        fs::write(&lock, "4242\n").expect("write");
        assert!(!take_over_stale(&lock, "999999"));
        assert_eq!(fs::read_to_string(&lock).expect("read"), "4242\n");
        assert_eq!(fs::read_dir(dir.path()).expect("list").count(), 1);
    }

    use super::*;

    fn sample() -> RunState {
        RunState::new(
            "20260101-120000-abcd",
            Path::new("plan.md"),
            Some("Plan".to_string()),
            AgentKind::Claude,
            "main",
            "phasewise/20260101-120000-abcd",
            "deadbeef",
            &[(1, "One".to_string()), (2, "Two".to_string())],
        )
    }

    #[test]
    fn v01_state_without_model_fields_still_loads() {
        let mut v: serde_json::Value = serde_json::to_value(sample()).unwrap();
        // A v0.1 file has none of the model fields.
        assert!(v.get("model_override").is_none());
        assert!(v["phases"][0].get("model").is_none());
        assert!(v["phases"][0].get("agent_model").is_none());
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "r1");
        std::fs::create_dir_all(paths.dir()).unwrap();
        std::fs::write(paths.state_file(), serde_json::to_vec(&v).unwrap()).unwrap();
        let loaded = RunState::load(&paths).unwrap();
        assert_eq!(loaded, sample_with_times(&loaded));
        assert_eq!(loaded.model_override, None);
        assert_eq!(loaded.phases[1].effort, None);
        assert_eq!(loaded.phases[1].agent_model, None);
        // And the new fields round-trip when set.
        v["model_override"] = "opus".into();
        v["phases"][0]["effort"] = "high".into();
        v["phases"][0]["agent_model"] = "claude-haiku-5-5".into();
        std::fs::write(paths.state_file(), serde_json::to_vec(&v).unwrap()).unwrap();
        let loaded = RunState::load(&paths).unwrap();
        assert_eq!(loaded.model_override.as_deref(), Some("opus"));
        assert_eq!(loaded.phases[0].effort.as_deref(), Some("high"));
        assert_eq!(
            loaded.phases[0].agent_model.as_deref(),
            Some("claude-haiku-5-5")
        );
    }

    /// `sample()` with the timestamps of `like` (they differ between calls).
    fn sample_with_times(like: &RunState) -> RunState {
        let mut s = sample();
        s.created_at = like.created_at;
        s.updated_at = like.updated_at;
        s
    }

    #[test]
    fn json_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "r1");
        let mut state = sample();
        state.phases[0].failure = Some(Failure {
            kind: FailureKind::Timeout,
            detail: "slow".to_string(),
        });
        state.phases[0].cost_usd = Some(0.5);
        state.save(&paths).unwrap();
        assert_eq!(RunState::load(&paths).unwrap(), state);
    }

    #[test]
    fn json_field_names_and_values() {
        let mut state = sample();
        state.phases[1].status = PhaseStatus::AwaitingReview;
        state.phases[0].failure = Some(Failure {
            kind: FailureKind::AgentError,
            detail: "x".to_string(),
        });
        let v: serde_json::Value = serde_json::to_value(&state).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["status"], "active");
        assert_eq!(v["agent"], "claude");
        assert_eq!(v["phases"][0]["status"], "pending");
        assert_eq!(v["phases"][1]["status"], "awaiting_review");
        assert_eq!(v["phases"][0]["failure"]["kind"], "agent_error");
        assert!(v["created_at"].is_string());
    }

    #[test]
    fn wrong_schema_version_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "r1");
        let mut value = serde_json::to_value(sample()).unwrap();
        value["schema_version"] = serde_json::json!(2);
        write_atomic(&paths.state_file(), value.to_string().as_bytes()).unwrap();
        let err = RunState::load(&paths).unwrap_err().to_string();
        assert!(
            err.starts_with("state.json has schema version 2; this phasewise understands 1."),
            "{err}"
        );
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("file.txt");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["file.txt".to_string()]);
    }

    #[test]
    fn run_id_format() {
        let now: Timestamp = "2026-03-04T05:06:07Z".parse().unwrap();
        let id = new_run_id(now);
        assert!(id.starts_with("20260304-050607-"), "{id}");
        let suffix = &id["20260304-050607-".len()..];
        assert_eq!(suffix.len(), 4);
        assert!(suffix.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
        assert_eq!(run_branch_name(&id), format!("phasewise/{id}"));
    }

    #[test]
    fn checkpoint_zero_is_base() {
        let mut state = sample();
        assert_eq!(state.checkpoint(0), Some("deadbeef"));
        assert_eq!(state.checkpoint(1), None);
        state.phase_mut(1).unwrap().checkpoint_sha = Some("abc".to_string());
        assert_eq!(state.checkpoint(1), Some("abc"));
        assert!(state.phase(0).is_none());
        assert!(state.phase(3).is_none());
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let long = "é".repeat(2500);
        let cut = truncate_summary(&long);
        assert_eq!(cut.chars().count(), 2000);
        assert_eq!(truncate_summary("short"), "short");
        let exact = "a".repeat(2000);
        assert_eq!(truncate_summary(&exact), exact);
    }

    #[test]
    fn paths_layout() {
        let paths = RunPaths::new(Path::new("/r"), "id");
        assert_eq!(
            paths.agent_stdout_log(3, 2),
            Path::new("/r/.phasewise/runs/id/phase-03.attempt-2.jsonl")
        );
        assert_eq!(
            paths.agent_stderr_log(3, 2),
            Path::new("/r/.phasewise/runs/id/phase-03.attempt-2.stderr.log")
        );
        assert_eq!(
            paths.verify_log(12),
            Path::new("/r/.phasewise/runs/id/phase-12.verify.log")
        );
    }

    fn temp_paths() -> (tempfile::TempDir, RunPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "r1");
        (dir, paths)
    }

    #[test]
    fn second_acquire_fails_and_names_the_lock_file() {
        let (_dir, paths) = temp_paths();
        let guard = LockGuard::acquire(&paths).unwrap();
        let err = LockGuard::acquire(&paths).unwrap_err().to_string();
        assert!(
            err.contains(&paths.lock_file().display().to_string()),
            "{err}"
        );
        assert!(err.contains(&std::process::id().to_string()), "{err}");
        drop(guard);
        assert!(!paths.lock_file().exists());
    }

    #[test]
    fn garbage_lock_is_stale() {
        let (_dir, paths) = temp_paths();
        fs::create_dir_all(paths.dir()).unwrap();
        fs::write(paths.lock_file(), "garbage").unwrap();
        let _guard = LockGuard::acquire(&paths).unwrap();
        assert_eq!(lock_holder(&paths), Some(std::process::id()));
    }

    #[test]
    fn empty_lock_is_refused() {
        let (_dir, paths) = temp_paths();
        fs::create_dir_all(paths.dir()).unwrap();
        fs::write(paths.lock_file(), "").unwrap();
        let err = LockGuard::acquire(&paths).unwrap_err().to_string();
        assert!(err.contains("in use"), "{err}");
        assert!(paths.lock_file().exists());
    }

    #[test]
    fn load_locked_recovers_despite_own_live_lock() {
        let (_dir, paths) = temp_paths();
        let mut state = sample();
        state.phases[0].status = PhaseStatus::Running;
        state.save(&paths).unwrap();
        let guard = LockGuard::acquire(&paths).unwrap();
        let unrecovered = load_with_recovery(&paths).unwrap();
        assert_eq!(unrecovered.phases[0].status, PhaseStatus::Running);
        let loaded = load_locked(&paths, &guard).unwrap();
        assert_eq!(loaded.phases[0].status, PhaseStatus::Failed);
        assert_eq!(
            loaded.phases[0].failure.as_ref().unwrap().kind,
            FailureKind::Interrupted
        );
        assert_eq!(RunState::load(&paths).unwrap(), loaded);
    }
}
