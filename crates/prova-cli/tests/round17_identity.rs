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
