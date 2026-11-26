//! `prova exp export`: the evidence behind an experiment's verdicts as one
//! versioned document (`prova.exp-export.v1`, `schemas/exp-export.v1.json`).
//! Read-only: it reads rows and never ends, repairs or writes anything, so
//! it is safe while a comparison runs (the document is then `partial`).

use rusqlite::params;
use schemars::JsonSchema;
use serde::Serialize;

use crate::experiment::{self, ExpError, Protocol};
use crate::store::Store;

/// The `schema` field of every export.
pub const SCHEMA: &str = "prova.exp-export.v1";

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExpExport {
    /// Always `prova.exp-export.v1`.
    pub schema: String,
    pub experiment: String,
    pub hypothesis: String,
    /// The frozen protocol as stored.
    pub protocol: Protocol,
    pub protocol_hash: String,
    /// The variant later candidates are compared against.
    pub best: String,
    /// A selected comparison was still running: its trials and samples are
    /// those recorded so far.
    pub partial: bool,
    /// Oldest first.
    pub comparisons: Vec<ExportComparison>,
    /// The decision log of the selected comparisons, oldest first.
    pub decisions: Vec<ExportDecision>,
    /// Times the best was set by hand or withdrawn.
    pub best_resets: Vec<ExportReset>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExportComparison {
    pub id: String,
    pub baseline: String,
    pub candidate: String,
    /// `running`, `decided`, `failed` or `cancelled`.
    pub status: String,
    /// `accept`, `reject`, `inconclusive` or `invalid`, once decided.
    pub verdict: Option<String>,
    pub created_ms: i64,
    pub ended_ms: Option<i64>,
    /// The full verdict details (statistics, reason, caveats), as stored.
    pub result: Option<serde_json::Value>,
    /// Every trial, warmups and failed ones included, in run order.
    pub trials: Vec<ExportTrial>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExportTrial {
    pub seq: u32,
    /// `baseline` or `candidate`.
    pub arm: String,
    pub warmup: bool,
    /// `complete` or `failed`.
    pub state: String,
    /// None when the workload could not start or was killed.
    pub exit_code: Option<i32>,
    /// The machine fingerprint the trial ran on.
    pub fingerprint: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub samples: Vec<ExportSample>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExportSample {
    pub metric: String,
    pub value: f64,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExportDecision {
    pub comparison: String,
    pub candidate: String,
    /// `keep`, `discard` or `crash`.
    pub decision: String,
    pub reason: String,
    pub created_ms: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExportReset {
    pub from_best: String,
    pub to_best: String,
    pub reason: String,
    pub created_ms: i64,
}

/// The export of an experiment: every comparison, or just `only`.
pub fn export(store: &Store, exp: &str, only: Option<&str>) -> Result<ExpExport, ExpError> {
    let e = experiment::get(store, exp)?;
    if let Some(id) = only {
        experiment::check_comparison(store, exp, id)?;
    }
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT id, baseline, candidate, status, verdict, result, created_ms, ended_ms
           FROM comparisons WHERE experiment = ?1 AND (?2 IS NULL OR id = ?2)
          ORDER BY created_ms, rowid",
    )?;
    let rows = stmt
        .query_map(params![exp, only], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Option<i64>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut trial_stmt = conn.prepare(
        "SELECT seq, arm, warmup, state, exit_code, fingerprint, started_ms, ended_ms
           FROM trials WHERE comparison = ?1 ORDER BY seq",
    )?;
    let mut sample_stmt = conn.prepare(
        "SELECT metric, value, unit FROM samples WHERE comparison = ?1 AND seq = ?2 ORDER BY metric",
    )?;
    let mut comparisons = Vec::new();
    for (id, baseline, candidate, status, verdict, result, created_ms, ended_ms) in rows {
        let trials = trial_stmt
            .query_map(params![id], |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i32>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::new();
        for (seq, arm, warmup, state, exit_code, fingerprint, started_ms, ended_ms) in trials {
            let samples = sample_stmt
                .query_map(params![id, seq], |r| {
                    Ok(ExportSample {
                        metric: r.get(0)?,
                        value: r.get(1)?,
                        unit: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            out.push(ExportTrial {
                seq,
                arm,
                warmup,
                state,
                exit_code,
                fingerprint,
                started_ms,
                ended_ms,
                samples,
            });
        }
        comparisons.push(ExportComparison {
            id,
            baseline,
            candidate,
            status,
            verdict,
            created_ms,
            ended_ms,
            result: result.and_then(|r| serde_json::from_str(&r).ok()),
            trials: out,
        });
    }
    let partial = comparisons.iter().any(|c| c.status == "running");
    let decisions = conn
        .prepare(
            "SELECT comparison, candidate, decision, reason, created_ms FROM decisions
              WHERE experiment = ?1 AND (?2 IS NULL OR comparison = ?2) ORDER BY created_ms, rowid",
        )?
        .query_map(params![exp, only], |r| {
            Ok(ExportDecision {
                comparison: r.get(0)?,
                candidate: r.get(1)?,
                decision: r.get(2)?,
                reason: r.get(3)?,
                created_ms: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let best_resets = conn
        .prepare(
            "SELECT from_best, to_best, reason, created_ms FROM best_resets
              WHERE experiment = ?1 ORDER BY created_ms, rowid",
        )?
        .query_map(params![exp], |r| {
            Ok(ExportReset {
                from_best: r.get(0)?,
                to_best: r.get(1)?,
                reason: r.get(2)?,
                created_ms: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ExpExport {
        schema: SCHEMA.to_owned(),
        experiment: e.name,
        hypothesis: e.hypothesis,
        protocol: e.protocol,
        protocol_hash: e.protocol_hash,
        best: e.best,
        partial,
        comparisons,
        decisions,
        best_resets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiment::Better;

    /// A running comparison whose worker is gone: `status` would end it as
    /// failed, an export must leave it as it found it.
    #[test]
    fn exporting_a_comparison_whose_worker_died_writes_nothing() {
        let d = std::env::temp_dir().join(format!(
            "prova-export-{}-{}",
            std::process::id(),
            crate::ids::new_run_id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let s = Store::open_dir(&d, &d.join(".prova")).unwrap();
        let p = Protocol {
            metric: "t".into(),
            unit: None,
            better: Better::Lower,
            min_effect: 0.02,
            alpha: 0.05,
            trials: 5,
            min_trials: 5,
            max_trials: 5,
            warmup: 0,
            guardrails: Vec::new(),
            command: vec!["true".into()],
            kill_after_ms: 1000,
            setup: None,
            specs: Vec::new(),
            oracle: None,
            scope: Vec::new(),
            scope_only: false,
        };
        let c = s.conn();
        c.execute(
            "INSERT INTO experiments (name, hypothesis, protocol, protocol_hash, cwd, created_ms, best)
             VALUES ('e', 'h', ?1, ?2, '', 1, 'baseline')",
            params![serde_json::to_string(&p).unwrap(), p.hash()],
        )
        .unwrap();
        for v in ["baseline", "c1"] {
            c.execute(
                "INSERT INTO variants (experiment, name, kind, code, params, created_ms)
                 VALUES ('e', ?1, 'workdir', 'x', '{}', 1)",
                params![v],
            )
            .unwrap();
        }
        // No process has this pid: the worker is dead.
        c.execute(
            "INSERT INTO comparisons (id, experiment, baseline, candidate, status, worker_pid, created_ms)
             VALUES ('c-1', 'e', 'baseline', 'c1', 'running', 2147483646, 5)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO trials (comparison, seq, arm, warmup, state, exit_code, fingerprint, started_ms, ended_ms)
             VALUES ('c-1', 1, 'baseline', 1, 'complete', 0, 'f', 1, 2)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO samples (comparison, seq, metric, value, unit) VALUES ('c-1', 1, 't', 3.5, 'ms')",
            [],
        )
        .unwrap();
        let x = export(&s, "e", None).unwrap();
        assert!(x.partial);
        assert_eq!(x.comparisons[0].status, "running");
        let t = &x.comparisons[0].trials[0];
        assert!(t.warmup);
        assert_eq!(t.samples[0].unit.as_deref(), Some("ms"));
        let (status, decisions): (String, i64) = c
            .query_row(
                "SELECT status, (SELECT COUNT(*) FROM decisions) FROM comparisons WHERE id = 'c-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "running", "export must not end the comparison");
        assert_eq!(decisions, 0);
        assert!(export(&s, "e", Some("nope")).is_err());
    }
}
