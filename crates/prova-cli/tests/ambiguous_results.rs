//! Medium test for ADR 0037 section 2: results of one name whose place the
//! output does not print (quiet `cargo test` names no crate) are not told
//! apart by a guess. Real runs are needed: the claim is about what the store
//! says across several runs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-ambig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".gitignore"), "mode\n").unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
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

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Some real work first: prova does not believe results printed by a
/// command that used no CPU (round 10).
const BUSY: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done;";

/// What `cargo test -q --no-fail-fast -- --format=pretty` prints for three
/// crates that each have `tests::same`: no binary is named. The `mode` file
/// holds each crate's result (`p` passes, `f` fails).
fn fake_cargo(d: &Path) {
    let one = |i: usize| {
        format!(
            "case \"$(cut -c{n} mode)\" in f) printf '%s\\n' 'running 1 test' 'test tests::same ... FAILED' '' 'failures:' '' '---- tests::same stdout ----' 'thread '\"'\"'tests::same'\"'\"' panicked at src/lib.rs:5:9:' 'boom' '' 'failures:' '    tests::same' '' 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s' ''; code=101;; *) printf '%s\\n' 'running 1 test' 'test tests::same ... ok' '' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s' '';; esac;",
            n = i
        )
    };
    let script = format!(
        "#!/bin/sh\n{BUSY} code=0; {} {} {} exit $code\n",
        one(1),
        one(2),
        one(3)
    );
    let dir = d.join("rc");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cargo");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.q]\nrun = \"./rc/cargo test -q --no-fail-fast -- --format=pretty\"\n",
    )
    .unwrap();
}

fn run(d: &Path, mode: &str) -> (String, String, i32) {
    std::fs::write(d.join("mode"), mode).unwrap();
    let o = prova(d, &["run", "q"]);
    let out = text(&o);
    let id = out
        .split_whitespace()
        .find(|w| w.len() == 8 && w.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or("")
        .to_owned();
    (id, out, o.status.code().unwrap())
}

/// Probe of 0.3.0: a run where one of three same-named, unplaced tests failed
/// recorded the name as passed, and the next run said "failed 3x before".
#[test]
fn unplaced_same_name_results_are_one_failed_result_with_no_history() {
    let d = workspace("quiet");
    fake_cargo(&d);
    let (_, out1, c1) = run(&d, "ppp");
    assert_eq!(c1, 0, "{out1}");
    let (r2, out2, c2) = run(&d, "fpf");
    assert_eq!(c2, 1, "{out2}");
    assert!(out2.contains("FAIL tests::same"), "{out2}");
    assert!(
        out2.contains("cannot tell apart") && out2.contains("without `--quiet`"),
        "{out2}"
    );
    // Three crates ran it: three results, and a place shown once in parentheses.
    assert!(out2.contains("3 results named"), "{out2}");
    assert!(!out2.contains("(("), "{out2}");
    // The first run's pass is no history of a failing result.
    assert!(
        !out2.contains("passed before") && !out2.contains("new failure"),
        "{out2}"
    );
    let (r3, out3, c3) = run(&d, "fpf");
    assert_eq!(c3, 1, "{out3}");
    assert!(out3.contains("FAIL tests::same"), "{out3}");
    for label in ["before", "new failure", "FLAKY", "flaky", "last passed"] {
        assert!(!out3.contains(label), "{label}: {out3}");
    }
    // History of the name shows no pass for the run where one failed.
    let h = text(&prova(&d, &["history", "tests::same"]));
    assert!(!h.contains(&format!("{r2} passed")), "{h}");
    assert!(
        !h.contains("(("),
        "a place that is already parenthesised: {h}"
    );
    // The diff does not say one test is still failing.
    let df = text(&prova(&d, &["diff", &r2, &r3]));
    assert!(!df.contains("still failing"), "{df}");
}

/// `make test` runs pytest in three directories with `-v --no-header`: no
/// `rootdir:` line, the same id `tests/test_a.py::test_same` three times.
fn fake_pytest(d: &Path) {
    let one = |i: usize| {
        format!(
            "case \"$(cut -c{i} mode)\" in f) printf '%s\\n' '============================= test session starts ==============================' 'collecting ... collected 1 item' '' 'tests/test_a.py::test_same FAILED                                        [100%]' '' '=================================== FAILURES ===================================' '________________________________ test_same _____________________________________' '' '    def test_same():' '>       assert 1 == 2' 'E       assert 1 == 2' '' 'tests/test_a.py:2: AssertionError' '=========================== short test summary info ============================' 'FAILED tests/test_a.py::test_same - assert 1 == 2' '============================== 1 failed in 0.01s ==============================='; code=1;; *) printf '%s\\n' '============================= test session starts ==============================' 'collecting ... collected 1 item' '' 'tests/test_a.py::test_same PASSED                                        [100%]' '' '============================== 1 passed in 0.01s ==============================='; ;; esac;"
        )
    };
    let script = format!(
        "#!/bin/sh\n{BUSY} code=0; {} {} {} exit $code\n",
        one(1),
        one(2),
        one(3)
    );
    let dir = d.join("rc");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pytest");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.q]\nrun = \"./rc/pytest -v --no-header\"\n",
    )
    .unwrap();
}

/// Probe of 0.3.0: pytest from three directories, no `rootdir:` line; the
/// start directory stood for all three places, so z's failure was "failed 1x
/// before" from x's.
#[test]
fn pytest_sessions_with_no_rootdir_are_one_failed_result_with_no_history() {
    let d = workspace("pytest");
    fake_pytest(&d);
    let (_, out1, c1) = run(&d, "ppp");
    assert_eq!(c1, 0, "{out1}");
    let (r2, out2, c2) = run(&d, "fpf");
    assert_eq!(c2, 1, "{out2}");
    assert!(out2.contains("FAIL tests.test_a::test_same"), "{out2}");
    assert!(
        out2.contains("cannot tell apart") && out2.contains("pytest with `-v`"),
        "{out2}"
    );
    // Three sessions, three results: the short summary repeats one.
    assert!(out2.contains("3 results named"), "{out2}");
    assert!(!out2.contains("passed before"), "{out2}");
    let (r3, out3, c3) = run(&d, "fpf");
    assert_eq!(c3, 1, "{out3}");
    for label in ["before", "new failure", "FLAKY", "flaky", "last passed"] {
        assert!(!out3.contains(label), "{label}: {out3}");
    }
    let h = text(&prova(&d, &["history", "tests.test_a::test_same"]));
    assert!(!h.contains(&format!("{r2} passed")), "{h}");
    assert!(
        !h.contains("(("),
        "a place that is already parenthesised: {h}"
    );
    let df = text(&prova(&d, &["diff", &r2, &r3]));
    assert!(!df.contains("still failing"), "{df}");
}
