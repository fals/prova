# 0032: A revalidation replicates and adds no comparison; the oracle leaves nothing behind; recording a result does not read the whole history

Status: Accepted (2026-11-25). Amends [0007](0007-experiments.md) (Holm family and the re-validation of the winner), [0021](0021-verdicts-under-adversarial-candidates.md) (what a candidate may change; its Consequences) and [0023](0023-evidence-hygiene-and-experiment-isolation.md) (its same-user limit). Superseded in part by [0037](0037-prova-keeps-what-it-can-know.md).

## Context

Three items stayed open after usability round 17.

1. **Replications were penalised like new hypotheses.** 0007 re-validates the
   winner against the original baseline with fresh trials, and Holm
   correction runs across the comparisons of an experiment. A revalidation
   counted as one more comparison: `adjusted p=0.0159 for 2 comparison(s)`
   for the same hypothesis tested again. With exact tests at five trials per
   arm the smallest possible p is 0.0079; the seventh comparison puts it
   above 0.05, so replicating a real effect enough times turned it into
   INCONCLUSIVE. A replication that failed, on the other side, only left a
   REJECT in the log: the accept it contradicted stayed the best.
2. **The oracle can write where the trials read.** 0021 and 0023 say the
   candidate's code runs inside the oracle as the same user and "the limit
   stands". Since 0024 the arms are sandboxed, but the oracle and the trials
   of an arm share one checkout. A candidate whose code ran in the oracle
   planted an untracked file next to the baseline's own workload; the trials
   read it and the candidate was ACCEPTED at 10x (probe in
   `a_candidate_cannot_plant_files_during_the_oracle_for_the_trials`). Files
   an oracle reads as expected values (`testdata`, golden files, snapshots)
   were also in the candidate's default scope.
3. **Recording a result cost milliseconds per test once history existed**
   (issue #91): replaying 1500 Go `--- PASS` lines took 0.2 s with no history,
   3.1 s with one earlier run and 6.1 s with two, growing with every run. The
   supervisor reads the history of each test as it starts to set its slow
   budget; it read the whole analysis of the test (flaky statistics, suites,
   failure messages) to use one number, the p95 of its passing durations. A
   slow recorder also made counts trail the framework and dropped the end of
   a run at the post-exit drain.

## Decision

1. **A revalidation replicates a hypothesis; it adds no comparison to the
   Holm family.** A comparison is a revalidation when its candidate is
   measured against the original baseline and an earlier comparison of that
   candidate against it was accepted. It records `revalidates: <that
   comparison>` in its result. For its own adjusted p, the p it replicates is
   replaced by its own in the family (the family keeps its size); revalidations
   never count in the family of any other comparison. Its raw p is reported
   as its own, with the comparison it replicates. Records made before this
   decision carry no marker and are judged as they were.
2. **A replication that does not hold withdraws the accept.** A revalidation
   of the current best that ends REJECT or INCONCLUSIVE (INVALID says nothing
   about the effect) puts the best back to what the accepted comparison was
   measured against, as a recorded best reset, and the decision says so. The
   withdrawn variant is no longer a reference (`exp best` and `--against`
   refuse it), as after any reset. `exp status` still shows the accept it
   withdrew.
3. **The oracle runs on a throwaway view of the checkout.** With bubblewrap
   and overlay support (Linux), the oracle sees its arm's checkout through an
   overlay whose writes land in a scratch directory that is deleted when the
   oracle ends. Nothing it writes there reaches the trials, the setup, the
   other arm or a later comparison; only its exit status and its log, which
   prova holds, count. A tracked file the oracle rewrote is found in that
   scratch layer and is a violation (the check that already covered the real
   checkout). An oracle that builds for the trials must build in `--setup`.
   The verdict's `sandbox:` line says whether the oracle was isolated.
4. **Expected values and build steps are harness files.** `testdata`,
   `fixtures`, `__snapshots__` and `golden` directories, `*.golden`, `*.snap`
   and `build.rs` join the files a candidate may not change by default
   (0021 §1).
5. **Recording reads the p95, not the analysis.** The slow budget of a test
   that starts reads only the p95 of its recent passing durations (finished,
   remembered, verified runs; zero-only durations count as unknown; a test
   not yet recorded under this id is known by its name from other suites):
   `analysis::usual_p95_ms`, a bounded query, instead of `analysis::stats`.
   The cost of a result no longer grows with history. The evidence is a prova
   experiment (`recording-cost`: frozen protocol, an oracle that the same 1500
   results are recorded, interleaved trials, Mann-Whitney): ACCEPT, median
   3239 ms to 225 ms (14.4x, p = 0.0043, comparison 3e0850cf), and again with
   fresh trials, 3337 ms to 225 ms (14.8x, comparison 9cdfff10).

## Consequences

- Repeating `exp revalidate` is free of statistical cost and can only end in
  one of two ways: it holds, or the accept is withdrawn. Its p is read next to
  the original, not instead of it.
- A candidate can no longer use the oracle to leave something for the trials.
  It can still, with code the oracle imports, print the passing lines of the
  baseline's oracle and exit 0 (0020 requires the same tests to pass, which a
  forger who read them can fake). That is in-process forgery by code under
  test: the oracle's process and the candidate's code are one process and no
  boundary prova can draw between them separates their output. Reading the
  candidate's diff stays the reviewer's job, and the skills say so.
- Where prova cannot sandbox (no bubblewrap: macOS, containers without user
  namespaces) or the system cannot do overlays, the oracle's writes persist
  as before and the verdict says so in its `sandbox:` line. There is no
  equivalent defence there yet; a cheap detection of new untracked files was
  rejected because builds and test runners create such files legitimately.
- An oracle that relied on building into the checkout for the trials breaks
  loudly (the trials fail to find the build) and is fixed by moving the build
  to `--setup`.
- The oracle's build outputs inside the checkout are discarded, so the first
  trial may rebuild what the oracle already built; both arms are treated alike.

## Alternatives

- Counting each revalidation as a new comparison, with a larger family: what
  0007 did; it punishes a real effect for being checked.
- Merging a revalidation's samples with the original comparison's: pools
  trials that were run under different load, and the original decision was
  made on its own data.
- Copying the checkout for the oracle: costs time and space for every large
  checkout; the overlay copies nothing until it is written.
- Re-running the oracle with the candidate's changed files removed, as a
  witness that the oracle can fail: the forging code is in those files, so
  removing them removes the forger as well as the implementation.
- A cache or a materialized view of each test's durations for the slow
  budget: a second source of truth for a number one bounded query gives.
