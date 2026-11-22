# 0007: Experiments: pre-registered protocol, trials, verdict

Status: Proposed

## Context

For performance work, load tests and feature exploration, agents make ad hoc changes, run once, and compare single numbers. Nothing records inputs, baseline, or why a change was kept. Agent research loops in the wild keep a change when one run beats the best so far, with no statistics ([03](../research/03-formalization-experiments-storage.md)). The README asks for the scientific method, formalised, with cheap brute-force repetition.

## Decision

**Entities.**

- `experiment`: question and hypothesis with a direction.
- `protocol`: frozen, hashed. Primary metric, unit, better = lower or higher, guardrail metrics, minimum effect, alpha, trials per arm (minimum, maximum), stopping rule, workload command, inputs hash, controlled variables.
- `variant`: baseline or candidate. Code identity, parameters, parent variant.
- `trial`: one execution of the workload for one variant. State `waiting | running | complete | failed | pruned`. Environment fingerprint.
- `sample`: one metric value from a trial.
- `verdict`: variant pair, statistic, p-value, effect confidence interval, decision `accept | reject | inconclusive`, reason, protocol hash.

**Lifecycle rules (invariants).**

1. Protocol is frozen at the first trial. Changing it means a new experiment.
2. No verdict without a baseline and the minimum trials on both arms.
3. Completed trials are immutable.
4. A verdict is invalid when environment fingerprints of the two arms differ.
5. Terminal states are absorbing; resuming an experiment is idempotent.

**Statistics defaults.**

- Warmup trials discarded; 10 trials per arm, hard minimum 5; baseline and candidate interleaved to cancel drift.
- Exact two-sided Mann-Whitney U at alpha 0.05, plus a 95% confidence interval of relative change (Hodges-Lehmann or bootstrap).
- Accept only when the interval lies entirely beyond the minimum effect (default 2%) in the hypothesised direction and no guardrail regresses.
- Interval straddling the threshold = inconclusive, which schedules more trials up to the declared maximum.
- Outliers flagged, never dropped. Report median, MAD, interval.

**Improvement loop.** An experiment with many candidates and an append-only decision log (`keep | discard | crash`). Each keep requires a verdict against the current best. Multiple-comparison control (Holm). The final winner is re-validated against the original baseline with fresh trials.

**Metric ingestion.** Workload prints metrics in a simple declared form (JSON line `{"metric": ..., "value": ..., "unit": ...}`); adapters for criterion, hyperfine, k6 and similar come later. Export in Bencher Metric Format.

## Consequences

- Every kept change has a recorded, reproducible justification.
- Ten trials per arm costs time; the protocol makes that cost explicit and adjustable.
- Noise control (CPU pinning, governor) is recorded, not enforced, in v1.

## Alternatives

- Single-run comparison: fast, wrong under noise.
- Welch t-test default: assumes near-normal data; timing data is skewed.
