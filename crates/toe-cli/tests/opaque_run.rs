//! Medium tests: real processes, real store, the real binary (ADR 0005).
//! No sleeps: every wait goes through `toe wait` with a bound.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOE: &str = env!("CARGO_BIN_EXE_toe");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("toe-it-{name}-{}", std::process::id()));
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

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

fn detach(dir: &Path, extra: &[&str], script: &str) -> String {
    let mut args = vec!["run", "--detach", "--json"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--", "sh", "-c", script]);
    let o = toe(dir, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    json(&o)["run"].as_str().unwrap().to_owned()
}

#[test]
fn passing_and_failing_commands_map_to_exit_codes() {
    let d = workspace("exit");
    let ok = toe(&d, &["run", "--", "sh", "-c", "echo fine"]);
    assert_eq!(ok.status.code(), Some(0), "{}", stdout(&ok));
    let bad = toe(
        &d,
        &["run", "--json", "--", "sh", "-c", "echo 'boom 42'; exit 3"],
    );
    assert_eq!(bad.status.code(), Some(1));
    let s = json(&bad);
    assert_eq!(s["outcome"], "failed");
    assert_eq!(s["exit_code"], 3);
    assert_eq!(s["level"], "opaque");
    assert_eq!(s["output_tail"][0], "boom 42");
}

#[test]
fn same_failure_is_recognised_across_runs() {
    let d = workspace("history");
    let script = "echo 'expected 1 got 2 at 0xdead'; exit 1";
    toe(&d, &["run", "--", "sh", "-c", script]);
    let second = json(&toe(&d, &["run", "--json", "--", "sh", "-c", script]));
    assert_eq!(second["history"]["previous_runs"], 1);
    assert_eq!(second["history"]["same_failure_seen"], 1);
}

#[test]
fn idle_process_is_reported_as_stall_and_can_be_cancelled() {
    let d = workspace("stall");
    let id = detach(&d, &["--stall-after", "1s"], "echo up; exec sleep 60");
    let w = toe(&d, &["wait", &id, "--max-wait", "20s", "--json"]);
    assert_eq!(w.status.code(), Some(3), "{}", stdout(&w));
    assert_eq!(json(&w)["reason"], "stall");
    assert!(toe(&d, &["cancel", &id]).status.success());
    let done = toe(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    );
    let s = json(&done);
    assert_eq!(s["status"], "cancelled");
    assert_eq!(done.status.code(), Some(3));
}

#[test]
fn busy_process_is_not_a_stall() {
    let d = workspace("busy");
    let id = detach(&d, &["--stall-after", "1s"], "while :; do :; done");
    let w = toe(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "stall",
            "--max-wait",
            "3s",
            "--json",
        ],
    );
    assert_eq!(
        w.status.code(),
        Some(5),
        "still running, never stalled: {}",
        stdout(&w)
    );
    toe(&d, &["cancel", &id]);
    toe(&d, &["wait", &id, "--until", "done", "--max-wait", "20s"]);
}

#[test]
fn kill_after_bounds_the_run() {
    let d = workspace("ceiling");
    let id = detach(&d, &["--kill-after", "1s"], "exec sleep 60");
    let w = toe(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    );
    let s = json(&w);
    assert_eq!(s["status"], "aborted");
    assert!(s["detail"].as_str().unwrap().starts_with("killed"));
    assert_eq!(w.status.code(), Some(3));
}

#[test]
fn leaked_children_are_reported_and_killed() {
    let d = workspace("leak");
    let id = detach(&d, &[], "sleep 60 & echo spawned");
    let s = json(&toe(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    ));
    assert_eq!(s["outcome"], "passed");
    let leaked = s["leaked"].as_array().expect("leaked list");
    assert_eq!(leaked.len(), 1);
    let pid = leaked[0].as_u64().unwrap();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists() || {
            let st = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            st.contains(") Z ")
        }
    );
}

#[test]
fn dead_supervisor_leaves_run_aborted_not_running() {
    let d = workspace("orphan");
    let id = detach(&d, &[], "exec sleep 60");
    let status = json(&toe(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "1s", "--json"],
    ));
    assert_eq!(status["status"], "running");
    let pgid = run_started_pid(&d, &id);
    Command::new("kill")
        .args(["-9", &supervisor_pid(&d, &id).to_string()])
        .status()
        .unwrap();
    let s = json(&toe(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "10s",
            "--json",
        ],
    ));
    assert_eq!(s["status"], "aborted");
    // The test process outlives its supervisor in its own group; clean up.
    Command::new("kill")
        .args(["-9", &format!("-{pgid}")])
        .status()
        .unwrap();
}

fn run_started_pid(d: &Path, id: &str) -> u64 {
    let o = toe(d, &["events", id]);
    let first: serde_json::Value =
        serde_json::from_str(stdout(&o).lines().next().unwrap()).unwrap();
    first["pid"].as_u64().unwrap()
}

/// Reads the supervisor pid via `toe status --json` of the run list.
fn supervisor_pid(d: &Path, id: &str) -> u32 {
    let runs = json(&toe(d, &["status", "--json"]));
    runs.as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .and_then(|r| r["supervisor_pid"].as_u64())
        .expect("supervisor pid") as u32
}

#[test]
fn events_are_json_lines_in_order() {
    let d = workspace("events");
    toe(&d, &["run", "--", "sh", "-c", "echo x"]);
    let o = toe(&d, &["events", "last"]);
    let lines: Vec<serde_json::Value> = stdout(&o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.first().unwrap()["kind"], "run_started");
    assert_eq!(lines.last().unwrap()["kind"], "run_finished");
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
}

#[test]
fn suite_from_toe_toml_with_selection() {
    let d = workspace("suite");
    std::fs::write(
        d.join("toe.toml"),
        "[suite.small]\nrun = \"echo all\"\nselect = \"echo only {filter}\"\nsize = \"small\"\n",
    )
    .unwrap();
    let o = toe(&d, &["run", "small", "--select", "parser", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let out = toe(&d, &["output", "last"]);
    assert_eq!(stdout(&out).trim(), "only parser");
    let none = toe(&d, &["run", "nope"]);
    assert_eq!(none.status.code(), Some(4));
}
