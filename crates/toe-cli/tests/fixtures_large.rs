//! Large tests: real fixture projects with real toolchains (ADR 0005).
//! Opt-in: `cargo test -p toe-cli --test fixtures_large -- --ignored`
//! (needs go and cargo on PATH).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOE: &str = env!("CARGO_BIN_EXE_toe");

fn copy_fixture(name: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    let d = std::env::temp_dir().join(format!("toe-large-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    assert!(
        Command::new("cp")
            .arg("-r")
            .arg(&src)
            .arg(&d)
            .status()
            .unwrap()
            .success()
    );
    d
}

fn toe(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(TOE);
    c.args(args).current_dir(dir);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

fn check_fixture(name: &str, bug: &str) {
    let d = copy_fixture(name);
    let s = json(&toe(&d, &["run", "--json", "--", "make", "test"], &[]));
    assert_eq!(s["outcome"], "failed");
    assert_ne!(s["level"], "opaque", "{s}");
    let failing: Vec<&str> = s["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["test"].as_str().unwrap())
        .collect();
    assert!(failing.iter().any(|t| t.contains(bug)), "{failing:?}");

    // A hang is caught as a stall without any per-test configuration.
    let id = json(&toe(
        &d,
        &[
            "run",
            "--detach",
            "--json",
            "--stall-after",
            "3s",
            "--",
            "make",
            "test",
        ],
        &[("HANG", "1")],
    ))["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let w = toe(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "stall",
            "--max-wait",
            "120s",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        json(&w)["reason"],
        "stall",
        "{}",
        String::from_utf8_lossy(&w.stdout)
    );
    toe(&d, &["cancel", &id], &[]);
    toe(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "30s"],
        &[],
    );
}

#[test]
#[ignore = "large: needs go"]
fn go_fixture_end_to_end() {
    check_fixture("go-make", "TestDiv");
}

#[test]
#[ignore = "large: needs cargo, compiles a crate"]
fn rust_fixture_end_to_end() {
    check_fixture("rust-make", "divides");
}
