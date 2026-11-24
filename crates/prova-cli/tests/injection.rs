//! Medium tests: opt-in environment injection (ADR 0013 section 6, #33).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-inject-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(PROVA);
    c.args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir))
        .env_remove("PYTEST_ADDOPTS")
        .env_remove("GOFLAGS");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

/// Pretends to be pytest: writes JUnit where PYTEST_ADDOPTS asks, if asked.
const FAKE_PYTEST: &str = r#"
out=$(printf '%s' "$PYTEST_ADDOPTS" | sed -n 's/.*--junitxml=\([^ ]*\).*/\1/p')
echo "1 failed, 2 passed in 0.01s"
[ -n "$out" ] && cat > "$out" <<XML
<testsuite name="pytest"><testcase classname="tests.test_m" name="test_a"/><testcase classname="tests.test_m" name="test_b"/><testcase classname="tests.test_m" name="test_c"><failure message="boom"/></testcase></testsuite>
XML
exit 1
"#;

#[test]
fn nothing_is_injected_unless_asked() {
    let d = workspace("off");
    let s = json(&prova(
        &d,
        &["run", "--json", "--", "sh", "-c", FAKE_PYTEST],
        &[],
    ));
    assert_eq!(s["level"], "opaque");
    assert!(
        s["notes"]
            .as_array()
            .is_none_or(|n| n.iter().all(|m| !m.as_str().unwrap().contains("injected")))
    );
}

#[test]
fn pytest_junit_recipe_lifts_the_run_and_is_visible() {
    let d = workspace("pytest");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.py]\nrun = \"sh fake.sh\"\ninject = [\"pytest-junit\"]\n",
    )
    .unwrap();
    std::fs::write(d.join("fake.sh"), FAKE_PYTEST).unwrap();
    let s = json(&prova(&d, &["run", "py", "--json"], &[]));
    assert_eq!(s["level"], "result_files", "{s}");
    assert_eq!(s["counts"]["passed"], 2);
    assert_eq!(s["failures"][0]["test"], "tests.test_m::test_c");
    let notes = s["notes"].to_string();
    assert!(
        notes.contains("injected by prova.toml (opt-in): PYTEST_ADDOPTS=--junitxml="),
        "{notes}"
    );
    assert!(
        !d.join("pytest-junit.xml").exists(),
        "report goes to the run's private dir"
    );
}

#[test]
fn recipes_append_to_the_users_own_value() {
    let d = workspace("append");
    let o = prova(
        &d,
        &[
            "run",
            "--json",
            "--inject",
            "go-nocache",
            "--",
            "sh",
            "-c",
            r#"[ "$GOFLAGS" = "-mod=mod -count=1" ]"#,
        ],
        &[("GOFLAGS", "-mod=mod")],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
}

#[test]
fn unknown_recipes_are_refused_before_running() {
    let d = workspace("unknown");
    let o = prova(&d, &["run", "--inject", "path-shims", "--", "true"], &[]);
    assert_eq!(o.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&o.stderr).contains(
        "known: pytest-junit, pytest-reportlog, vitest-stream, gotestsum-json, go-nocache"
    ));
    let status = prova(&d, &["status", "--json"], &[]);
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "[]",
        "no run was created"
    );
}

/// Isolated prova home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}

/// Pretends to be Vitest with prova's reporter: writes a failure to the
/// stream file, then keeps running. Nothing goes to stdout.
const FAKE_STREAMING: &str = r#"
[ -f "$PROVA_VITEST_REPORTER" ] || { echo "reporter not materialized"; exit 3; }
printf '%s\n' '{"type":"test","event":"started","name":"divides","file":"src/calc.test.ts"}' >> "$PROVA_STREAM"
printf '%s\n' '{"type":"test","event":"failed","name":"divides","file":"src/calc.test.ts","exec_time":0.004,"message":"expected 16 to be 4"}' >> "$PROVA_STREAM"
printf '%s' '{"type":"test","event":"started","name":"slow","file":"src/calc.test.ts"}' >> "$PROVA_STREAM"
sleep 30
"#;

/// The stream is read while the run is still going: the first failure
/// arrives through `prova wait` long before the process ends.
#[test]
fn stream_recipe_reports_the_first_failure_live() {
    let d = workspace("stream");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.ts]\nrun = \"sh fake.sh\"\ninject = [\"vitest-stream\"]\n",
    )
    .unwrap();
    std::fs::write(d.join("fake.sh"), FAKE_STREAMING).unwrap();
    let started = std::time::Instant::now();
    let r = json(&prova(&d, &["run", "ts", "--detach", "--json"], &[]));
    let run = r["run"].as_str().unwrap().to_owned();
    let w = json(&prova(
        &d,
        &[
            "wait",
            &run,
            "--until",
            "first-failure",
            "--max-wait",
            "15s",
            "--json",
        ],
        &[],
    ));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "returned before the run ended"
    );
    assert_eq!(w["status"], "running", "{w}");
    assert_eq!(w["level"], "native", "{w}");
    assert_eq!(w["failures"][0]["test"], "src/calc.test.ts::divides", "{w}");
    assert_eq!(w["failures"][0]["message"], "expected 16 to be 4", "{w}");
    let c = prova(&d, &["cancel", &run], &[]);
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
}
