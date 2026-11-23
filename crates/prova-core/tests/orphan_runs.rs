//! Medium: a run whose supervisor is gone is never shown as running
//! (usability round 3: in containers the dead supervisor's pid was reused,
//! and runs stayed "running" forever). Needs real processes and a store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use prova_core::engine::{WaitReason, summarize};
use prova_core::model::Status;
use prova_core::store::{NewRun, Store};
use serde_json::json;

#[test]
fn a_reused_pid_does_not_keep_a_run_alive() {
    let d = std::env::temp_dir().join(format!("prova-orphan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let s = Store::open_dir(&d, &d.join("store")).unwrap();
    let v = json!({});
    s.create_run(&NewRun {
        id: "r1",
        suite: "t",
        spec: &json!({"command": {"argv": ["true"]}, "stall_after_ms": 1000, "kill_after_ms": 1000}),
        cwd: ".",
        code: "nogit",
        env: &v,
        machine: &v,
        origin: &v,
        created_ms: prova_core::ids::now_ms(),
    })
    .unwrap();
    // A live process that is not the run's supervisor holds the pid.
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    s.mark_started("r1", other.id(), prova_core::ids::now_ms())
        .unwrap();
    let summary = summarize(&s, "r1", WaitReason::Running, 0).unwrap();
    other.kill().unwrap();
    let _ = other.wait();
    assert_eq!(summary.status, Status::Aborted);
    assert!(summary.detail.unwrap().contains("supervisor exited"));
}

/// A run's process group id is reused by an unrelated group after the run
/// (a fork-heavy CI job froze its runner): only processes carrying the
/// run's id are killed.
#[test]
fn leftovers_are_killed_only_when_they_belong_to_the_run() {
    use std::os::unix::process::CommandExt;
    let spawn = |env: Option<&str>| {
        let mut c = std::process::Command::new("sleep");
        c.arg("30").process_group(0);
        if let Some(run) = env {
            c.env("PROVA_RUN_ID", run);
        }
        c.spawn().unwrap()
    };
    let mut foreign = spawn(None);
    let pgid = foreign.id();
    assert_eq!(prova_core::process::kill_run_processes(pgid, "r1"), 0);
    assert!(
        foreign.try_wait().unwrap().is_none(),
        "an unrelated group was killed"
    );
    foreign.kill().unwrap();
    foreign.wait().unwrap();

    let mut ours = spawn(Some("r1"));
    assert_eq!(prova_core::process::kill_run_processes(ours.id(), "r1"), 1);
    assert!(!ours.wait().unwrap().success());
}
