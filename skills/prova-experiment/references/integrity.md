# What prova checks in an experiment

prova defends a verdict against honest mistakes: the harness changing by
accident, a workload that misreports, a process left running. It does not
defend against a candidate written to fool the measurement (ADR 0037): the
arms run as the same user in their own checkouts, see the session's
environment, and can write anywhere that user can. Read the candidate's diff.

## What it checks

- Each arm has its own checkout and its own build caches (Go, npm, XDG);
  trials do not see `PROVA_HOME` or Python's user site. Trials run in ABBA
  order.
- Changed tests, benchmarks, build files and expected values an oracle reads
  (`testdata`, `fixtures`, snapshots, golden files, `build.rs`, `Makefile`,
  `go.mod`, ...): out of scope unless the protocol allows them with
  `--change`; the candidate is rejected unmeasured.
- A skipped oracle: the candidate's oracle must pass the same tests as the
  baseline's, not only exit 0.
- Forged or missing metric lines: both arms must report the metric the same
  number of times per trial.
- Leaked processes: anything a trial leaves running is killed and the
  comparison is void.
- Tracked files rewritten: a trial that does voids the comparison, an oracle
  that does rejects the candidate.
- A weak reference: `--against` and `exp best` take only the baseline or a
  kept variant, and an ACCEPT against a baseline that is not an ancestor of
  the candidate says so.
- A win of 1000x or more carries a warning (caching across iterations, a
  smaller workload).
- A revalidation of the best that does not hold, or is void, withdraws the
  accept.

## What it cannot judge

Code that detects the oracle or the benchmark and behaves differently there,
that writes outside its checkout for the next iteration to read, or that
rewrites its own output in process (code imported by the oracle's tests can
print passing lines and exit 0: the same tests must pass as in the
baseline's oracle, which a forger who read them can fake). Read the
candidate's diff before reporting a win, large or not.
