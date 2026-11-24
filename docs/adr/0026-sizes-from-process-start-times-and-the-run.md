# 0026: Sizes from process start times and from the whole run

Status: Proposed (2026-11-24). Amends [0025](0025-sizes-by-resources.md) §3–§5.

## Context

Usability round 12 showed that 0025's sizes can claim **small** for tests that
used more, and leave most fast tests **unknown**:

1. A test's "before" snapshot was taken when prova *read* its start line. A
   test that starts a child at once already has it in that snapshot, so the
   child is never counted: `sleep 0.6` was small, `sleep 0.3; sleep 0.3`
   medium (only the second child was new).
2. "One process, no sockets" was printed as if observed. It meant that none
   of a few 0.5 s samples caught anything; for a 47 ms test there were none.
   Tests that connected to another host were recorded small, and a suite
   declared small got no note, while the same run's `run_size` event had
   seen the connection.
3. A test too short to be sampled stayed unknown, so on a typical suite
   almost every test was unknown, which helps nobody.

Duration is still not evidence: a 5 ms test can start a process or talk to
a server on `localhost`.

## Decision

1. **Processes are attributed by their start time** where the OS records it
   (Linux: `/proc/<pid>/stat` field 22, in clock ticks since boot). A process
   started after the test's start line arrived and before its finish line
   arrived, less one tick at each end, is the test's, whenever a sample sees
   it, even if it was there before the start line was read. Lines carry the
   time their bytes arrived, taken by the reader before any processing.
   Where no start time is available (macOS), 0025's first-seen rule stays.
2. **The run's test phase decides for tests prova cannot isolate.** From the
   first test event to the last, prova records what the run's tree used
   beyond the runner: processes that were there before the phase began
   (the runner, compilation) and processes that run one test (`--exact`)
   do not count; their children do; any socket counts. A test that was not
   attributed gets:
   - **small**, if the phase showed nothing beyond the runner: "no other
     process or socket seen in its run";
   - **medium or less** / **large or less**, naming what the phase used:
     "the run started postgres", "the run connected to 10.0.3.7:5432".
3. **Small is the absence of evidence and says so.** Evidence reads "no
   other process or socket seen (N samples)" or "… in its run", never "one
   process, no sockets". Small still needs 3 runs (attributed or by the
   run rule) with nothing more; medium and large are decided by one.
4. **Unknown means prova could not observe**: no sample of the run's test
   phase (a run shorter than one sample with no test events), or a platform
   that gives no process list. The reason is printed.
5. **A declared size is checked against the run too.** A suite declared
   `small` whose test phase used another process or a socket gets a run
   note naming it, even when no single test can be blamed; a suite declared
   `medium` whose run reached another host likewise.
6. Only runs that finished count as size evidence: cancelled and aborted
   runs, and skipped attempts, do not.

## Consequences

- Most tests of a clean suite become small after three runs, with honest
  evidence; tests in a run that used more are bounded by what it used.
- A process that starts and exits between two samples is still missed in
  that run (a lower bound); start times make it count whenever any sample
  sees it alive inside the window.
- The reader thread stamps each line with its arrival time: one `Instant`
  per line.

## Alternatives

- **Duration as a proxy for small:** rejected again (0025); a fast test can
  still start a process or open a socket.
- **Count forks exactly (cgroups, eBPF, ptrace):** needs privileges or
  changes how tests run; not assumed (ADR 0001, black box first).
