//! Trace conformance (ADR 0011): replay traces generated from `spec/run.qnt`
//! against `Lifecycle` and compare state after every step.
//!
//! Committed traces live in `tests/traces/`. CI also generates fresh ones and
//! points `TOE_TRACE_DIR` at them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;
use toe_core::lifecycle::Lifecycle;
use toe_core::model::{Outcome, Status};

fn tag(v: &Value) -> &str {
    v["tag"].as_str().expect("sum type tag")
}

fn pick<'a>(state: &'a Value, name: &str) -> &'a Value {
    &state["mbt::nondetPicks"][name]["value"]
}

fn status(v: &Value) -> Status {
    Status::parse(&tag(v).to_lowercase()).expect("status")
}

fn outcome(v: &Value) -> Outcome {
    Outcome::parse(&tag(v).to_lowercase()).expect("outcome")
}

fn int(v: &Value) -> u64 {
    v["#bigint"].as_str().expect("bigint").parse().expect("int")
}

fn spec_open(state: &Value) -> BTreeSet<String> {
    state["open"]["#set"]
        .as_array()
        .expect("set")
        .iter()
        .map(|t| t.as_str().expect("str").to_owned())
        .collect()
}

fn spec_outcomes(state: &Value) -> BTreeMap<String, Outcome> {
    state["outcomes"]["#map"]
        .as_array()
        .expect("map")
        .iter()
        .map(|kv| (kv[0].as_str().expect("key").to_owned(), outcome(&kv[1])))
        .collect()
}

fn replay(path: &PathBuf) -> usize {
    let text = std::fs::read_to_string(path).expect("read trace");
    let trace: Value = serde_json::from_str(&text).expect("parse trace");
    let states = trace["states"].as_array().expect("states");
    let mut l = Lifecycle::new();
    for (i, s) in states.iter().enumerate() {
        let action = s["mbt::actionTaken"].as_str().unwrap_or("init");
        let t = || pick(s, "t").as_str().expect("t").to_owned();
        let r = match action {
            "init" | "setInputs" => Ok(()),
            "start" => l.start(),
            "testStarted" => l.test_started(&t()),
            "testFinished" => l.test_finished(&t(), outcome(pick(s, "o"))),
            "exited" => l.exited(Some(int(pick(s, "code")) as i32)),
            "contradicted" => l.contradict(),
            "end" => {
                // The spec's exit code defaults to 0 before an `exited` step.
                if l.exit_code().is_none() {
                    l.exited(Some(int(&states[i - 1]["exitCode"]) as i32))
                        .unwrap();
                }
                l.end(status(pick(s, "s"))).map(|_| ())
            }
            "cancelCreated" => l.end(Status::Cancelled).map(|_| ()),
            "late" => match l.test_finished(&t(), outcome(pick(s, "o"))) {
                Err(_) => Ok(()),
                Ok(()) => panic!("{path:?} step {i}: engine accepted a late event"),
            },
            other => panic!("unknown action {other}"),
        };
        r.unwrap_or_else(|e| panic!("{path:?} step {i} {action}: engine refused: {e}"));
        let ctx = format!("{path:?} step {i} after {action}");
        assert_eq!(l.status(), status(&s["status"]), "{ctx}: status");
        assert_eq!(l.open(), &spec_open(s), "{ctx}: open tests");
        assert_eq!(l.outcomes(), &spec_outcomes(s), "{ctx}: outcomes");
        assert_eq!(l.dropped(), int(&s["dropped"]), "{ctx}: dropped");
        let spec_verdict = match tag(&s["verdict"]) {
            "Undecided" => None,
            "Pass" => Some(Outcome::Passed),
            "Fail" => Some(Outcome::Failed),
            "Error" => Some(Outcome::Errored),
            v => panic!("{ctx}: unknown verdict {v}"),
        };
        let verdict = if l.status() == Status::Cancelled && action == "cancelCreated" {
            Some(Outcome::Errored)
        } else {
            l.verdict()
        };
        assert_eq!(verdict, spec_verdict, "{ctx}: verdict");
    }
    states.len()
}

fn trace_files(dir: PathBuf) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{dir:?}: {e}"))
        .map(|e| e.expect("entry").path())
        .filter(|p| p.to_string_lossy().ends_with(".itf.json"))
        .collect();
    v.sort();
    v
}

#[test]
fn engine_conforms_to_run_spec() {
    let dir = std::env::var_os("TOE_TRACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/traces"));
    let files = trace_files(dir);
    assert!(!files.is_empty(), "no traces found");
    let steps: usize = files.iter().map(replay).sum();
    assert!(steps > files.len(), "traces have no steps");
}
