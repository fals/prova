//! Medium tests for ADR 0017: a pass needs evidence. Each case is a command
//! printing what a real framework prints; real processes are needed because
//! the verdict combines the exit code, the output and the store's history.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-nfp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", format!("{}.home", dir.display()))
        .output()
        .expect("run prova")
}

fn run_sh(dir: &Path, script: &str) -> (Option<i32>, serde_json::Value) {
    let o = prova(dir, &["run", "--json", "--", "sh", "-c", script]);
    let out = String::from_utf8_lossy(&o.stdout);
    let last = out
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .unwrap_or("{}");
    (o.status.code(), serde_json::from_str(last).unwrap())
}

/// (case, script, expected outcome, expected exit, text the explanation contains)
const CASES: &[(&str, &str, &str, i32, &str)] = &[
    (
        "libtest_zero",
        "echo 'running 0 tests'; echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out'",
        "failed",
        1,
        "counts 0 tests",
    ),
    (
        "go_no_test_files",
        "printf '?   \\texample.com/x\\t[no test files]\\n'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "jest_no_tests",
        "echo 'No tests found, exiting with code 0'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "maven_no_tests",
        "echo '[INFO] No tests to run.'; echo '[INFO] BUILD SUCCESS'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "vstest_no_tests",
        "echo 'No test is available in /x.dll.'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "pytest_exit_5",
        "echo 'no tests ran in 0.01s'; exit 5",
        "failed",
        1,
        "no test ran",
    ),
    (
        "unfinished_test",
        "echo '=== RUN   TestA'",
        "failed",
        1,
        "did not finish",
    ),
    (
        "summary_beats_exit",
        "echo 'test a ... ok'; echo 'test result: FAILED. 1 passed; 3 failed'",
        "failed",
        1,
        "reported 3 failed",
    ),
    (
        "not_found",
        "nosuchtestrunner",
        "errored",
        4,
        "could not start",
    ),
    (
        "build_error",
        "echo 'error[E0308]: mismatched types'; echo 'error: could not compile'; exit 101",
        "failed",
        1,
        "first error at line 1",
    ),
];

#[test]
fn contradicted_passes_fail_and_explain_themselves() {
    for (name, script, outcome, exit, says) in CASES {
        let d = workspace(name);
        let (code, s) = run_sh(&d, script);
        assert_eq!(s["outcome"], *outcome, "{name}: {s}");
        assert_eq!(code, Some(*exit), "{name}: {s}");
        let detail = s["detail"].as_str().unwrap_or("");
        assert!(detail.contains(says), "{name}: detail {detail:?}");
        assert_eq!(s["verified"], false, "{name}");
    }
}

#[test]
fn thin_passes_are_marked_unverified() {
    for (name, script, says) in [
        ("empty", "true", "no test was observed"),
        (
            "all_skipped",
            "echo 'test a ... ignored'; echo 'test result: ok. 0 passed; 0 failed; 1 ignored'",
            "were skipped",
        ),
        (
            "crash_after_summary",
            "echo 'test a ... ok'; echo 'test result: ok. 1 passed'; echo 'thread main panicked at boom'",
            "error outside any failing test",
        ),
    ] {
        let d = workspace(name);
        let (code, s) = run_sh(&d, script);
        assert_eq!(
            (s["outcome"].as_str(), code),
            (Some("passed"), Some(0)),
            "{name}: {s}"
        );
        assert_eq!(s["verified"], false, "{name}");
        assert!(s["warnings"].to_string().contains(says), "{name}: {s}");
        let human = String::from_utf8_lossy(&prova(&d, &["status", "last"]).stdout).into_owned();
        assert!(
            human.contains("PASSED (unverified") && human.contains("warning: "),
            "{name}: {human}"
        );
    }
    let d = workspace("clean");
    let (_, s) = run_sh(
        &d,
        "echo 'test a ... ok'; echo 'test result: ok. 1 passed; 0 failed'",
    );
    assert_eq!(
        (s["outcome"].as_str(), s["verified"].as_bool()),
        (Some("passed"), Some(true))
    );
    assert!(s.get("warnings").is_none());
}

#[test]
fn tests_that_disappear_are_noticed() {
    let d = workspace("vanish");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"sh t.sh\"\n\n[suite.lint]\nrun = \"true\"\nexpect_tests = false\n",
    )
    .unwrap();
    let run = |script: &str| {
        std::fs::write(d.join("t.sh"), script).unwrap();
        let o = prova(&d, &["run", "t", "--json"]);
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        serde_json::from_str::<serde_json::Value>(
            out.lines().rev().find(|l| l.starts_with('{')).unwrap(),
        )
        .unwrap()
    };
    run("echo 'test a ... ok'; echo 'test b ... ok'; echo 'test c ... ok'");
    let some = run("echo 'test a ... ok'");
    assert_eq!(some["outcome"], "passed");
    let w = some["warnings"].to_string();
    assert!(
        w.contains("2 test(s) from the last passing run") && w.contains("b, c"),
        "{w}"
    );
    let none = run("echo 'built, no test output'");
    assert_eq!(none["outcome"], "failed", "{none}");
    assert!(none["detail"].as_str().unwrap().contains("observed none"));
    // A suite that declares it runs no tests stays a clean pass.
    let o = prova(&d, &["run", "lint", "--json"]);
    let lint: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        (lint["outcome"].as_str(), lint["verified"].as_bool()),
        (Some("passed"), Some(true))
    );
}
