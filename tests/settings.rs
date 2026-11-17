//! `phasewise settings`: listing, setting, resetting and what a broken file does.
//! Every command runs with `PHASEWISE_CONFIG_DIR` pointing into a temporary directory.

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn phasewise(config: &TempDir) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("phasewise"));
    cmd.env("PHASEWISE_CONFIG_DIR", config.path())
        .env_remove("XDG_CONFIG_HOME");
    cmd
}

fn file(config: &TempDir) -> std::path::PathBuf {
    config.path().join("settings.toml")
}

#[test]
fn lists_every_setting_with_its_default_and_the_file_path() {
    let config = tempfile::tempdir().expect("temp dir");
    let assert = phasewise(&config)
        .arg("settings")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            file(&config).display().to_string(),
        ))
        .stdout(predicate::str::contains("DEFAULT"));
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for key in ["limits", "bell", "color", "agent", "model", "effort"] {
        assert!(
            out.lines()
                .any(|l| l.get(1..).is_some_and(|r| r.trim_start().starts_with(key))),
            "{key} missing in {out}"
        );
    }
    assert!(!out.lines().any(|l| l.starts_with('*')), "{out}");
    assert!(!file(&config).exists(), "listing must not create the file");
}

#[test]
fn set_saves_marks_the_change_and_reset_removes_it() {
    let config = tempfile::tempdir().expect("temp dir");
    phasewise(&config)
        .args(["settings", "set", "bell", "on"])
        .assert()
        .success()
        .stdout("bell = on\n");
    let text = std::fs::read_to_string(file(&config)).expect("file written");
    assert!(text.contains("bell = true"), "{text}");

    phasewise(&config)
        .args(["settings", "set", "agent", "codex"])
        .assert()
        .success();
    let out = phasewise(&config).arg("settings").output().expect("run");
    let out = String::from_utf8_lossy(&out.stdout).into_owned();
    let marked: Vec<&str> = out.lines().filter(|l| l.starts_with('*')).collect();
    assert_eq!(marked.len(), 2, "{out}");
    assert!(
        marked
            .iter()
            .any(|l| l.contains("bell") && l.contains("on"))
    );
    assert!(
        marked
            .iter()
            .any(|l| l.contains("agent") && l.contains("codex"))
    );

    phasewise(&config)
        .args(["settings", "reset", "bell"])
        .assert()
        .success();
    let text = std::fs::read_to_string(file(&config)).expect("file");
    assert!(!text.contains("bell"), "{text}");
    assert!(text.contains("agent = \"codex\""), "{text}");

    phasewise(&config)
        .args(["settings", "reset"])
        .assert()
        .success();
    let text = std::fs::read_to_string(file(&config)).expect("file");
    assert!(!text.contains("agent"), "{text}");
}

#[test]
fn a_bad_value_or_key_lists_what_is_valid_and_saves_nothing() {
    let config = tempfile::tempdir().expect("temp dir");
    phasewise(&config)
        .args(["settings", "set", "color", "rainbow"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected one of auto, never"));
    phasewise(&config)
        .args(["settings", "set", "bell", "maybe"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected on or off"));
    phasewise(&config)
        .args(["settings", "set", "nope", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "limits, bell, color, agent, model, effort",
        ));
    phasewise(&config)
        .args(["settings", "reset", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown setting"));
    assert!(!file(&config).exists());
}

#[test]
fn an_invalid_file_warns_and_is_kept_as_a_backup_when_replaced() {
    let config = tempfile::tempdir().expect("temp dir");
    std::fs::write(file(&config), "bell = [oops").expect("write");
    phasewise(&config)
        .arg("settings")
        .assert()
        .success()
        .stderr(predicate::str::contains("warning: Ignoring"));
    assert_eq!(
        std::fs::read_to_string(file(&config)).expect("file"),
        "bell = [oops",
        "listing leaves the file alone"
    );
    phasewise(&config)
        .args(["settings", "set", "limits", "off"])
        .assert()
        .success()
        .stderr(predicate::str::contains("settings.toml.bak"));
    assert_eq!(
        std::fs::read_to_string(config.path().join("settings.toml.bak")).expect("backup"),
        "bell = [oops"
    );
    assert!(
        std::fs::read_to_string(file(&config))
            .expect("file")
            .contains("limits = false")
    );
}

#[test]
fn the_directory_falls_back_to_xdg_then_home() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("phasewise"));
    cmd.env_remove("PHASEWISE_CONFIG_DIR")
        .env("XDG_CONFIG_HOME", dir.path())
        .args(["settings", "set", "bell", "on"])
        .assert()
        .success();
    assert!(dir.path().join("phasewise/settings.toml").exists());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("phasewise"));
    cmd.env_remove("PHASEWISE_CONFIG_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", dir.path())
        .args(["settings", "set", "bell", "on"])
        .assert()
        .success();
    assert!(dir.path().join(".config/phasewise/settings.toml").exists());
}

#[test]
fn parallel_sets_in_separate_processes_all_take_effect() {
    let changes = [
        ("limits", "off"),
        ("bell", "on"),
        ("color", "never"),
        ("agent", "codex"),
        ("model", "some-model"),
        ("effort", "low"),
    ];
    for _ in 0..5 {
        let config = tempfile::tempdir().expect("temp dir");
        let children: Vec<_> = changes
            .iter()
            .map(|(key, value)| {
                std::process::Command::new(assert_cmd::cargo::cargo_bin!("phasewise"))
                    .env("PHASEWISE_CONFIG_DIR", config.path())
                    .env_remove("XDG_CONFIG_HOME")
                    .args(["settings", "set", key, value])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn")
            })
            .collect();
        for mut child in children {
            assert!(child.wait().expect("wait").success());
        }
        let text = std::fs::read_to_string(file(&config)).expect("settings file");
        let table: toml::Table = text.parse().expect("valid TOML");
        assert_eq!(table.len(), changes.len(), "an update was lost: {text}");
        assert!(!config.path().join("settings.toml.lock").exists());
    }
}
