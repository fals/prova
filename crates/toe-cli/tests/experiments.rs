//! Medium tests for experiments (ADR 0007): real git worktrees, real
//! workloads, real verdicts. Workloads sleep a few ms; trials are few.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOE: &str = env!("CARGO_BIN_EXE_toe");

fn toe(dir: &Path, args: &[&str]) -> Output {
    Command::new(TOE)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run toe")
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

fn sh(dir: &Path, script: &str) {
    assert!(
        Command::new("sh")
            .args(["-c", script])
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "{script}"
    );
}

/// Git repo whose workload sleeps for the number of seconds in `delay`.
fn repo(name: &str, delay: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("toe-exp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("delay"), delay).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\nsleep $(cat delay)\necho '{\"metric\":\"mem_kb\",\"value\":100}'\n",
    )
    .unwrap();
    std::fs::write(d.join(".gitignore"), ".toe/\n").unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    d
}

fn new_exp(d: &Path, extra: &[&str]) {
    let mut a = vec![
        "exp",
        "new",
        "e",
        "--hypothesis",
        "h",
        "--trials",
        "5",
        "--max-trials",
        "6",
        "--warmup",
        "0",
        "--guardrail",
        "mem_kb",
    ];
    a.extend_from_slice(extra);
    a.extend_from_slice(&["--", "./work.sh"]);
    let o = toe(d, &a);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn faster_candidate_is_accepted_and_becomes_best() {
    let d = repo("accept", "0.06");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.02").unwrap();
    assert!(toe(&d, &["exp", "candidate", "e"]).status.success());
    let o = toe(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(o.status.code(), Some(0), "{r}");
    assert_eq!(r["comparison"]["verdict"], "accept");
    assert_eq!(r["best"], "c1");
    assert_eq!(r["decisions"][0]["decision"], "keep");
    assert!(
        r["comparison"]["result"]["primary"]["improvement"]
            .as_f64()
            .unwrap()
            > 0.3
    );
    // Revalidation runs the best against the original baseline afresh.
    let v = json(&toe(&d, &["exp", "revalidate", "e", "--json"]));
    assert_eq!(v["comparison"]["candidate"], "c1");
    assert_eq!(v["comparison"]["baseline"], "baseline");
    assert_eq!(
        v["comparison"]["result"]["comparisons_in_family"], 2,
        "Holm family grows"
    );
}

#[test]
fn same_code_is_not_an_improvement() {
    let d = repo("noeffect", "0.02");
    new_exp(&d, &["--min-effect", "20%"]);
    assert!(toe(&d, &["exp", "candidate", "e"]).status.success());
    let o = toe(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(o.status.code(), Some(1), "{r}");
    assert_eq!(r["comparison"]["verdict"], "reject");
    assert_eq!(r["best"], "baseline");
    assert_eq!(r["decisions"][0]["decision"], "discard");
}

#[test]
fn protocol_is_frozen_after_the_first_trial_and_workspace_changes_are_caught() {
    let d = repo("frozen", "0.01");
    new_exp(&d, &[]);
    assert!(
        toe(
            &d,
            &["exp", "protocol", "e", "--trials", "6", "--max-trials", "8"]
        )
        .status
        .success(),
        "before trials: allowed"
    );
    std::fs::write(d.join("delay"), "0.005").unwrap();
    toe(&d, &["exp", "candidate", "e"]);
    std::fs::write(d.join("delay"), "0.007").unwrap(); // edited after recording
    let stale = toe(&d, &["exp", "run", "e"]);
    assert_eq!(stale.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&stale.stderr).contains("workspace changed"));
    toe(&d, &["exp", "candidate", "e"]);
    toe(&d, &["exp", "run", "e", "--candidate", "c2"]);
    let frozen = toe(&d, &["exp", "protocol", "e", "--min-effect", "0.5%"]);
    assert_eq!(frozen.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&frozen.stderr).contains("protocol frozen"));
}

#[test]
fn crashing_candidate_is_logged_as_crash() {
    let d = repo("crash", "0.01");
    new_exp(&d, &[]);
    std::fs::write(d.join("work.sh"), "#!/bin/sh\nexit 3\n").unwrap();
    toe(&d, &["exp", "candidate", "e"]);
    let r = json(&toe(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed");
    assert_eq!(r["decisions"][0]["decision"], "crash");
}
