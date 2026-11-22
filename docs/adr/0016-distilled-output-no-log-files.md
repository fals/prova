# 0016: Distilled output: everything in SQLite, no log files after a run

Status: Proposed. Supersedes the raw-output parts of [0001](0001-architecture-overview.md) (`.toe/` holds raw output artifacts), [0008](0008-storage-sqlite.md) (content-addressed artifact files, `toe gc`) and [0013](0013-black-box-adoption.md) ("raw output and exit code are always kept"), and the AGENTS.md rule "raw output is always kept as an artifact".

## Context

Every run leaves `runs/<id>/output.log` and `supervisor.log` in the toe
home, result files are copied as artifacts, experiments keep one log per
trial and a git worktree per baseline. Nothing removes them. On a large suite
run many times a day by agents, that is an unbounded pile on the user's disk,
most of it the output of passing tests that nobody reads.

What is actually read, and by whom:

- **Agents** read failures: which test, its message, its own output, and for
  failures outside any test (compile errors, a crashed runner, an opaque
  command) the lines that explain them. They read them through `toe wait`,
  `toe status`, `toe output` and `--json`, never the files.
- **Humans** read the same through the CLI, as text.
- **toe** reads `output.log` today only to print the last lines of a run
  (`output_tail`), which is a defect: the summary depends on a file.
- Full raw output of an old passing run is read by no one. Its value is
  re-parsing with a better recognizer, a toe-development concern.

Prior art keeps the failure and drops the rest. nextest's JUnit report stores
output for failing tests and not for passing ones by default
(`store-success-output = false`, `store-failure-output = true`) [1]. Bazel
keeps one `test.log` per target under `bazel-testlogs` and zips any extra
outputs into `outputs.zip` [2]. For the storage itself, SQLite measures small
blobs (around 10 KB) read and written about 35% faster than one file each,
and packs them without per-file block padding [3]; below roughly 100 KB a blob
inside the database is faster than a separate file [4].

## Decision

1. **After a run ends, a run is rows in `toe.db` and nothing else.** One
   SQLite file per repository, as 0001 intended. No `runs/`, `artifacts/` or
   per-trial log files remain.
2. **While a run is live**, its output goes to a private spool file, so
   `toe output <run>` and `toe wait` can show it as it happens. The spool is
   deleted when the run ends, after distillation. Disk use while running is
   bounded by the number of concurrent runs.
3. **Distillation at the end of a run** keeps what an agent needs, compressed
   (deflate) in the database:
   - per test, as today: outcome, duration, message; **plus, for failing
     tests, the test's own output** (from the stream, the recognizer or the
     result file's `system-out`), up to 64 KB each;
   - per run: an **excerpt**: the first lines, every block that looks like an
     error (compiler and runner error markers, panics, tracebacks) with a few
     lines of context, and the last lines, up to 64 KB, with `[... N lines
     elided ...]` where output was cut. A passing run keeps only its last 20
     lines.
   - result files and streams (JUnit, TRX, libtest JSON, reportlog) are
     parsed into rows; their raw bytes are not kept.
   - the excerpt replaces the file read in summaries: `output_tail` comes from
     the database.
4. **A budget, not a schedule.** Run rows, test results and statistics are
   kept forever (they are small and they are the history). Excerpts and
   failure output share a per-repository budget (64 MB compressed); when it
   is exceeded, the oldest are dropped first and the run keeps a note that
   its output was pruned. No `toe gc`, no age setting, no daemon.
5. **Experiments** keep metrics and verdicts; a trial's output is kept, as an
   excerpt, only when the trial or the setup or the oracle failed. Baseline
   worktrees are removed when the comparison ends.
6. **Reading it.** `toe output <run>` prints the excerpt (or the live spool)
   as text for humans; `toe output <run> --test <name>` prints one failing
   test's output; `--json` returns the sections (head, error blocks, tail,
   elided line counts) for agents. Nobody needs to read the database or a
   compressed blob directly.
7. **Escape hatch for toe development:** `toe run --keep-output` stores the
   whole output, compressed, within the same budget, so a recognizer can be
   debugged on what the framework really printed. Off by default.
8. **Migration:** on first open, existing `runs/*/output.log` files are
   distilled into excerpts, then the files, `artifacts/`, finished
   experiments' logs and stale worktrees are deleted.

## Consequences

- Disk use per repository is bounded and small, whatever the suite size or
  run frequency.
- The full output of a finished run is gone unless `--keep-output` was used;
  old runs cannot be re-parsed by an improved recognizer.
- An error printed outside the head, the tail and the recognized error blocks
  of a long output can be lost. The error-block patterns are the place to
  improve, tested on fixture output.
- One new dependency: `flate2` with its pure-Rust backend (`miniz_oxide`), no
  C code, so static musl builds stay simple.

## Alternatives

- **Keep logs, prune by age (`toe gc`)**: the pile still grows between
  prunes, needs a setting and a trigger, and keeps mostly passing output.
- **Compressed log files**: smaller pile, same pile; still files beside the
  database, still unbounded.
- **Full output in SQLite**: one file, but the database grows with the
  output; same problem in a different place.
- **No output at all**: failures outside tests (build errors, opaque runs)
  would have no explanation.

## Sources

1. nextest JUnit support: https://nexte.st/book/junit
2. Bazel test outputs: https://leimao.github.io/blog/Bazel-Test-Outputs
3. SQLite, 35% faster than the filesystem: https://www.sqlite.org/fasterthanfs.html
4. SQLite, internal versus external BLOBs: https://sqlite.org/intern-v-extern-blob.html
