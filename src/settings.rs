//! User settings: a small TOML file in the user's configuration directory.
//!
//! This is the only thing phasewise writes outside the repository's
//! `.phasewise/` directory, and only when the user changes a setting. A missing
//! file means the defaults; an unreadable or invalid one means the defaults
//! plus a warning, and it is never overwritten without keeping a copy
//! (`settings.toml.bak`). Unknown keys are kept as they are.
//!
//! Every setting is one entry of [`REGISTRY`] plus its consumer: the menu, the
//! `settings` command and the validation all read the table.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

use crate::plan::{self, AgentKind};
use crate::state::write_atomic;

/// How long an update waits for another one to finish.
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// Name of the settings file inside the configuration directory.
pub const FILE_NAME: &str = "settings.toml";
/// Environment variable that replaces the configuration directory.
pub const CONFIG_DIR_ENV: &str = "PHASEWISE_CONFIG_DIR";
/// The terminal bell.
pub const BELL: &str = "\x07";

/// What values a setting takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `on` or `off`.
    Bool,
    /// One of the listed values.
    Choice(&'static [&'static str]),
    /// Free text; empty means "not set".
    Text,
}

/// One setting of the registry.
#[derive(Debug)]
pub struct Spec {
    pub key: &'static str,
    pub label: &'static str,
    /// One line, for the menu and the `settings` command.
    pub description: &'static str,
    pub kind: Kind,
    /// Default value as shown: `on`/`off` for a bool.
    pub default: &'static str,
}

impl Spec {
    /// The values a user may give, for error messages.
    pub fn values(&self) -> Vec<&'static str> {
        match self.kind {
            Kind::Bool => vec!["on", "off"],
            Kind::Choice(values) => values.to_vec(),
            Kind::Text => Vec::new(),
        }
    }

    /// The canonical form of a value given by the user, or what is wrong.
    pub fn normalize(&self, input: &str) -> Result<String, String> {
        let value = input.trim();
        match self.kind {
            Kind::Bool => match value.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => Ok("on".to_string()),
                "off" | "false" | "no" | "0" => Ok("off".to_string()),
                _ => Err(format!(
                    "Invalid value `{value}` for `{}`: expected on or off.",
                    self.key
                )),
            },
            Kind::Choice(values) => {
                let lower = value.to_ascii_lowercase();
                if values.contains(&lower.as_str()) {
                    Ok(lower)
                } else {
                    Err(format!(
                        "Invalid value `{value}` for `{}`: expected one of {}.",
                        self.key,
                        values.join(", ")
                    ))
                }
            }
            Kind::Text => {
                if value.is_empty() || value == "default" {
                    return Ok(String::new());
                }
                plan::validate_model(value)
                    .map(|()| value.to_string())
                    .map_err(|e| format!("Invalid value for `{}`: {e}.", self.key))
            }
        }
    }
}

/// Every setting, in the order of the menu.
pub static REGISTRY: &[Spec] = &[
    Spec {
        key: "limits",
        label: "Usage limits",
        description: "Show the agent's usage limits on the screens and in status.",
        kind: Kind::Bool,
        default: "on",
    },
    Spec {
        key: "bell",
        label: "Bell",
        description: "Ring the terminal bell when a phase needs you or the run completes.",
        kind: Kind::Bool,
        default: "off",
    },
    Spec {
        key: "color",
        label: "Colour",
        description: "auto uses colour on a terminal; never turns it off (NO_COLOR always wins).",
        kind: Kind::Choice(&["auto", "never"]),
        default: "auto",
    },
    Spec {
        key: "agent",
        label: "Agent",
        description: "Agent the home screen starts with when the plan does not point at one.",
        kind: Kind::Choice(&["plan", "claude", "codex"]),
        default: "plan",
    },
    Spec {
        key: "model",
        label: "Model",
        description: "Model the home screen starts with when the plan sets none (empty: the agent's own).",
        kind: Kind::Text,
        default: "",
    },
    Spec {
        key: "effort",
        label: "Effort",
        description: "Effort the home screen starts with when the plan sets none.",
        kind: Kind::Choice(&["default", "low", "medium", "high", "xhigh", "max"]),
        default: "default",
    },
];

/// The registry entry of `key`.
pub fn spec(key: &str) -> Option<&'static Spec> {
    REGISTRY.iter().find(|s| s.key == key)
}

/// The error for a key that is not a setting.
pub fn unknown_key(key: &str) -> String {
    let keys: Vec<&str> = REGISTRY.iter().map(|s| s.key).collect();
    format!(
        "Unknown setting `{key}`. The settings are: {}.",
        keys.join(", ")
    )
}

/// The values of the settings; what is not set has its default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    /// The whole file, so keys phasewise does not know survive a save.
    table: toml::Table,
}

// A table of settings never holds a NaN (a float is not a value of any
// setting and is only kept untouched), so equality is reflexive.
impl Eq for Settings {}

impl Settings {
    /// The value of `key` as shown (`on`/`off` for a bool); the default when
    /// the key is not set or its value is not valid. Empty for an unknown key.
    pub fn get(&self, key: &str) -> String {
        let Some(spec) = spec(key) else {
            return String::new();
        };
        self.stored(spec)
            .ok()
            .flatten()
            .unwrap_or_else(|| spec.default.to_string())
    }

    /// The stored value: `Ok(None)` when absent, `Err` when it is not valid.
    fn stored(&self, spec: &Spec) -> Result<Option<String>, String> {
        let Some(value) = self.table.get(spec.key) else {
            return Ok(None);
        };
        let bad = || {
            format!(
                "`{}` in the settings file is not valid; using the default ({}).",
                spec.key,
                shown(spec, spec.default)
            )
        };
        match (spec.kind, value) {
            (Kind::Bool, toml::Value::Boolean(b)) => {
                Ok(Some(if *b { "on" } else { "off" }.to_string()))
            }
            // The guide lists `on` and `off`, so the file accepts them as text too.
            (Kind::Bool, toml::Value::String(s)) if s == "on" || s == "off" => Ok(Some(s.clone())),
            (Kind::Choice(values), toml::Value::String(s)) if values.contains(&s.as_str()) => {
                Ok(Some(s.clone()))
            }
            (Kind::Text, toml::Value::String(s)) if plan::validate_model(s.trim()).is_ok() => {
                Ok(Some(s.trim().to_string()))
            }
            (Kind::Text, toml::Value::String(s)) if s.trim().is_empty() => Ok(None),
            _ => Err(bad()),
        }
    }

    /// Whether the value differs from the default.
    pub fn is_changed(&self, key: &str) -> bool {
        spec(key).is_some_and(|s| self.get(key) != s.default)
    }

    /// Sets `key` from a value given by the user. A value equal to the default
    /// removes the key, so the file only holds what differs.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let spec = spec(key).ok_or_else(|| unknown_key(key))?;
        let value = spec.normalize(value)?;
        if value == spec.default {
            self.table.remove(key);
        } else if spec.kind == Kind::Bool {
            self.table
                .insert(key.to_string(), toml::Value::Boolean(value == "on"));
        } else {
            self.table
                .insert(key.to_string(), toml::Value::String(value));
        }
        Ok(())
    }

    /// Moves a bool or choice setting to its next value (wrapping).
    pub fn cycle(&mut self, key: &str) {
        let Some(spec) = spec(key) else { return };
        let values = spec.values();
        if values.is_empty() {
            return;
        }
        let current = self.get(key);
        let at = values.iter().position(|v| *v == current).unwrap_or(0);
        let next = values[(at + 1) % values.len()];
        // The value comes from the registry, so it is valid.
        let _ = self.set(key, next);
    }

    /// Removes one setting (or all of them); true when something was removed.
    /// Keys that are not settings are kept.
    pub fn reset(&mut self, key: Option<&str>) -> bool {
        let keys: Vec<&str> = match key {
            Some(key) => vec![key],
            None => REGISTRY.iter().map(|s| s.key).collect(),
        };
        let mut removed = false;
        for k in keys {
            removed |= self.table.remove(k).is_some();
        }
        removed
    }

    /// Whether the usage limits are shown.
    pub fn limits(&self) -> bool {
        self.get("limits") == "on"
    }

    /// Whether the terminal bell rings.
    pub fn bell(&self) -> bool {
        self.get("bell") == "on"
    }

    /// Whether colour is turned off by the file.
    pub fn color_never(&self) -> bool {
        self.get("color") == "never"
    }

    /// The agent the home screen starts with; `None` leaves it to the plan.
    pub fn agent(&self) -> Option<AgentKind> {
        self.get("agent").parse().ok()
    }

    /// The model the home screen starts with.
    pub fn model(&self) -> Option<String> {
        Some(self.get("model")).filter(|m| !m.is_empty())
    }

    /// The effort the home screen starts with.
    pub fn effort(&self) -> Option<String> {
        Some(self.get("effort")).filter(|e| e != "default")
    }

    /// The file's text.
    fn to_toml(&self) -> Result<String> {
        let body = toml::to_string(&self.table).context("Cannot encode the settings")?;
        Ok(format!(
            "# phasewise user settings. Edit by hand or use `phasewise settings`.\n{body}"
        ))
    }
}

/// A value for display: an empty text is shown as `(default)`.
pub fn shown(spec: &Spec, value: &str) -> String {
    if spec.kind == Kind::Text && value.is_empty() {
        "(default)".to_string()
    } else {
        value.to_string()
    }
}

/// What reading the file gave.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    pub settings: Settings,
    /// Problems found, one line each; the settings still work (defaults).
    pub warnings: Vec<String>,
}

/// The configuration directory from the environment values: the override,
/// else `$XDG_CONFIG_HOME/phasewise`, else `$HOME/.config/phasewise`. Empty
/// values count as unset.
pub fn config_dir(config: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|v| !v.is_empty())
    }
    if let Some(dir) = set(config) {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = set(xdg) {
        return Some(Path::new(dir).join("phasewise"));
    }
    set(home).map(|h| Path::new(h).join(".config").join("phasewise"))
}

/// Path of the settings file for this process's environment.
pub fn settings_path() -> Option<PathBuf> {
    let var = |name| std::env::var(name).ok();
    config_dir(
        var(CONFIG_DIR_ENV).as_deref(),
        var("XDG_CONFIG_HOME").as_deref(),
        var("HOME").as_deref(),
    )
    .map(|dir| dir.join(FILE_NAME))
}

/// Like [`settings_path`], with an error that says what to do.
pub fn require_path() -> Result<PathBuf> {
    settings_path().ok_or_else(|| {
        anyhow!("Cannot find the configuration directory. Set HOME or {CONFIG_DIR_ENV}.")
    })
}

/// Reads the settings file; never fails (see the module docs).
pub fn load_from(path: &Path) -> Loaded {
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| "it is not valid UTF-8".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
        Err(e) => Err(format!("it cannot be read ({e})")),
    };
    let table = text.and_then(|t| {
        t.parse::<toml::Table>()
            .map_err(|e| format!("it is not valid TOML ({})", e.message()))
    });
    match table {
        Ok(table) => {
            let settings = Settings { table };
            let warnings = REGISTRY
                .iter()
                .filter_map(|spec| settings.stored(spec).err())
                .collect();
            Loaded { settings, warnings }
        }
        Err(why) => Loaded {
            settings: Settings::default(),
            warnings: vec![format!(
                "Ignoring {}: {why}. Using the default settings; fix the file or change a setting to replace it (a copy is kept as {FILE_NAME}.bak).",
                path.display()
            )],
        },
    }
}

/// Reads the settings of this process's environment.
pub fn load() -> Loaded {
    settings_path().map_or_else(Loaded::default, |path| load_from(&path))
}

/// The settings for a command-line run: read once, warnings on stderr.
pub fn current() -> &'static Settings {
    static CURRENT: OnceLock<Settings> = OnceLock::new();
    CURRENT.get_or_init(|| {
        let loaded = load();
        for warning in &loaded.warnings {
            eprintln!("warning: {warning}");
        }
        loaded.settings
    })
}

/// Writes the settings atomically. An existing file that is not valid TOML is
/// first copied to `settings.toml.bak`; the copy's path is returned then.
pub fn save_to(path: &Path, settings: &Settings) -> Result<Option<PathBuf>> {
    let mut backup = None;
    if let Ok(old) = std::fs::read(path) {
        let valid = String::from_utf8(old)
            .ok()
            .is_some_and(|t| t.parse::<toml::Table>().is_ok());
        if !valid {
            let bak = path.with_file_name(format!("{FILE_NAME}.bak"));
            std::fs::copy(path, &bak).with_context(|| {
                format!(
                    "Cannot keep a copy of {} as {}",
                    path.display(),
                    bak.display()
                )
            })?;
            backup = Some(bak);
        }
    }
    write_atomic(path, settings.to_toml()?.as_bytes())
        .with_context(|| format!("Cannot save the settings to {}", path.display()))?;
    Ok(backup)
}

/// An update that has held the lock this long has crashed (an update takes
/// milliseconds), so a waiting one removes the lock.
const STALE_AFTER: Duration = Duration::from_secs(10);

/// The lock file of `settings.toml`: created exclusively, removed when dropped.
struct SettingsLock {
    path: PathBuf,
}

impl SettingsLock {
    /// Waits for the lock for up to `LOCK_WAIT`. Unlike a run's lock it has no
    /// owner check: it only exists while a settings update runs, and a lock
    /// older than `STALE_AFTER` is the leftover of a crash.
    fn acquire(path: &Path) -> Result<SettingsLock> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    // The PID only helps whoever reads the file; it is not checked.
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(SettingsLock {
                        path: path.to_path_buf(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok());
                    if age.is_some_and(|age| age > STALE_AFTER) {
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(anyhow!(
                            "The settings are being changed by another phasewise process \
(lock file: {}). Try again; if no phasewise process is running, delete the lock file.",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(anyhow!(e))
                        .with_context(|| format!("Cannot create {}", path.display()));
                }
            }
        }
    }
}

impl Drop for SettingsLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Reads the settings, applies `change` and saves them, all under a lock file
/// (`settings.toml.lock`, next to the settings) so that concurrent updates do
/// not lose each other's changes. A lock left by a crash is removed once it is
/// older than ten seconds. `change` gets the loaded settings and their
/// warnings and returns whether to save them. Returns the backup path, if one
/// was made.
pub fn update_at<F>(path: &Path, change: F) -> Result<Option<PathBuf>>
where
    F: FnOnce(&mut Settings, &[String]) -> Result<bool>,
{
    let _lock = SettingsLock::acquire(&path.with_file_name(format!("{FILE_NAME}.lock")))?;
    let loaded = load_from(path);
    let mut settings = loaded.settings;
    if change(&mut settings, &loaded.warnings)? {
        save_to(path, &settings)
    } else {
        Ok(None)
    }
}

/// Writes the settings of this process's environment.
pub fn save(settings: &Settings) -> Result<Option<PathBuf>> {
    update_at(&require_path()?, |current, _| {
        *current = settings.clone();
        Ok(true)
    })
}

/// Whether to ring the bell: it is enabled and the output is a terminal (a
/// bell in a pipe or a log is noise).
pub fn bell_wanted(enabled: bool, is_terminal: bool) -> bool {
    enabled && is_terminal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn settings_with(text: &str) -> Settings {
        Settings {
            table: text.parse().expect("valid toml"),
        }
    }

    #[test]
    fn the_directory_follows_the_override_then_xdg_then_home() {
        let dir = |c, x, h| config_dir(c, x, h);
        assert_eq!(dir(Some("/c"), Some("/x"), Some("/h")), Some("/c".into()));
        assert_eq!(
            dir(None, Some("/x"), Some("/h")),
            Some("/x/phasewise".into())
        );
        assert_eq!(
            dir(Some(""), Some(""), Some("/h")),
            Some("/h/.config/phasewise".into())
        );
        assert_eq!(dir(None, None, None), None);
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let dir = temp();
        let loaded = load_from(&dir.path().join(FILE_NAME));
        assert_eq!(loaded, Loaded::default());
        assert!(loaded.settings.limits());
        assert!(!loaded.settings.bell());
        assert!(!loaded.settings.color_never());
        assert_eq!(loaded.settings.agent(), None);
        assert_eq!(loaded.settings.model(), None);
        assert_eq!(loaded.settings.effort(), None);
    }

    #[test]
    fn values_are_read_and_unknown_keys_are_ignored() {
        let dir = temp();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(
            &path,
            "limits = false\nbell = true\ncolor = \"never\"\nagent = \"codex\"\nmodel = \"gpt-5\"\neffort = \"high\"\nfuture = 3\n",
        )
        .expect("write");
        let loaded = load_from(&path);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let s = loaded.settings;
        assert!(!s.limits());
        assert!(s.bell());
        assert!(s.color_never());
        assert_eq!(s.agent(), Some(AgentKind::Codex));
        assert_eq!(s.model().as_deref(), Some("gpt-5"));
        assert_eq!(s.effort().as_deref(), Some("high"));
        assert!(s.is_changed("limits"));
    }

    #[test]
    fn an_invalid_value_falls_back_to_its_default_with_a_warning() {
        let dir = temp();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "limits = \"maybe\"\ncolor = \"never\"\n").expect("write");
        let loaded = load_from(&path);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("`limits`"));
        assert!(loaded.settings.limits());
        assert!(loaded.settings.color_never());
    }

    #[test]
    fn a_bool_setting_accepts_on_and_off_as_text_in_the_file() {
        let dir = temp();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "bell = \"on\"\nlimits = \"off\"\n").expect("write");
        let loaded = load_from(&path);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.settings.get("bell"), "on");
        assert!(!loaded.settings.limits());
        // Other text is still invalid and falls back to the default.
        std::fs::write(&path, "bell = \"yes\"\n").expect("write");
        let loaded = load_from(&path);
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.settings.get("bell"), "off");
    }

    #[test]
    fn an_invalid_file_gives_the_defaults_a_warning_and_a_backup_on_save() {
        let dir = temp();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "limits = [broken").expect("write");
        let loaded = load_from(&path);
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("Ignoring"));
        // Loading never touches the file.
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "limits = [broken"
        );

        let mut settings = loaded.settings;
        settings.set("bell", "on").expect("valid");
        let backup = save_to(&path, &settings).expect("saved");
        let backup = backup.expect("a backup was made");
        assert_eq!(backup, dir.path().join("settings.toml.bak"));
        assert_eq!(
            std::fs::read_to_string(backup).expect("read"),
            "limits = [broken"
        );
        assert!(load_from(&path).settings.bell());
    }

    #[test]
    fn saving_over_a_valid_file_makes_no_backup_and_keeps_unknown_keys() {
        let dir = temp();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "future = \"x\"\n[nested]\na = 1\n").expect("write");
        let mut settings = load_from(&path).settings;
        settings.set("limits", "off").expect("valid");
        assert_eq!(save_to(&path, &settings).expect("saved"), None);
        assert!(!dir.path().join("settings.toml.bak").exists());
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("future = \"x\""), "{text}");
        assert!(text.contains("[nested]"), "{text}");
        assert!(text.contains("limits = false"), "{text}");
        // No temporary file is left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(FILE_NAME)]);
    }

    #[test]
    fn saving_creates_the_directory() {
        let dir = temp();
        let path = dir.path().join("a").join("b").join(FILE_NAME);
        save_to(&path, &Settings::default()).expect("saved");
        assert!(path.exists());
    }

    #[test]
    fn parallel_updates_do_not_lose_each_other() {
        let changes = [
            ("limits", "off"),
            ("bell", "on"),
            ("color", "never"),
            ("agent", "codex"),
            ("model", "some-model"),
            ("effort", "low"),
        ];
        for _ in 0..20 {
            let dir = temp();
            let path = dir.path().join(FILE_NAME);
            std::thread::scope(|scope| {
                for (key, value) in changes {
                    let path = &path;
                    scope.spawn(move || {
                        update_at(path, |s, _| {
                            s.set(key, value).map_err(|e| anyhow!(e))?;
                            Ok(true)
                        })
                        .expect("updated");
                    });
                }
            });
            let loaded = load_from(&path);
            for (key, value) in changes {
                assert_eq!(loaded.settings.get(key), value, "{key} was lost");
            }
            assert!(!path.with_file_name("settings.toml.lock").exists());
        }
    }

    #[test]
    fn a_lock_left_by_a_crash_does_not_block_and_reading_makes_no_lock() {
        let dir = temp();
        let path = dir.path().join(FILE_NAME);
        let lock = dir.path().join("settings.toml.lock");
        load_from(&path);
        assert!(!lock.exists());
        std::fs::write(&lock, "4000000000\n").expect("lock");
        let old = std::time::SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .expect("open lock")
            .set_modified(old)
            .expect("age the lock");
        update_at(&path, |s, _| {
            s.set("bell", "on").map_err(|e| anyhow!(e))?;
            Ok(true)
        })
        .expect("updated");
        assert_eq!(load_from(&path).settings.get("bell"), "on");
        assert!(!lock.exists());
    }

    #[test]
    fn a_fresh_lock_blocks_until_it_is_released() {
        let dir = temp();
        let path = dir.path().join(FILE_NAME);
        let lock = dir.path().join("settings.toml.lock");
        std::fs::write(&lock, "1\n").expect("lock");
        let release = lock.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            std::fs::remove_file(release).expect("release");
        });
        let started = Instant::now();
        update_at(&path, |s, _| {
            s.set("bell", "on").map_err(|e| anyhow!(e))?;
            Ok(true)
        })
        .expect("updated");
        assert!(started.elapsed() >= Duration::from_millis(150));
        handle.join().expect("join");
        assert_eq!(load_from(&path).settings.get("bell"), "on");
    }

    #[test]
    fn set_validates_against_the_registry() {
        let mut s = Settings::default();
        assert!(s.set("bell", "yes").is_ok());
        assert_eq!(s.get("bell"), "on");
        assert!(s.set("color", "NEVER").is_ok());
        assert_eq!(s.get("color"), "never");
        let err = s.set("color", "rainbow").expect_err("invalid");
        assert!(err.contains("auto, never"), "{err}");
        let err = s.set("effort", "huge").expect_err("invalid");
        assert!(
            err.contains("default, low, medium, high, xhigh, max"),
            "{err}"
        );
        let err = s.set("bell", "maybe").expect_err("invalid");
        assert!(err.contains("on or off"), "{err}");
        let err = s.set("nope", "x").expect_err("unknown");
        assert!(err.contains("limits, bell, color"), "{err}");
        assert_eq!(s.get("color"), "never", "a bad value changes nothing");
    }

    #[test]
    fn a_default_value_removes_the_key() {
        let mut s = Settings::default();
        s.set("limits", "off").expect("valid");
        assert!(s.is_changed("limits"));
        s.set("limits", "on").expect("valid");
        assert!(!s.is_changed("limits"));
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn model_is_free_text_and_default_clears_it() {
        let mut s = Settings::default();
        s.set("model", "  opus ").expect("valid");
        assert_eq!(s.model().as_deref(), Some("opus"));
        s.set("model", "default").expect("valid");
        assert_eq!(s.model(), None);
        assert!(s.set("model", "bad model").is_err());
    }

    #[test]
    fn cycle_walks_the_values_and_wraps() {
        let mut s = Settings::default();
        s.cycle("bell");
        assert!(s.bell());
        s.cycle("bell");
        assert!(!s.bell());
        let seen: Vec<String> = (0..4)
            .map(|_| {
                s.cycle("agent");
                s.get("agent")
            })
            .collect();
        assert_eq!(seen, ["claude", "codex", "plan", "claude"]);
        s.cycle("model");
        assert_eq!(s.get("model"), "");
    }

    #[test]
    fn reset_removes_one_or_all_settings_but_not_other_keys() {
        let mut s = settings_with("bell = true\nlimits = false\nfuture = 1\n");
        assert!(s.reset(Some("bell")));
        assert!(!s.reset(Some("bell")));
        assert!(s.is_changed("limits"));
        assert!(s.reset(None));
        assert!(!s.is_changed("limits"));
        assert!(s.to_toml().expect("toml").contains("future = 1"));
    }

    #[test]
    fn the_registry_is_consistent() {
        for spec in REGISTRY {
            // The default is a valid value, and is what an empty table gives.
            assert_eq!(
                spec.normalize(spec.default).as_deref(),
                Ok(spec.default),
                "{}",
                spec.key
            );
            assert_eq!(Settings::default().get(spec.key), spec.default);
            assert!(!spec.description.is_empty() && !spec.label.is_empty());
        }
        let mut keys: Vec<_> = REGISTRY.iter().map(|s| s.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), REGISTRY.len(), "keys are unique");
    }

    #[test]
    fn the_bell_needs_the_setting_and_a_terminal() {
        assert!(bell_wanted(true, true));
        assert!(!bell_wanted(true, false));
        assert!(!bell_wanted(false, true));
        assert!(!bell_wanted(false, false));
    }
}
