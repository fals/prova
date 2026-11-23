//! Medium tests for ADR 0019 (rename toe to prova): history left under the
//! old names is moved once, and an old config file is refused with a hint.
//! Real directories and stores are the point, so these cannot be smaller.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-rename-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("ws")).unwrap();
    d
}

/// prova with only the default home locations (no PROVA_HOME).
fn prova(base: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(cwd)
        .env_remove("PROVA_HOME")
        .env("XDG_DATA_HOME", base.join("data"))
        .env("HOME", base)
        .output()
        .expect("run prova")
}

fn out(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn an_old_toe_home_is_moved_with_its_history() {
    let base = workspace("home");
    let ws = base.join("ws");
    // Record a run, then lay the home out as toe left it.
    assert!(prova(&base, &ws, &["run", "--", "true"]).status.success());
    let run =
        String::from_utf8_lossy(&prova(&base, &ws, &["status", "--json"]).stdout).into_owned();
    let new = base.join("data/prova");
    let old = base.join("data/toe");
    std::fs::rename(&new, &old).unwrap();
    for e in std::fs::read_dir(old.join("repos")).unwrap().flatten() {
        std::fs::rename(e.path().join("prova.db"), e.path().join("toe.db")).unwrap();
        let _ = std::fs::remove_file(e.path().join("prova.db-wal"));
        let _ = std::fs::remove_file(e.path().join("prova.db-shm"));
    }
    let after = prova(&base, &ws, &["status", "--json"]);
    assert!(after.status.success(), "{}", out(&after));
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        run,
        "same history after the move"
    );
    assert!(!old.exists(), "moved, not copied");
    let repo = std::fs::read_dir(new.join("repos"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    assert!(repo.join("prova.db").exists() && !repo.join("toe.db").exists());
}

#[test]
fn a_workspace_toe_folder_becomes_prova() {
    let base = workspace("ws");
    let ws = base.join("ws");
    std::fs::create_dir_all(ws.join(".toe/stable/bin")).unwrap();
    std::fs::write(ws.join(".toe/stable/bin/marker"), "x").unwrap();
    let o = prova(&base, &ws, &["status"]);
    assert!(o.status.success(), "{}", out(&o));
    assert!(ws.join(".prova/stable/bin/marker").exists() && !ws.join(".toe").exists());
}

#[test]
fn an_old_config_file_is_refused_with_the_new_name() {
    let base = workspace("cfg");
    let ws = base.join("ws");
    std::fs::write(ws.join("toe.toml"), "[suite.t]\nrun = \"true\"\n").unwrap();
    let o = prova(&base, &ws, &["run", "t"]);
    assert!(!o.status.success());
    let text = out(&o);
    assert!(text.contains("rename it to prova.toml"), "{text}");
}
