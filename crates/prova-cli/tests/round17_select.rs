//! Medium tests for round 17 selection findings (ADR 0031 section 3), each
//! a real prova process over a scripted command: a smaller test cannot
//! cover the shell, the wrapper and the verdict together.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");
const WORK_SH: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; ";

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-r17sel-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env(
            "PROVA_HOME",
            PathBuf::from(format!("{}.home", dir.display())),
        )
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

fn suite(d: &Path, name: &str, select: &str) {
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.{name}]\nrun = \"true\"\nselect = {select:?}\n"),
    )
    .unwrap();
}

/// Round 17 (python-make, java-maven): `{filter}` inside a wrapper's quoted
/// argument was split by the outer shell and expanded again by the inner.
#[test]
fn a_filter_in_a_wrapper_argument_reaches_the_inner_shell_as_one_word() {
    use std::os::unix::fs::PermissionsExt;
    let d = workspace("wrapper");
    let dev = d.join("dev");
    // Like `./dev`: the whole command is one argument, run by a new shell.
    std::fs::write(&dev, "#!/bin/sh\nexec sh -c \"$*\"\n").unwrap();
    std::fs::set_permissions(&dev, std::fs::Permissions::from_mode(0o755)).unwrap();
    suite(
        &d,
        "w",
        &format!("{WORK_SH}./dev \"printf 'test m::%s ... FAILED\\n' {{filter}}; exit 101\""),
    );
    let o = prova(&d, &["run", "w", "--select", "Calc$Div ision x"]);
    let out = text(&o);
    // One failing test with the whole text, not three, not expanded.
    assert!(out.contains("m::Calc$Div ision x"), "{out}");
}

/// Round 17 (pytest): a node id `tests/test_calc.py::test_ok` ran and
/// passed, and prova called the test missing.
#[test]
fn a_pytest_node_id_selection_counts_as_run() {
    let d = workspace("nodeid");
    suite(
        &d,
        "py",
        &format!(
            "{WORK_SH}echo 'collected 2 items / 1 deselected / 1 selected'; echo 'tests/test_calc.py::test_ok PASSED'; echo '1 passed, 1 deselected in 0.01s'; echo {{filter}} >/dev/null"
        ),
    );
    let o = prova(
        &d,
        &["run", "py", "--select", "tests/test_calc.py::test_ok"],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(!out.contains("did not run"), "{out}");
}

/// Round 17 (Go): `TestTable/x` with no such subtest ran only the parent.
#[test]
fn a_missing_go_subtest_is_not_a_pass_because_its_parent_ran() {
    let d = workspace("gosub");
    suite(
        &d,
        "go",
        &format!(
            "{WORK_SH}printf '=== RUN   TestTable\\n--- PASS: TestTable (0.00s)\\nPASS\\n'; echo {{filter}} >/dev/null"
        ),
    );
    let o = prova(&d, &["run", "go", "--select", "TestTable/x"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("that test did not run"), "{out}");
}

/// Round 17: `adopt --verify` with a failing selected test said VERIFIED
/// and nothing of the failure.
#[test]
fn verify_says_the_selected_test_failed() {
    let d = workspace("verify-failed");
    suite(
        &d,
        "t",
        &format!(
            "{WORK_SH}printf 'test calc::%s ... FAILED\\n' {{filter}}; echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored'; exit 101"
        ),
    );
    let o = prova(&d, &["adopt", "--verify", "t", "--filter", "divides"]);
    let out = text(&o);
    assert!(out.starts_with("VERIFIED"), "{out}");
    assert!(
        out.contains("failed") && out.contains("calc::divides"),
        "{out}"
    );
}

/// Round 17 (Java): one parameterized case cannot be selected in Surefire.
#[test]
fn verify_explains_that_surefire_selects_by_method() {
    let d = workspace("verify-param");
    suite(
        &d,
        "t",
        &format!(
            "{WORK_SH}echo 'test calc.CalcTest::addsMany(int, int, int)[1] ... ok'; echo 'test calc.CalcTest::addsMany(int, int, int)[2] ... ok'; echo {{filter}} >/dev/null"
        ),
    );
    let o = prova(
        &d,
        &[
            "adopt",
            "--verify",
            "t",
            "--filter",
            "calc.CalcTest::addsMany(int, int, int)[2]",
        ],
    );
    let out = text(&o);
    assert!(out.contains("selects by method"), "{out}");
    assert!(!out.contains("select is not selective"), "{out}");
}

/// Round 17: a selection that ran 0 tests blamed the level.
#[test]
fn verify_names_a_filter_that_reached_nothing() {
    let d = workspace("verify-zero");
    suite(
        &d,
        "t",
        &format!(
            "{WORK_SH}echo 'Tests run: 0, Failures: 0, Errors: 0, Skipped: 0'; echo {{filter}} >/dev/null"
        ),
    );
    let o = prova(
        &d,
        &["adopt", "--verify", "t", "--filter", "CalcTest$Division"],
    );
    let out = text(&o);
    assert!(out.contains("did not reach the framework"), "{out}");
    assert!(!out.contains("raise the level"), "{out}");
}
