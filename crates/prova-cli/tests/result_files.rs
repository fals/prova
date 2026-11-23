//! Medium tests for level 1: result files found after the run (ADR 0013),
//! per-test history and flake statistics (ADR 0006).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-it1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir))
        .output()
        .expect("run prova")
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

/// A fake test suite: writes JUnit XML whose `b` test outcome comes from
/// the file `state` (pass or fail), so one command can pass and fail.
// A little real work first: results from a command that used no CPU are no
// evidence (round 10).
const SCRIPT: &str = r#"
b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done
mkdir -p build/test-results
if [ "$(cat state)" = pass ]; then B='<testcase classname="suite" name="b" time="0.010"/>'; code=0
else B='<testcase classname="suite" name="b" time="0.010"><failure message="b broke at 0x1f">trace</failure></testcase>'; code=1; fi
cat > build/test-results/junit.xml <<XML
<testsuite name="suite">
  <testcase classname="suite" name="a" time="0.001"/>
  $B
  <testcase classname="suite" name="c" time="0.002"><flakyFailure message="race"/></testcase>
</testsuite>
XML
exit $code
"#;

fn run(d: &Path, state: &str) -> serde_json::Value {
    std::fs::write(d.join("state"), state).unwrap();
    json(&prova(d, &["run", "--json", "--", "sh", "-c", SCRIPT]))
}

#[test]
fn result_files_lift_runs_to_level_one_with_per_test_outcomes() {
    let d = workspace("level1");
    let s = run(&d, "fail");
    assert_eq!(s["level"], "result_files");
    assert_eq!(
        s["counts"]["passed"], 2,
        "c passed on retry counts once, as passed"
    );
    assert_eq!(s["counts"]["failed"], 1);
    assert_eq!(s["failures"][0]["test"], "suite::b");
    assert_eq!(s["failures"][0]["context"]["seen_before"], 0);
}

#[test]
fn history_marks_flips_on_same_code_and_retry_passes_as_flaky() {
    let d = workspace("flaky");
    run(&d, "fail");
    run(&d, "pass");
    let third = run(&d, "fail");
    let ctx = &third["failures"][0]["context"];
    assert_eq!(
        ctx["seen_before"], 1,
        "same fingerprint despite volatile hex"
    );
    assert_eq!(ctx["flaky"], true);

    let h = json(&prova(&d, &["history", "suite::b", "--json"]));
    assert_eq!(h["runs"], 3);
    assert_eq!(h["failures"], 2);
    assert_eq!(h["transitions"], 2);
    assert_eq!(h["flips_on_same_code"], 1);

    let flaky = json(&prova(&d, &["flaky", "--json"]));
    let names: Vec<&str> = flaky
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["display"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"suite::b"), "{names:?}");
    assert!(names.contains(&"suite::c"), "passed on retry: {names:?}");
}

#[test]
fn diff_reports_newly_failing_and_fixed() {
    let d = workspace("diff");
    // A real change of code between the runs (a commit each): the flip is a
    // fix, not flakiness on the same code.
    let git = |args: &[&str]| {
        Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(&d)
            .status()
            .unwrap()
    };
    git(&["init", "-q"]);
    let committed = |state: &str| {
        std::fs::write(d.join("state"), state).unwrap();
        git(&["add", "state"]);
        git(&["commit", "-qm", state]);
        run(&d, state)["run"].as_str().unwrap().to_owned()
    };
    let a = committed("pass");
    let b = committed("fail");
    let o = prova(&d, &["diff", &a, &b, "--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(json(&o)["newly_failing"][0], "suite::b");
    let back = json(&prova(&d, &["diff", &b, &a, "--json"]));
    assert_eq!(back["fixed"][0], "suite::b");
}

/// ADR 0016: result files are parsed into the store and not copied; the
/// failing test's body is kept as its own output, and the run leaves no
/// files behind.
#[test]
fn result_files_are_distilled_not_copied() {
    let d = workspace("distilled");
    let s = run(&d, "fail");
    let store = PathBuf::from(s["store"].as_str().unwrap());
    assert!(
        !store.join("artifacts").exists(),
        "no copies of result files"
    );
    assert!(
        !store.join("runs").join(s["run"].as_str().unwrap()).exists(),
        "no run files"
    );
    let run_id = s["run"].as_str().unwrap();
    let o = prova(&d, &["output", run_id, "--test", "suite::b", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(json(&o)[0]["output"], "trace");
}

/// Isolated prova home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}
