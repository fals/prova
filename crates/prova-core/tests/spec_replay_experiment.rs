//! Trace conformance for `spec/experiment.qnt` against `Comparison`.
//! Committed traces: `tests/traces-exp/`; CI also sets PROVA_TRACE_DIR_EXP.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use prova_core::experiment::{Arm, Comparison, Verdict};
use serde_json::Value;

fn int(v: &Value) -> u32 {
    v["#bigint"].as_str().unwrap().parse().unwrap()
}

fn verdict(v: &Value) -> Option<Verdict> {
    match v["tag"].as_str().unwrap() {
        "NoVerdict" => None,
        "Accept" => Some(Verdict::Accept),
        "Reject" => Some(Verdict::Reject),
        "Inconclusive" => Some(Verdict::Inconclusive),
        "Invalid" => Some(Verdict::Invalid),
        t => panic!("unknown verdict {t}"),
    }
}

fn check(v: &Value) -> Option<bool> {
    match v["tag"].as_str().unwrap() {
        "Unchecked" => None,
        "Holds" => Some(true),
        "Violated" => Some(false),
        t => panic!("unknown check {t}"),
    }
}

fn replay(path: &PathBuf) -> usize {
    let trace: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let states = trace["states"].as_array().unwrap();
    let mut c = Comparison::new(2, 4); // MIN_TRIALS, MAX_TRIALS in the spec
    for (i, s) in states.iter().enumerate() {
        let action = s["mbt::actionTaken"].as_str().unwrap_or("init");
        let picks = &s["mbt::nondetPicks"];
        let ctx = format!("{path:?} step {i} {action}");
        match action {
            "init" => {}
            "setProtocol" => assert!(
                c.can_change_protocol(),
                "{ctx}: engine refuses protocol change"
            ),
            "checkSpec" => {
                let arm = match picks["arm"]["value"]["tag"].as_str().unwrap() {
                    "Baseline" => Arm::Baseline,
                    _ => Arm::Candidate,
                };
                let holds = picks["holds"]["value"].as_bool().unwrap();
                assert!(c.check_spec(arm, holds), "{ctx}: engine refused spec check");
            }
            // The live tree changing is outside the engine: arms run from
            // their snapshots, so nothing in the comparison moves.
            "editWorkTree" => {}
            "rejectViolation" => {
                assert!(
                    c.reject_violation().is_some(),
                    "{ctx}: engine refused rejection"
                );
            }
            "trial" => {
                let fp = int(&picks["fp"]["value"]).to_string();
                assert!(c.trial(&fp).is_some(), "{ctx}: engine refused trial");
            }
            "decide" => {
                let o = verdict(&picks["o"]["value"]).unwrap();
                assert!(c.decide(o).is_some(), "{ctx}: engine refused decision");
            }
            a => panic!("unknown action {a}"),
        }
        assert_eq!(c.base, int(&s["baseTrials"]), "{ctx}: baseline trials");
        assert_eq!(c.cand, int(&s["candTrials"]), "{ctx}: candidate trials");
        assert_eq!(c.verdict, verdict(&s["verdict"]), "{ctx}: verdict");
        assert_eq!(c.base_spec, check(&s["baseSpec"]), "{ctx}: baseline spec");
        assert_eq!(c.cand_spec, check(&s["candSpec"]), "{ctx}: candidate spec");
        assert!(
            int(&s["measured"]) == 0 || int(&s["measured"]) == int(&s["recorded"]),
            "{ctx}: measured content differs from the recorded snapshot"
        );
        // Disabled actions in the spec must be refused by the engine too.
        let mut probe = c.clone();
        let spec_can_decide = s["verdict"]["tag"] == "NoVerdict" && c.base >= 2 && c.cand >= 2;
        let spec_can_trial = matches!(
            s["verdict"]["tag"].as_str(),
            Some("NoVerdict" | "Inconclusive")
        ) && s["baseSpec"]["tag"] == "Holds"
            && s["candSpec"]["tag"] == "Holds"
            && c.base + c.cand < 8;
        assert_eq!(
            c.next_arm().is_some(),
            spec_can_trial,
            "{ctx}: trial enabledness"
        );
        assert_eq!(
            probe.decide(Verdict::Accept).is_some(),
            spec_can_decide,
            "{ctx}: decide enabledness"
        );
    }
    states.len()
}

#[test]
fn comparison_conforms_to_experiment_spec() {
    let dir = std::env::var_os("PROVA_TRACE_DIR_EXP")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/traces-exp"));
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".itf.json"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let steps: usize = files.iter().map(replay).sum();
    assert!(steps > files.len());
}
