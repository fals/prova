# What prova guards in an experiment

## Isolation of the arms

Where bubblewrap is installed, each arm runs sandboxed (`sandbox:` line in
the verdict): it can write only its own checkout, a private state directory
and a private `/tmp`, with no network for trials and the oracle unless the
protocol has `--allow-network`. Each arm has its own build caches, and
secret-named variables are not passed. Without bubblewrap, the verdict says
the arms were not isolated.

Trials run in ABBA order. Trials do not see `PROVA_HOME`.

## Checks on the verdict

prova guards the verdict against:

- changed tests and benchmarks;
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

Recorded samples cannot be changed: `exp status` re-checks every ACCEPT
against its samples and warns if the record was changed.

## What it cannot judge

Code that detects the oracle or the benchmark and behaves differently
there, or that rewrites its own output in process. Read the candidate's
diff before reporting a win, large or not.
