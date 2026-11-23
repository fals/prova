---
name: prova-experiment
description: Decide whether a change is better (faster, fewer steps, more accurate, less memory, any numeric objective) with a pre-registered protocol, interleaved trials and a statistical verdict, optionally gated by a formal specification. Use for optimisation, benchmarking (go test -bench, criterion, pytest-benchmark, Vitest bench, JMH, BenchmarkDotNet), algorithm comparisons, load tests, or any "is it better" question, instead of timing or eyeballing runs by hand.
---

# Experiments with prova

One run proves nothing, and neither does a run you chose after seeing
several. prova fixes the question first, runs baseline and candidate
interleaved, and decides with a rank test and a confidence interval.

## 0. Is it the right question?

- What does "better" mean, as one number? Time per operation, accuracy on a
  frozen dataset, nodes expanded, retries, bytes allocated. Pick one primary
  metric; anything that must not get worse is a guardrail.
- What does "correct" mean? If the change touches an algorithm or a
  protocol, write the specification first (skill `prova-spec`) and register it
  with `--spec` and `--oracle`. A faster wrong answer must never win.
- What effect would matter? `--min-effect 5%` means a 4% gain is "no
  practical effect" and gets rejected. Choose it before measuring.

## 1. Find the metric

Run the workload once through prova to see what it can read:

```sh
prova exp metrics -- go test -run '^$' -bench CountPrimes -count 3 .
```

```
BenchmarkCountPrimes.ns_per_op       18348449.000 ns
wall_ms                              3625.753 ms
```

prova reads, from the workload's whole output:

| Source | Metric names |
|---|---|
| JSON lines `{"metric": "accuracy", "value": 0.93, "unit": "ratio"}` | as printed |
| `go test -bench` (median of `-count` repeats) | `BenchmarkX.ns_per_op`; `.bytes_per_op`, `.allocs_per_op` with `-benchmem`; custom `b.ReportMetric` units |
| criterion | `name.time_ns` (point estimate) |
| pytest-benchmark | `test_name.mean_ns`, `.median_ns` |
| Vitest bench | `name.mean_ns`, `name.hz` |
| JMH | `Class.method.ns_per_op` (time modes), `.ops_per_s` (throughput) |
| BenchmarkDotNet | `Method.mean_ns`; `.median_ns` when the summary table has a Median column |
| always | `wall_ms` of the whole command |

Times are normalized to nanoseconds, because tools switch units with speed.
For anything else (accuracy, steps, path cost), print a JSON line.

## 2. Declare before measuring

```sh
prova exp new <name> --hypothesis "<what you expect and why>" \
  --metric BenchmarkCountPrimes.ns_per_op --better lower --min-effect 5% \
  --trials 5 --max-trials 10 --warmup 1 -- <workload command>
```

- Baseline is `HEAD` (committed code); your change stays uncommitted.
- `--better higher` for accuracy, throughput, hit rate.
- `--guardrail mem_kb` (or `NAME:higher`) rejects a change that makes that
  metric worse, whatever the primary metric does.
- `--setup "npm ci"` runs once in each variant's directory before trials:
  the baseline worktree has no installed dependencies or build caches.
- `--spec spec/x.qnt --oracle "make conformance"`: see section 5.
- `--kill-after 2m` bounds one trial.
- `--change primes.go` (repeatable, globs) names the files a candidate may
  change. Without it, a candidate may change anything except the files that
  decide the verdict: tests, benchmarks, build and runner files (`*_test.go`,
  `test_*.py`, `*.bench.ts`, `src/test/**`, `Makefile`, `*.sh`, ...). A
  candidate that touches them is rejected unmeasured; if the change truly
  needs one (a new dependency in `Cargo.toml`), say so in a new experiment
  with `--change`.
- Specification files must be committed: `exp new` refuses a `--spec` file
  that differs from the baseline.
- The protocol freezes at the first trial. To change it, start a new
  experiment; never edit it after seeing data.

Workload rules: same inputs every trial (fixed seed, fixed dataset); exits 0;
prints the metric every trial. Native benchmark tools already repeat
internally: keep their own run short (a few seconds) and let prova repeat
trials, because prova's interleaving is what cancels machine drift.

Deterministic metrics (accuracy on a fixed set, step counts) are fine: equal
arms give p = 1, and constant but different arms are decided with the usual
minimum of five trials each. Keep `--trials 5` for them; more adds nothing.

## 3. Measure a candidate

```sh
<edit code>
prova exp candidate <name> --label <idea>   # records the working tree
prova exp run <name> --detach
prova exp wait <name> --max-wait 30s
```

`prova exp candidate` snapshots the working tree (a commit under
`refs/prova/candidates/`; your branch, index and stash are untouched), and every
arm runs in its own checkout. Editing files afterwards does not change what is
measured; record a new candidate to measure the edit. Git-ignored files are not
in the checkout: rebuild them with the protocol's `--setup`.

## 4. Act on the verdict

The verdict line gives the relative change, the medians and their ratio
(`improved +97.3%, median 18.07 -> 0.48 (37.6x)`), the confidence interval
and p. With few trials the exact rank test has a floor (5 + 5 trials:
p cannot go below 0.0079); the verdict says when p is at it. `adjusted`
p accounts for every comparison in the experiment (Holm), so an
improvement loop does not get lucky by trying many ideas.

- `ACCEPT`: the improvement interval clears the minimum effect. Keep it; it
  becomes the best, and the next idea is compared against it.
- `REJECT`: worse, no practical effect, a guardrail regressed, or the
  candidate violates the specification. Revert, or fix correctness first.
- `INCONCLUSIVE` turns into more trials up to `--max-trials`, then `REJECT`.
- `INVALID`: trials ran on different machines; rerun on one machine.
- Failed comparison: a trial or the setup crashed, the baseline violates the
  specification, a trial left a process running (it would slow the other
  arm), or the two arms printed the metric a different number of times per
  trial (forged or changed output); `prova exp log <name>` prints what failed
  (`--tail 40` for the end of each log).
- An ACCEPT of 1000x or more carries a warning: check for caching across
  iterations or a smaller workload before believing it.
- An ACCEPT found wrong afterwards: `prova exp best <name> baseline --reason
  "..."` sets the best back (recorded in the decision log), so later
  candidates are not compared against it.

prova guards the verdict against changed tests and benchmarks, a skipped
oracle, forged extra lines, leaked processes, code that rewrites tracked
files at run time (both checkouts are read-only and checked after every
oracle and trial), load aimed at one arm from outside the trials (judged
for trials of a second or more), and a weak reference: `--against` and
`exp best` take only the baseline or a kept variant, and an ACCEPT against
a baseline that is not an ancestor of the candidate says so. Trials do not
see `PROVA_HOME`, and recorded samples cannot be changed.

It cannot judge code that detects the oracle or the benchmark and behaves
differently there, or that rewrites its own output in process: read the
candidate's diff before reporting a win, large or not.

Before reporting: `prova exp revalidate <name>` runs the best against the
original baseline with fresh trials. `prova exp status <name>` shows the
decision log. Report the experiment name, the verdict line and the interval;
"it looked faster" is not a result.

## 5. Experiments under a specification

When correctness can be stated, it is stated before the experiment (skill
`prova-spec`), and the experiment only compares correct implementations:

```sh
prova exp new route --hypothesis "A* expands fewer nodes than BFS, same paths" \
  --spec spec/shortest.qnt --oracle "make conformance" \
  --metric nodes_expanded --better lower --min-effect 20% --trials 5 -- ./bench-route.sh
```

- The spec files are frozen by content hash in the protocol. A candidate
  that edits them is a violation. Freeze the oracle's own files too (its
  conformance test, its reference table, the Makefile target): pass each
  with `--spec`, or a candidate could weaken the check instead of the code.
- `prova exp protocol <name> --spec <file>` (before the first trial) changes
  only what it names and prints the resulting protocol; `prova exp status`
  shows it too.
- The oracle runs once per arm before any trial. A violating candidate is
  rejected without being measured; a violating baseline fails the
  comparison.
- Accuracy works the same way: `--spec data/eval.csv` freezes the dataset,
  the workload prints `{"metric": "accuracy", ...}`, `--better higher`.

## Patterns

- Improvement loop: one idea per candidate, each against the current best;
  `prova exp status` keeps the keep/discard log.
- Prepared alternatives (as in prova's own fixtures): keep each variant as a
  patch or file, apply, `prova exp candidate --label`, run, revert.
- Noisy machine: more trials does not fix a busy machine. Close the noise
  source, or use a deterministic metric (allocations, instructions, steps).
