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
