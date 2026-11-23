//! Medium tests for the central store (ADR 0014): one history per
//! repository across worktrees, migration of old workspace stores,
//! fallback, cross-repository views, run origin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-central-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(ws: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(PROVA);
    c.args(args).current_dir(ws).env("PROVA_HOME", home);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn git(ws: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(ws)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn worktrees_of_one_repository_share_history() {
    let base = dir("worktrees");
    let (main, wt, home) = (base.join("main"), base.join("wt"), base.join("home"));
    std::fs::create_dir_all(&main).unwrap();
    std::fs::write(main.join("f"), "x").unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "init"]);
    git(
        &main,
        &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
    );

    let script = [
        "run",
        "--json",
        "--",
        "sh",
        "-c",
        "echo 'test m::a ... FAILED'; exit 1",
    ];
    let a = json(&prova(&main, &home, &script, &[]));
    let b = json(&prova(&wt, &home, &script, &[]));
    assert_eq!(a["store"], b["store"], "one store per repository");
    assert_eq!(
        b["failures"][0]["context"]["seen_before"], 1,
        "the worktree sees the main checkout's failure"
    );
    assert!(!main.join(".prova").exists() && !wt.join(".prova").exists());
}

#[test]
fn old_workspace_store_is_imported_once() {
    let base = dir("migrate");
    let (ws, home) = (base.join("ws"), base.join("home"));
    std::fs::create_dir_all(&ws).unwrap();
    // An unusable home makes prova keep data in <ws>/.prova, like prova before ADR 0014.
    std::fs::write(base.join("not-a-dir"), "").unwrap();
    let blocked = base.join("not-a-dir/home");
    let old = prova(
        &ws,
        &blocked,
        &["run", "--json", "--", "sh", "-c", "echo 'test m::a ... ok'"],
        &[],
    );
    assert!(
        String::from_utf8_lossy(&old.stderr).contains("not writable"),
        "fallback is announced"
    );
    assert!(ws.join(".prova/prova.db").exists());
    let old_run = json(&old)["run"].as_str().unwrap().to_owned();

    let new = json(&prova(
        &ws,
        &home,
        &["run", "--json", "--", "sh", "-c", "echo 'test m::a ... ok'"],
        &[],
    ));
    assert_eq!(
        new["history"]["previous_runs"], 1,
        "imported history is visible"
    );
    assert!(ws.join(".prova/MOVED").exists() && !ws.join(".prova/prova.db").exists());
    let out = prova(&ws, &home, &["output", &old_run], &[]);
    assert!(out.status.success(), "captured output was imported too");
}

#[test]
fn status_and_flaky_across_repositories() {
    let base = dir("all");
    let home = base.join("home");
    for name in ["alpha", "beta"] {
        let ws = base.join(name);
        std::fs::create_dir_all(&ws).unwrap();
        prova(&ws, &home, &["run", "--", "true"], &[]);
    }
    let all = json(&prova(&base, &home, &["status", "--all", "--json"], &[]));
    let names: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["repo"]["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"alpha") && names.contains(&"beta"),
        "{names:?}"
    );
    assert!(
        prova(&base, &home, &["flaky", "--all"], &[])
            .status
            .success()
    );
}

#[test]
fn runs_record_where_they_ran() {
    let base = dir("origin");
    let (ws, home) = (base.join("ws"), base.join("home"));
    std::fs::create_dir_all(&ws).unwrap();
    prova(
        &ws,
        &home,
        &["run", "--", "true"],
        &[
            ("PROVA_ACTOR", "agent-7"),
            ("POD_NAME", "runner-abc"),
            ("NODE_NAME", "kind-worker"),
        ],
    );
    let runs = json(&prova(&ws, &home, &["status", "--json"], &[]));
    let origin = &runs[0]["origin"];
    assert_eq!(origin["actor"], "agent-7");
    assert_eq!(origin["pod"], "runner-abc");
    assert_eq!(origin["node"], "kind-worker");
    assert!(origin["host"].as_str().is_some_and(|h| !h.is_empty()));
}
