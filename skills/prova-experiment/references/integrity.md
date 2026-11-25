# What prova guards in an experiment

## Isolation of the arms

Where bubblewrap is installed, each arm runs sandboxed (`sandbox:` line in
the verdict): it can write only its own checkout, a private state directory
and a private `/tmp`, with no network for trials and the oracle unless the
protocol has `--allow-network`. Each arm has its own build caches, and
secret-named variables are not passed. Without bubblewrap, the verdict says
the arms were not isolated.

The oracle runs the candidate's own code, so it sees the checkout through a
throwaway layer: what it writes there (a planted result file, a binary, a
marker) is gone before the trials, and only its exit status and log count.
Build in `--setup`, not in the oracle, if the trials need the output. Without
bubblewrap, or where the system cannot do this, the `sandbox:` line says what
the oracle wrote stayed.

Trials run in ABBA order. Trials do not see `PROVA_HOME`.

## Checks on the verdict

prova guards the verdict against:

- changed tests and benchmarks, and the expected values and build scripts an
  oracle reads (`testdata`, `fixtures`, snapshots, golden files, `build.rs`),
  runner configuration that names the program a tool runs (`.npmrc`,
  `.yarnrc*`, `.cargo/config*`, `rust-toolchain`, `rust-toolchain.toml`, `go.env`, `pip.conf`,
  `sitecustomize.py`) and compiled modules (`*.pyc`, `__pycache__/`);
- a skipped oracle;
- forged extra metric lines;
- leaked processes;
- code that rewrites tracked files at run time (both checkouts are
  read-only and checked after every oracle and trial);
- load aimed at one arm from outside the trials (judged for trials of a
  second or more);
- a weak reference: `--against` and `exp best` take only the baseline or a
  kept variant, and an ACCEPT against a baseline that is not an ancestor of
  the candidate says so.

A trial's private `/tmp` is looked at when the trial ends: files left there
are named in a `caveat:` (a result cache kept between iterations). A
revalidation of the best that is void because of the candidate (its own median moved, forged lines, leaked processes, tampering), like one that fails, withdraws the
accept; one the machine spoiled (both arms moved alike, load on one arm, a stopped arm) is inconclusive and the accept stands. Files a trial leaves in `/tmp` that the baseline's trials do not leave are what is named.

Known limit: a candidate that makes every revalidation inconclusive (for
example by being slow or noisy only when revalidated) keeps its accept. An
accept whose revalidations stay inconclusive is unconfirmed: read its diff.

Recorded samples cannot be changed: `exp status` re-checks every ACCEPT
against its samples and warns if the record was changed.

## What it cannot judge

Code that detects the oracle or the benchmark and behaves differently
there, or that rewrites its own output in process (code imported by the
oracle's tests can print passing lines and exit 0: the same tests must pass
as in the baseline's oracle, which a forger who read them can fake). Read the candidate's
diff before reporting a win, large or not.
