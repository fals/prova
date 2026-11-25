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
        "{BUSY} printf '%s\\n' '    Starting 3 tests across 2 binaries' '        PASS [   0.004s] calc tests::adds' '        {calc} [   0.004s] calc tests::flaky' '        {geo} [   0.004s] geo tests::flaky' '     Summary [   0.020s] 3 tests run'; exit {}",
        if calc == "FAIL" || geo == "FAIL" {
            100
        } else {
            0
        }
    )
}

fn two_crates(d: &Path) {
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.a]\nrun = {:?}\n\n[suite.b]\nrun = {:?}\n\n[suite.c]\nrun = {:?}\n",
            nextest("FAIL", "PASS"),
            nextest("PASS", "FAIL"),
            nextest("PASS", "PASS"),
        ),
    )
    .unwrap();
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
            "{BUSY} printf '%s\\n' '    Starting 2 tests across 2 binaries' '        {} [   0.004s] {} integ' '        {} [   0.004s] {} integ' '     Summary [   0.020s] 2 tests run'; exit {code}",
            first.0, first.1, second.0, second.1
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
    let sub = d.join("sub2");
    std::fs::create_dir_all(&sub).unwrap();
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
