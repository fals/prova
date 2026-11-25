//! Medium tests for ADR 0031 section 2 (usability round 17): a test keeps its
//! place. Real runs are needed: names are read from a command's output and
//! compared with the store's history and quarantines across several runs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-r17id-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".gitignore"), "state\n").unwrap();
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

/// nextest's human output for two crates that both have `tests::flaky`.
fn nextest(calc: &str, geo: &str) -> String {
    format!(
        "{BUSY} printf '%s\\n' '    Starting 3 tests across 2 binaries' '        PASS [   0.004s] calc tests::adds' '        {calc} [   0.004s] calc tests::flaky' '        {geo} [   0.004s] geo tests::flaky' '     Summary [   0.020s] 3 tests run: {} passed, {} failed'; exit {}",
        3 - [calc, geo].iter().filter(|o| **o == "FAIL").count(),
        [calc, geo].iter().filter(|o| **o == "FAIL").count(),
        if calc == "FAIL" || geo == "FAIL" {
            100
        } else {
            0
        }
    )
}

/// Each suite runs a fake `cargo` printing nextest's output: one
/// test-runner call, which a quarantine needs (ADR 0031).
fn two_crates(d: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut toml = String::new();
    for (suite, calc, geo) in [
        ("a", "FAIL", "PASS"),
        ("b", "PASS", "FAIL"),
        ("c", "PASS", "PASS"),
    ] {
        let dir = d.join("rc").join(suite);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cargo");
        std::fs::write(&path, format!("#!/bin/sh\n{}\n", nextest(calc, geo))).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        toml.push_str(&format!(
            "[suite.{suite}]\nrun = \"./rc/{suite}/cargo nextest run\"\n\n"
        ));
    }
    std::fs::write(d.join("prova.toml"), toml).unwrap();
}

/// Round 17 (probe M1): calc's `tests::flaky` is quarantined; geo's own
/// failure was shown as the quarantined test, with no FAIL line and a false
/// "only quarantined test(s) failed".
#[test]
fn a_failure_of_the_other_crates_same_named_test_is_reported_with_its_place() {
    let d = workspace("geo");
    two_crates(&d);
    prova(&d, &["run", "a"]);
    prova(&d, &["run", "c"]);
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests::flaky",
            "--in",
            "calc",
            "--reason",
            "clock",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = prova(&d, &["run", "b"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("FAIL tests::flaky (geo)"), "{out}");
    assert!(!out.contains("only quarantined"), "{out}");
    let o = prova(&d, &["run", "b", "--json"]);
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let f = &j["failures"][0];
    assert_eq!(f["test"], "tests::flaky", "{j}");
    assert_eq!(f["place"], "geo", "{j}");
    // And calc's own, quarantined, is set aside while geo passes.
    let o = prova(&d, &["run", "a"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
}

/// Round 17 (probe M6): nextest's `alpha::it integ` and `beta::it integ`
/// became `integ` and `integ (second result)` by execution order, and two
/// passing ones were counted once.
#[test]
fn integration_tests_of_two_crates_keep_their_crate_not_their_order() {
    let d = workspace("integ");
    let nx = |first: (&str, &str), second: (&str, &str), code: u8| {
        format!(
            "{BUSY} printf '%s\\n' '    Starting 2 tests across 2 binaries' '        {} [   0.004s] {} integ' '        {} [   0.004s] {} integ' '     Summary [   0.020s] 2 tests run: {} passed, {} failed'; exit {code}",
            first.0,
            first.1,
            second.0,
            second.1,
            [first.0, second.0].iter().filter(|o| **o == "PASS").count(),
            [first.0, second.0].iter().filter(|o| **o == "FAIL").count()
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.a]\nrun = {:?}\n\n[suite.b]\nrun = {:?}\n\n[suite.c]\nrun = {:?}\n",
            nx(("PASS", "alpha::it"), ("FAIL", "beta::it"), 100),
            nx(("FAIL", "beta::it"), ("PASS", "alpha::it"), 100),
            nx(("PASS", "alpha::it"), ("PASS", "beta::it"), 0),
        ),
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "a"]));
    assert!(out.contains("tests: 1 passed, 1 failed"), "{out}");
    assert!(out.contains("FAIL integ (beta::it)"), "{out}");
    let out = text(&prova(&d, &["run", "b"]));
    assert!(out.contains("FAIL integ (beta::it)"), "{out}");
    assert!(out.contains("failed 1x before"), "{out}");
    let out = text(&prova(&d, &["run", "c"]));
    assert!(
        out.contains("tests: 2 passed"),
        "two passing, one name: {out}"
    );
    let all = text(&prova(&d, &["history", ""]));
    assert!(!all.contains("second result"), "{all}");
    assert!(
        all.contains("integ (alpha::it)") && all.contains("integ (beta::it)"),
        "{all}"
    );
}

/// Round 17 (probe B4): pytest names tests from its rootdir, so
/// `sub2/tests/test_calc.py::test_flaky` and the root's
/// `tests/test_calc.py::test_flaky` were both `tests.test_calc::test_flaky`.
/// Ids are read from prova's working directory.
#[test]
fn pytest_ids_are_relative_to_the_working_directory_not_the_rootdir() {
    let d = workspace("pyroot");
    std::fs::create_dir_all(d.join("sub2")).unwrap();
    // The rootdir printed through a symlink of the working directory (macOS
    // temp dirs: `/var` is `/private/var`) is still below it.
    let link = PathBuf::from(format!("{}.link", d.display()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&d, &link).unwrap();
    let sub = link.join("sub2");
    let py = |rootdir: &Path, id: &str, outcome: &str, code: u8| {
        format!(
            "{BUSY} printf '%s\\n' '============ test session starts ============' 'rootdir: {}' 'collecting ... collected 1 item' '{id} {outcome} [100%]' '' '============ 1 {} in 0.01s ============'; exit {code}",
            rootdir.display(),
            if outcome == "PASSED" {
                "passed"
            } else {
                "failed"
            },
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.root]\nrun = {:?}\n\n[suite.sub]\nrun = {:?}\n",
            py(&d, "tests/test_calc.py::test_flaky", "PASSED", 0),
            py(&sub, "tests/test_calc.py::test_flaky", "FAILED", 1),
        ),
    )
    .unwrap();
    prova(&d, &["run", "root"]);
    let out = text(&prova(&d, &["run", "sub"]));
    assert!(
        out.contains("FAIL sub2.tests.test_calc::test_flaky"),
        "named from the working directory: {out}"
    );
    let all = text(&prova(&d, &["history", ""]));
    assert!(
        all.contains("sub2.tests.test_calc::test_flaky")
            && all.contains("  tests.test_calc::test_flaky")
            && !all.contains("second result"),
        "{all}"
    );
}

/// Round 17 follow-up: pytest prints ids relative to the directory it runs
/// in (`cwd_relative_nodeid`), not to its rootdir, and `-q` prints no
/// rootdir at all. prova checks which directory holds the test's file:
/// the working directory, a `cd` in the command, the rootdir.
#[test]
fn pytest_ids_are_named_from_the_directory_pytest_ran_in() {
    let d = workspace("pydir");
    for dir in ["tests", "sub2/tests"] {
        std::fs::create_dir_all(d.join(dir)).unwrap();
        std::fs::write(d.join(dir).join("test_calc.py"), "").unwrap();
    }
    std::fs::write(d.join("sub2/pytest.ini"), "[pytest]\n").unwrap();
    let verbose = |id: &str| {
        format!(
            "{BUSY} printf '%s\\n' 'rootdir: {}' 'collecting ... collected 2 items' '{id} FAILED [ 50%]' '' 'FAILED {id} - assert 1 == 2' '=== 1 failed, 1 passed in 0.01s ==='; exit 1",
            d.join("sub2").display()
        )
    };
    let quiet = |id: &str| {
        format!(
            "{BUSY} printf '%s\\n' 'F.   [100%]' '' 'FAILED {id} - assert 1 == 2' '1 failed, 1 passed in 0.01s'; exit 1"
        )
    };
    let suites = [
        // `pytest -v sub2/tests` from the root: rootdir sub2, ids from the root.
        ("verbose", verbose("sub2/tests/test_calc.py::test_flaky")),
        // `cd sub2 && pytest -q tests`: no rootdir printed.
        (
            "cdquiet",
            format!("cd sub2 && {}", quiet("tests/test_calc.py::test_flaky")),
        ),
        // `pytest -q` at the root.
        ("rootquiet", quiet("tests/test_calc.py::test_flaky")),
    ];
    let toml: String = suites
        .iter()
        .map(|(n, c)| format!("[suite.{n}]\nrun = {c:?}\n\n"))
        .collect();
    std::fs::write(d.join("prova.toml"), toml).unwrap();
    let out = text(&prova(&d, &["run", "verbose"]));
    assert!(
        out.contains("FAIL sub2.tests.test_calc::test_flaky"),
        "{out}"
    );
    assert!(!out.contains("sub2.sub2"), "{out}");
    let out = text(&prova(&d, &["run", "cdquiet"]));
    assert!(
        out.contains("FAIL sub2.tests.test_calc::test_flaky"),
        "{out}"
    );
    let out = text(&prova(&d, &["run", "rootquiet"]));
    assert!(out.contains("FAIL tests.test_calc::test_flaky"), "{out}");
    assert!(!out.contains("sub2"), "{out}");
}

/// When the directory pytest ran in cannot be told (two `cd` targets hold
/// the file), the test's place is unknown: it is said, and no quarantine of
/// another directory's test of that id sets its failure aside.
#[test]
fn a_pytest_test_of_unknown_directory_is_not_set_aside_by_a_quarantine() {
    let d = workspace("pyunknown");
    for dir in ["tests", "sub2/tests", "sub3/tests"] {
        std::fs::create_dir_all(d.join(dir)).unwrap();
        std::fs::write(d.join(dir).join("test_calc.py"), "").unwrap();
    }
    let out = |outcome: &str, code: u8| {
        format!(
            "{BUSY} printf '%s\\n' 'tests/test_calc.py::test_flaky {outcome} [100%]' '=== 1 {} in 0.01s ==='; exit {code}",
            outcome.to_lowercase()
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.root]\nrun = {:?}\n\n[suite.two]\nrun = {:?}\n",
            out("PASSED", 0),
            format!("(cd sub2; true); cd sub3; {}", out("FAILED", 1)),
        ),
    )
    .unwrap();
    prova(&d, &["run", "root"]);
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests.test_calc::test_flaky",
            "--reason",
            "r",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = prova(&d, &["run", "two"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("cannot tell which test is which"), "{out}");
    assert!(out.contains("FAIL tests.test_calc::test_flaky"), "{out}");
    assert!(!out.contains("test_flaky failed (r"), "{out}");
    // The quarantine line says what happened to it, not "not seen".
    assert!(
        out.contains(
            "tests.test_calc::test_flaky failed in an unknown directory: not set aside (r"
        ),
        "{out}"
    );
}

/// Vitest names files from its root, which its `RUN` header prints: from a
/// package directory (`cd pkg && vitest run`, `--root pkg`) two packages'
/// `src/a.test.ts` were one file.
#[test]
fn vitest_files_are_named_from_the_working_directory_not_the_root() {
    let d = workspace("vtroot");
    std::fs::create_dir_all(d.join("pkg")).unwrap();
    // Through a symlink of the working directory, as macOS temp dirs are.
    let link = PathBuf::from(format!("{}.link", d.display()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&d, &link).unwrap();
    let run = |root: &Path, ok: &str| {
        format!(
            "{BUSY} printf '%s\\n' '' ' RUN  v3.2.4 {}' '' ' {ok} src/a.test.ts > works 1ms' ' ✓ src/a.test.ts > other 1ms' '' ' Test Files  1 failed (1)' '      Tests  1 failed | 1 passed (2)'; exit {}",
            root.display(),
            u8::from(ok != "✓")
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.root]\nrun = {:?}\n\n[suite.pkg]\nrun = {:?}\n",
            run(&d, "✓"),
            run(&link.join("pkg"), "×"),
        ),
    )
    .unwrap();
    prova(&d, &["run", "root"]);
    let out = text(&prova(&d, &["run", "pkg"]));
    assert!(
        out.contains("FAIL pkg/src/a.test.ts::works"),
        "named from the working directory: {out}"
    );
    let all = text(&prova(&d, &["history", ""]));
    assert!(
        all.contains("pkg/src/a.test.ts::works") && all.contains("  src/a.test.ts::works"),
        "{all}"
    );
}

/// Round 17 (java-maven): the live line and the XML record of one failing
/// test were two tests in `adopt --verify`: "1 tests ran that do not match"
/// for the full name, "2 matching test(s) ran" for `Class#method`.
#[test]
fn verify_counts_one_failing_test_once_whatever_the_filter_form() {
    let d = workspace("verify");
    std::fs::write(
        d.join("t.sh"),
        format!(
            "{BUSY}\nprintf '%s\\n' '[INFO] Running calc.CalcTest' '[ERROR] Tests run: 1, Failures: 1, Errors: 0, Skipped: 0, Time elapsed: 0.04 s <<< FAILURE! -- in calc.CalcTest' '[INFO] Results:' '[ERROR] Failures: ' '[ERROR]   CalcTest.divides:12 expected 4' '[ERROR] Tests run: 1, Failures: 1, Errors: 0, Skipped: 0'\ncat > report.xml <<'XML'\n<testsuite name=\"calc.CalcTest\" tests=\"1\" failures=\"1\"><testcase classname=\"calc.CalcTest\" name=\"divides\" time=\"0.01\"><failure message=\"expected 4\">trace</failure></testcase></testsuite>\nXML\nexit 1\n"
        ),
    )
    .unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.default]\nrun = \"sh t.sh\"\nselect = \"sh t.sh {filter}\"\nresults = [\"report.xml\"]\n",
    )
    .unwrap();
    for filter in ["calc.CalcTest::divides", "CalcTest#divides"] {
        let o = prova(
            &d,
            &["adopt", "--verify", "default", "--filter", filter, "--json"],
        );
        let j: serde_json::Value =
            serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
        assert_eq!(j["verified"], true, "{filter}: {j}");
        assert_eq!(j["tests"].as_array().unwrap().len(), 1, "{filter}: {j}");
    }
}

/// Round 17 (probe M2): `quarantine add --in` worked only for Rust
/// binaries; Go, pytest and Vitest said "be more specific" without a word
/// of `--in`. Each framework's own form of the place picks the test.
#[test]
#[allow(clippy::type_complexity)]
fn quarantine_in_takes_the_place_in_each_frameworks_form() {
    // (name of the shared test, command output, [(--in form, quarantined name)])
    let cases: [(&str, &str, &[(&str, &str)]); 3] = [
        (
            "TestFlaky",
            "printf '%s\\n' '=== RUN   TestFlaky' '--- PASS: TestFlaky (0.00s)' 'PASS' 'ok  \t ex/a\t0.01s' '=== RUN   TestFlaky' '--- FAIL: TestFlaky (0.00s)' 'FAIL' 'FAIL\tex/b\t0.01s'; exit 1",
            &[
                ("a", "ex/a::TestFlaky"),
                ("ex/a", "ex/a::TestFlaky"),
                ("./a", "ex/a::TestFlaky"),
                ("ex/b", "ex/b::TestFlaky"),
            ],
        ),
        (
            "test_flaky",
            "printf '%s\\n' 'rootdir: /x' 'tests/test_calc.py::test_flaky PASSED [ 50%]' 'sub/tests/test_calc.py::test_flaky FAILED [100%]' '' '=== 1 failed, 1 passed in 0.01s ==='; exit 1",
            &[
                ("tests/test_calc.py", "tests.test_calc::test_flaky"),
                ("tests.test_calc", "tests.test_calc::test_flaky"),
                ("sub/tests/test_calc.py", "sub.tests.test_calc::test_flaky"),
            ],
        ),
        (
            "works",
            "printf '%s\\n' ' ✓ src/a/x.test.js > works 1ms' ' ✓ src/b/x.test.js > works 1ms' '' ' Test Files  2 passed (2)' '      Tests  2 passed (2)'",
            &[
                ("src/a/x.test.js", "src/a/x.test.js::works"),
                ("src/b/x.test.js", "src/b/x.test.js::works"),
            ],
        ),
    ];
    for (i, (name, script, forms)) in cases.into_iter().enumerate() {
        let d = workspace(&format!("in{i}"));
        let o = prova(&d, &["run", "--", "sh", "-c", &format!("{BUSY} {script}")]);
        let out = text(&o);
        let o = prova(&d, &["quarantine", "add", name, "--reason", "r"]);
        let t = text(&o);
        assert_eq!(o.status.code(), Some(2), "{name}: {t}\n{out}");
        assert!(t.contains("--in") && t.contains("2 tests match"), "{t}");
        for (form, quarantined) in forms {
            let o = prova(
                &d,
                &["quarantine", "add", name, "--in", form, "--reason", "r"],
            );
            let t = text(&o);
            assert_eq!(o.status.code(), Some(0), "--in {form}: {t}");
            assert!(
                t.contains(&format!("quarantined {quarantined} ")),
                "--in {form}: {t}"
            );
        }
        let o = prova(
            &d,
            &[
                "quarantine",
                "add",
                name,
                "--in",
                "nowhere",
                "--reason",
                "r",
            ],
        );
        assert!(text(&o).contains("is not known in"), "{}", text(&o));
    }
}

/// The id of the run a `prova run` printed.
fn run_id(o: &Output) -> String {
    text(o)
        .lines()
        .find_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            (w.first() == Some(&"run") && w.get(2) == Some(&"suite")).then(|| w[1].to_owned())
        })
        .unwrap_or_else(|| panic!("no run id in {}", text(o)))
}

/// Round 17: two crates' `tests::flaky` were one test in `diff`, `flaky`,
/// `slowest`, `sizes` and `history`: calc's flakiness made geo's constant
/// failure "known flaky", and geo's failures were invisible.
#[test]
fn two_crates_same_name_stay_apart_in_every_list() {
    let d = workspace("lists");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.x]\nrun = {:?}\n\n[suite.y]\nrun = {:?}\n",
            nextest("FAIL", "FAIL"),
            nextest("PASS", "FAIL"),
        ),
    )
    .unwrap();
    let r1 = run_id(&prova(&d, &["run", "x"]));
    let r2 = run_id(&prova(&d, &["run", "y"]));
    let r3 = run_id(&prova(&d, &["run", "x"]));
    let diff = text(&prova(&d, &["diff", &r2, &r3]));
    assert!(
        diff.contains("changed, but known flaky (1):\n  tests::flaky (calc)"),
        "{diff}"
    );
    assert!(
        diff.contains("still failing (1):\n  tests::flaky (geo)"),
        "{diff}"
    );
    assert!(!diff.contains("newly failing"), "{diff}");
    // The first two: calc's pass is not geo's.
    let diff = text(&prova(&d, &["diff", &r1, &r2]));
    assert!(diff.contains("tests::flaky (calc)"), "{diff}");

    let flaky = text(&prova(&d, &["flaky"]));
    assert!(flaky.contains("tests::flaky (calc)"), "{flaky}");
    assert!(!flaky.contains("tests::flaky (geo)"), "{flaky}");

    let slow = text(&prova(&d, &["slowest"]));
    assert!(
        slow.contains("3 runs  tests::flaky (calc)") && slow.contains("3 runs  tests::flaky (geo)"),
        "{slow}"
    );
    let sizes = text(&prova(&d, &["sizes"]));
    assert!(sizes.contains("3 tests by what they used"), "{sizes}");

    let h = text(&prova(&d, &["history", "tests::flaky"]));
    assert!(
        h.contains("tests::flaky (calc)") && h.contains("tests::flaky (geo)"),
        "{h}"
    );
    let h = text(&prova(&d, &["history", "tests::flaky", "--in", "geo"]));
    assert!(
        h.contains("tests::flaky (geo)") && !h.contains("(calc)") && h.contains("3 failed (100%)"),
        "{h}"
    );
    let all = text(&prova(&d, &["history", ""]));
    assert!(
        all.contains("tests::flaky (calc)") && all.contains("tests::flaky (geo)"),
        "{all}"
    );

    prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests::flaky",
            "--in",
            "calc",
            "--reason",
            "clock",
        ],
    );
    let list = text(&prova(&d, &["quarantine", "list"]));
    assert!(list.contains("tests::flaky (calc)"), "{list}");
    // The run summary shows it, too.
    let out = text(&prova(&d, &["run", "y"]));
    assert!(out.contains("quarantined: tests::flaky (calc)"), "{out}");
}

/// Round 17 (rs-make): plain `cargo test` prints `Running tests/it.rs
/// (target/debug/deps/it-<hash>)` with no crate, so two crates' integration
/// tests of one file name were one test. cargo's dep-info file of that very
/// binary names its source, and the source's Cargo.toml the crate; nextest
/// names it `calc::it`, so does prova. Medium: it reads files cargo wrote.
#[test]
fn integration_tests_of_two_crates_run_by_cargo_test_keep_their_crate() {
    let d = workspace("cargoit");
    for (krate, hash) in [("calc", "aaaa1111"), ("geo", "bbbb2222")] {
        std::fs::create_dir_all(d.join(krate).join("tests")).unwrap();
        std::fs::write(
            d.join(krate).join("Cargo.toml"),
            format!("[package]\nname = \"{krate}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        std::fs::write(d.join(krate).join("tests/it.rs"), "").unwrap();
        let deps = d.join("target/debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(
            deps.join(format!("it-{hash}.d")),
            format!(
                "{p}/it-{hash}.d: {krate}/tests/it.rs\n\n{p}/it-{hash}: {krate}/tests/it.rs\n\n{krate}/tests/it.rs:\n",
                p = deps.display()
            ),
        )
        .unwrap();
    }
    let out = |calc: &str, geo: &str, code: u8| {
        format!(
            "{BUSY} printf '%s\\n' '     Running tests/it.rs (target/debug/deps/it-aaaa1111)' '' 'running 1 test' 'test works ... {calc}' '' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s' '' '     Running tests/it.rs (target/debug/deps/it-bbbb2222)' '' 'running 1 test' 'test works ... {geo}' '' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'; exit {code}"
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.a]\nrun = {:?}\n\n[suite.b]\nrun = {:?}\n",
            out("ok", "ok", 0),
            out("ok", "FAILED", 101),
        ),
    )
    .unwrap();
    let o = prova(&d, &["run", "a"]);
    assert!(text(&o).contains("tests: 2 passed"), "{}", text(&o));
    let out = text(&prova(&d, &["run", "b"]));
    assert!(out.contains("FAIL works (geo::it)"), "{out}");
    let all = text(&prova(&d, &["history", ""]));
    assert!(
        all.contains("works (calc::it)") && all.contains("works (geo::it)"),
        "{all}"
    );
}

/// Round 17 (rs-make): `quarantine add tests::flaky --in geo` replaced the
/// quarantine `--in calc` made: one entry per name. A quarantine belongs to
/// a name in a place; `remove` names the place too.
#[test]
fn a_quarantine_is_kept_per_name_and_place() {
    let d = workspace("qplace");
    two_crates(&d);
    prova(&d, &["run", "a"]);
    prova(&d, &["run", "b"]);
    for krate in ["calc", "geo"] {
        let o = prova(
            &d,
            &[
                "quarantine",
                "add",
                "tests::flaky",
                "--in",
                krate,
                "--reason",
                krate,
            ],
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    }
    let list = text(&prova(&d, &["quarantine", "list"]));
    assert!(
        list.contains("tests::flaky (calc)") && list.contains("tests::flaky (geo)"),
        "{list}"
    );
    // Both are in force: a failure of either is set aside, on its own.
    for suite in ["a", "b"] {
        let o = prova(&d, &["run", suite]);
        assert_eq!(o.status.code(), Some(0), "{suite}: {}", text(&o));
    }
    // Without the place, which one to remove is ambiguous.
    let o = prova(&d, &["quarantine", "remove", "tests::flaky"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("--in"), "{}", text(&o));
    let o = prova(&d, &["quarantine", "remove", "tests::flaky", "--in", "geo"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let list = text(&prova(&d, &["quarantine", "list"]));
    assert!(
        list.contains("tests::flaky (calc)") && !list.contains("(geo)"),
        "{list}"
    );
    // geo's failure counts again; calc's is still set aside.
    assert_eq!(prova(&d, &["run", "b"]).status.code(), Some(1));
    assert_eq!(prova(&d, &["run", "a"]).status.code(), Some(0));
}
