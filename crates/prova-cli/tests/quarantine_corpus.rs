//! Real output of every supported framework, captured in
//! `fixtures/quarantine/` (each `<name>.txt` with the exit code in
//! `<name>.code`; the capture commands are in `fixtures/quarantine/README.md`).
//! One test per capture: prova must learn the flaky test that fails in it;
//! with that test quarantined the run still fails (ADR 0034) and says every
//! failing test is quarantined; another test failing, or another error of
//! the framework, fails it without saying so.
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

/// A script named like the capture's test runner that prints the capture,
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

/// Whether prova's verdict line says PASSED (pytest's own `PASSED` lines are
/// quoted with a prefix).
fn verdict_passed(t: &str) -> bool {
    t.lines().any(|l| l.starts_with("PASSED"))
}

/// One capture: prova learns its flaky test; quarantined, the run still
/// fails and says every failing test is quarantined; `extra` (another real
/// error of that framework, or another test failing, appended) fails it too.
fn corpus(name: &str, result: Option<(&str, &str)>, extra: (&str, &str)) {
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
    // The pytest captures were made in a container (`rootdir: /w`): a place
    // outside the repository is unknown (ADR 0037), so the replay puts the
    // rootdir where the replay runs.
    let argv = runner(
        &d,
        "one",
        name,
        &format!(
            "{BUSY} {copy} sed 's|^rootdir: /w$|rootdir: {}|' {} extra.txt; exit {code}",
            d.display(),
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
    assert_eq!(o.status.code(), Some(1), "{name} must still fail: {t}");
    assert!(!verdict_passed(&t), "{name}: {t}");
    assert!(
        t.contains(&format!("every failing test is quarantined: {}", flaky[0])),
        "{name}: {t}"
    );

    for extra in [error, failure] {
        std::fs::write(d.join("extra.txt"), extra).unwrap();
        let o = run_argv(&d, &[], &argv);
        let t = text(&o);
        assert_eq!(o.status.code(), Some(1), "{name} plus {extra:?}: {t}");
        assert!(!verdict_passed(&t), "{name} plus {extra:?}: {t}");
    }
}

macro_rules! cases {
    ($($fn_name:ident: $file:literal, $result:expr, $extra:expr;)*) => {
        $(
            #[test]
            fn $fn_name() {
                corpus($file, $result, $extra);
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
    corpus_go_test: "go-test", None, GO;
    corpus_go_test_verbose: "go-test-v", None, GO;
    corpus_go_test_json: "go-test-json", None, GO;
    corpus_gotestsum_pkgname: "gotestsum-pkgname", None, GO;
    corpus_gotestsum_testname: "gotestsum-testname", None, GO;
    corpus_gotestsum_standard_verbose: "gotestsum-standard-verbose", None, GO;
    corpus_cargo_test: "cargo-test", None, CARGO;
    corpus_cargo_nextest: "cargo-nextest", None, CARGO;
    corpus_cargo_nextest_libtest_json: "cargo-nextest-libtest-json", None, CARGO;
    corpus_vitest_default: "vitest-default", None, VITEST;
    corpus_vitest_verbose: "vitest-verbose", None, VITEST;
    corpus_pytest_quiet: "pytest-q", None, PYTEST;
    corpus_pytest_verbose: "pytest-v", None, PYTEST;
    corpus_pytest_report_all: "pytest-rA", None, PYTEST;
    corpus_dotnet_test_with_trx: "dotnet-test", TRX, DOTNET;
    corpus_dotnet_test_normal: "dotnet-test-normal", None, DOTNET;
    corpus_mvn_quiet_with_junit: "mvn-q", JUNIT, MVN;
    corpus_mvn_test: "mvn", JUNIT, MVN;
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
