//! Medium tests for level 1: result files found after the run (ADR 0013),
//! per-test history and flake statistics (ADR 0006).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOE: &str = env!("CARGO_BIN_EXE_toe");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("toe-it1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn toe(dir: &Path, args: &[&str]) -> Output {
    Command::new(TOE)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run toe")
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

/// A fake test suite: writes JUnit XML whose `b` test outcome comes from
/// the file `state` (pass or fail), so one command can pass and fail.
const SCRIPT: &str = r#"
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
    json(&toe(d, &["run", "--json", "--", "sh", "-c", SCRIPT]))
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

    let h = json(&toe(&d, &["history", "suite::b", "--json"]));
    assert_eq!(h["runs"], 3);
    assert_eq!(h["failures"], 2);
    assert_eq!(h["transitions"], 2);
    assert_eq!(h["flips_on_same_code"], 1);

    let flaky = json(&toe(&d, &["flaky", "--json"]));
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
    let a = run(&d, "pass")["run"].as_str().unwrap().to_owned();
    let b = run(&d, "fail")["run"].as_str().unwrap().to_owned();
    let o = toe(&d, &["diff", &a, &b, "--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(json(&o)["newly_failing"][0], "suite::b");
    let back = json(&toe(&d, &["diff", &b, &a, "--json"]));
    assert_eq!(back["fixed"][0], "suite::b");
}

#[test]
fn result_files_are_kept_as_artifacts() {
    let d = workspace("artifacts");
    run(&d, "pass");
    let n = std::fs::read_dir(d.join(".toe/artifacts")).unwrap().count();
    assert_eq!(n, 1);
}
