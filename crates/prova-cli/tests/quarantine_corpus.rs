//! Round 16: ADR 0029's guard accounts for every error-looking line of a
//! run's output. It was written against hand-made output and refused real
//! runs (nextest's `error: test run failed`, gotestsum's red package line,
//! VSTest's `Error Message:` block). This runs it against output captured
//! from the real frameworks, in `fixtures/quarantine/` (each `<name>.txt`
//! with the exit code in `<name>.code`; the capture commands are in
//! `fixtures/quarantine/README.md`). One test per capture: the flaky test
//! fails alone and quarantining it must pass the run, unless the output
//! shows no other test passing; one more error line must still fail it.
//! A medium test: it runs the CLI end to end on files and subprocesses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");
const BUSY: &str = "i=0; while [ $i -lt 300000 ]; do i=$((i+1)); done;";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quarantine")
}

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-qc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".gitignore"), "TestResults\ntarget\nextra.txt\n").unwrap();
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

/// A script named like the capture's test runner that prints the capture
/// (a quarantine sets aside only the failures of one runner call, ADR 0031),
/// and the command that runs it.
fn runner(d: &Path, tag: &str, capture: &str, body: &str) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let prog = if capture.starts_with("gotestsum") {
        "gotestsum"
    } else if capture.starts_with("go-") {
        "go"
    } else if capture.starts_with("cargo") {
        "cargo"
    } else if capture.starts_with("vitest") {
        "vitest"
    } else if capture.starts_with("pytest") {
        "pytest"
    } else if capture.starts_with("dotnet") {
        "dotnet"
    } else if capture.starts_with("mvn") {
        "mvn"
    } else {
        "jest"
    };
    let dir = d.join("rc").join(tag);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(prog);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    vec![format!("./rc/{tag}/{prog}"), "test".to_owned()]
}

/// Runs `argv` as `prova run <flags> -- argv...`.
fn run_argv(d: &Path, flags: &[&str], argv: &[String]) -> Output {
    let mut args: Vec<&str> = vec!["run"];
    args.extend(flags);
    args.push("--");
    args.extend(argv.iter().map(String::as_str));
    prova(d, &args)
}

/// Whether the run's verdict line says PASSED (the output it quotes, as in
/// pytest's `PASSED tests/..`, is prefixed).
fn passed(t: &str) -> bool {
    t.lines().any(|l| l.starts_with("PASSED"))
}

/// What a capture's quarantined run must do.
enum Expect {
    /// The output shows another test passing: the run passes.
    Passes,
    /// The output (or its result file) shows no pass: the guard says so.
    NoOtherPass,
}

/// One capture: the flaky test, set aside, passes the run (or is refused
/// for want of a verified pass); `extra` (another real error of that
/// framework, appended) always fails it.
fn corpus(name: &str, result: Option<(&str, &str)>, extra: (&str, &str), expect: Expect) {
    let (error, failure) = extra;
    let dir = fixture_dir();
    let output = dir.join(format!("{name}.txt"));
    let code: i32 = std::fs::read_to_string(dir.join(format!("{name}.code")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let d = workspace(name);
    std::fs::write(d.join("extra.txt"), "").unwrap();
    let copy = result.map_or(String::new(), |(file, dest)| {
        let dest_dir = Path::new(dest).parent().unwrap().display();
        format!(
            "mkdir -p {dest_dir}; cp {} {dest};",
            dir.join(file).display()
        )
    });
    // `extra.txt` is empty until a case appends to it.
    let argv = runner(
        &d,
        "one",
        name,
        &format!(
            "{BUSY} {copy} cat {} extra.txt; exit {code}",
            output.display()
        ),
    );
    let learn = run_argv(&d, &["--json"], &argv);
    let json: serde_json::Value = serde_json::from_slice(&learn.stdout)
        .unwrap_or_else(|e| panic!("{name}: {e}: {}", text(&learn)));
    let failures = json["failures"].as_array().unwrap();
    let flaky: Vec<&str> = failures
        .iter()
        .filter_map(|f| f["test"].as_str())
        .filter(|t| t.to_lowercase().contains("flaky"))
        .collect();
    assert_eq!(
        flaky.len(),
        1,
        "{name}: prova must learn the flaky test: {}",
        text(&learn)
    );
    let o = prova(&d, &["quarantine", "add", flaky[0], "--reason", "clock"]);
    assert_eq!(o.status.code(), Some(0), "{name}: {}", text(&o));

    let o = run_argv(&d, &[], &argv);
    let t = text(&o);
    match expect {
        Expect::Passes => {
            assert_eq!(o.status.code(), Some(0), "{name} must pass: {t}");
            assert!(passed(&t), "{name}: {t}");
            assert!(!t.contains("outside any failing test"), "{name}: {t}");
        }
        Expect::NoOtherPass => {
            assert_eq!(o.status.code(), Some(1), "{name}: {t}");
            assert!(
                (t.contains("no other test passed") || t.contains("names no passing test")),
                "{name}: {t}"
            );
        }
    }

    // Another real error of the framework's, in a line no test owns: the
    // guard says so. Another test's failure: the run fails (its own way).
    for (extra, is_guard) in [(error, true), (failure, false)] {
        std::fs::write(d.join("extra.txt"), extra).unwrap();
        let o = run_argv(&d, &[], &argv);
        let t = text(&o);
        assert_eq!(o.status.code(), Some(1), "{name} plus {extra:?}: {t}");
        assert!(!passed(&t), "{name} plus {extra:?}: {t}");
        assert!(
            !is_guard
                || t.contains("outside the quarantined test")
                || t.contains("printed more after the tests' summary")
                || matches!(expect, Expect::NoOtherPass),
            "{name} plus {extra:?} must say why: {t}"
        );
    }
}

macro_rules! cases {
    ($($fn_name:ident: $file:literal, $result:expr, $extra:expr, $expect:expr;)*) => {
        $(
            #[test]
            fn $fn_name() {
                corpus($file, $result, $extra, $expect);
            }
        )*
    };
}

// Another error of the framework's own, no test's, and another test failing.
const GO: (&str, &str) = (
    "# example.com/other\n./other.go:3:1: syntax error: unexpected }\n",
    "--- FAIL: TestDiv (0.00s)\n    calc_test.go:17: Div(8, 2) = 16, want 4\n",
);
const CARGO: (&str, &str) = (
    "error[E0432]: unresolved import `other`\n --> src/lib.rs:1:5\n",
    "---- tests::divides stdout ----\nthread 'tests::divides' panicked at src/lib.rs:23:9:\n",
);
const VITEST: (&str, &str) = (
    "Error: Cannot find module './other' imported from src/calc.test.ts\n",
    " FAIL  src/calc.test.ts > calc > divides\nAssertionError: expected 16 to be 4 // Object.is equality\n",
);
const PYTEST: (&str, &str) = (
    "ERROR: file or directory not found: tests/other\n",
    "E   assert 16 == 4\nFAILED tests/test_calc.py::test_div - assert 16 == 4\n",
);
const DOTNET: (&str, &str) = (
    "/w/Other.cs(3,5): error CS1002: ; expected [/w/Calc.Tests.csproj]\n",
    "  Failed Calc.CalcTests.Divides [1 ms]\n  Error Message:\n   Assert.Equal() Failure: Values differ\n",
);
const MVN: (&str, &str) = (
    "[ERROR] COMPILATION ERROR :\n[ERROR] /w/src/main/java/Other.java:[3,5] ';' expected\n",
    "[ERROR] calc.CalcTest.divides -- Time elapsed: 0.001 s <<< FAILURE!\norg.opentest4j.AssertionFailedError: expected: <4> but was: <16>\n",
);
const TRX: Option<(&str, &str)> = Some(("dotnet-test-normal.trx", "TestResults/r.trx"));
const JUNIT: Option<(&str, &str)> = Some((
    "mvn.junit.xml",
    "target/surefire-reports/TEST-calc.CalcTest.xml",
));

cases! {
    corpus_go_test: "go-test", None, GO, Expect::NoOtherPass;
    corpus_go_test_verbose: "go-test-v", None, GO, Expect::Passes;
    corpus_go_test_json: "go-test-json", None, GO, Expect::Passes;
    corpus_gotestsum_pkgname: "gotestsum-pkgname", None, GO, Expect::NoOtherPass;
    corpus_gotestsum_testname: "gotestsum-testname", None, GO, Expect::Passes;
    corpus_gotestsum_standard_verbose: "gotestsum-standard-verbose", None, GO, Expect::Passes;
    corpus_cargo_test: "cargo-test", None, CARGO, Expect::Passes;
    corpus_cargo_nextest: "cargo-nextest", None, CARGO, Expect::Passes;
    corpus_cargo_nextest_libtest_json: "cargo-nextest-libtest-json", None, CARGO, Expect::Passes;
    corpus_vitest_default: "vitest-default", None, VITEST, Expect::Passes;
    corpus_vitest_verbose: "vitest-verbose", None, VITEST, Expect::Passes;
    corpus_pytest_quiet: "pytest-q", None, PYTEST, Expect::NoOtherPass;
    corpus_pytest_verbose: "pytest-v", None, PYTEST, Expect::Passes;
    corpus_pytest_report_all: "pytest-rA", None, PYTEST, Expect::Passes;
    corpus_dotnet_test_with_trx: "dotnet-test", TRX, DOTNET, Expect::Passes;
    corpus_dotnet_test_normal: "dotnet-test-normal", None, DOTNET, Expect::Passes;
    corpus_mvn_quiet_with_junit: "mvn-q", JUNIT, MVN, Expect::Passes;
    corpus_mvn_test: "mvn", JUNIT, MVN, Expect::Passes;
}

/// A capture with its output edited: the flaky test is learned from the
/// unedited output and quarantined; the edited run must fail, with `why`.
fn probe(name: &str, edit: &dyn Fn(&str) -> String, why: &str) {
    let dir = fixture_dir();
    let raw = std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap();
    let code: i32 = std::fs::read_to_string(dir.join(format!("{name}.code")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Unique per call: `cargo test` runs every probe in one process, at
    // once, and two probes of one capture must not share a store.
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = workspace(&format!("probe-{name}-{n}"));
    std::fs::write(d.join("base.txt"), &raw).unwrap();
    std::fs::write(d.join("edited.txt"), edit(&raw)).unwrap();
    let script = |f: &str| runner(&d, f, name, &format!("{BUSY} cat {f}; exit {code}"));
    let learn = run_argv(&d, &["--json"], &script("base.txt"));
    let json: serde_json::Value = serde_json::from_slice(&learn.stdout).unwrap();
    let flaky = json["failures"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["test"].as_str())
        .find(|t| t.to_lowercase().contains("flaky"))
        .unwrap_or_else(|| panic!("{name}: {}", text(&learn)));
    prova(&d, &["quarantine", "add", flaky, "--reason", "clock"]);
    let o = run_argv(&d, &[], &script("base.txt"));
    assert_eq!(o.status.code(), Some(0), "{name} base: {}", text(&o));
    let o = run_argv(&d, &[], &script("edited.txt"));
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{name} edited ({why}): {t}");
    assert!(!passed(&t), "{name} edited ({why}): {t}");
    assert!(t.contains(why), "{name} edited must say {why:?}: {t}");
}

const AFTER: &str = "the command printed more after the tests' summary";
const OUTSIDE: &str = "outside the quarantined test";

fn append(extra: &'static str) -> impl Fn(&str) -> String {
    move |raw| format!("{raw}{extra}")
}

fn insert_before(marker: &'static str, extra: &'static str) -> impl Fn(&str) -> String {
    move |raw| raw.replacen(marker, &format!("{extra}{marker}"), 1)
}

/// Round 16 probes: a step that ran after the tests, with no error word in
/// its output, passed behind a quarantined failure.
#[test]
fn probe_steps_after_the_tests_summary_are_refused() {
    for (name, extra) in [
        (
            "pytest-v",
            "app/core.py:3:8: F401 [*] `os` imported but unused\nFound 1 error.\n",
        ),
        (
            "pytest-v",
            "./app/core.py:3:1: F401 'os' imported but unused\n",
        ),
        (
            "go-test-v",
            "calc/calc.go:9:2: fmt.Printf format %d has arg s of wrong type string\n",
        ),
        (
            "pytest-v",
            "In scripts/deploy.sh line 4:\nrm -rf $DIR/\n^-- SC2115 (warning): Use \"${var:?}\"\n",
        ),
        ("pytest-v", "smoke: GET /health returned 503\n"),
        (
            "pytest-v",
            "Coverage failure: total of 85 is less than fail-under=90\n",
        ),
        (
            "pytest-v",
            "Jest: \"global\" coverage threshold for lines (90%) not met: 85%\n",
        ),
    ] {
        probe(name, &append(extra), AFTER);
    }
}

#[test]
fn probe_a_coverage_gate_inside_the_failures_section_is_refused() {
    probe(
        "pytest-v",
        &insert_before(
            "=========================== short test summary info",
            "---------- coverage: platform linux, python 3.12.1 ----------\nTOTAL 10 5 50%\nFAIL Required test coverage of 90% not reached. Total coverage: 50.00%\n",
        ),
        OUTSIDE,
    );
}

#[test]
fn probe_a_crash_naming_the_test_from_another_process_is_refused() {
    for extra in [
        "./scripts/check.sh: line 4: 31337 Segmentation fault (core dumped) ./bin/test_flaky_clock --integration\n",
        "Error: test_flaky_clock fixture service exited with code 137 (OOMKilled)\n",
    ] {
        probe(
            "pytest-v",
            &insert_before("=================================== FAILURES", extra),
            OUTSIDE,
        );
    }
}

#[test]
fn probe_sub_make_failures_are_error_lines() {
    for extra in [
        "make[1]: *** [Makefile:6: lint] Error 1\n",
        "make[1]: Target 'all' not remade because of errors.\n",
    ] {
        probe(
            "pytest-v",
            &insert_before("=================================== FAILURES", extra),
            OUTSIDE,
        );
    }
}

#[test]
fn probe_two_failed_cargo_targets_are_refused() {
    probe(
        "cargo-test",
        &|raw| {
            raw.replace(
                "error: 1 target failed:\n    `--lib`\n",
                "error: 2 targets failed:\n    `-p calc --lib`\n    `-p calc --test smoke`\n",
            )
        },
        "error: 2 targets failed:",
    );
}

#[test]
fn probe_a_second_failed_file_is_refused() {
    probe(
        "jest-suites",
        &|raw| raw.replace("Test Suites: 1 failed", "Test Suites: 2 failed"),
        "failed file",
    );
}

/// Round 16: nextest's retry that passed: the panic of the first try is
/// the retried test's own, not an error outside any test.
#[test]
fn a_nextest_retry_that_passed_is_not_an_error_outside_a_test() {
    let dir = fixture_dir();
    let d = workspace("nextest-retry");
    let script = format!(
        "{BUSY} cat {}; exit 0",
        dir.join("cargo-nextest-retry.txt").display()
    );
    let o = prova(&d, &["run", "--", "sh", "-c", &script]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    assert!(!t.contains("outside any failing test"), "{t}");
}
