# 0025: Test sizes come from the resources a test uses, never from its duration

Status: Proposed (2026-11-23). Amends [0005](0005-test-sizes.md) and [0013](0013-black-box-adoption.md) §4.

## Context

[0005](0005-test-sizes.md) defines sizes by what a test may touch. [0013](0013-black-box-adoption.md) §4 says a black box cannot declare sizes, so prova observes and proposes one. In practice the proposal uses duration alone: `analysis::proposed_size` maps a test's p95 to small (up to 1 s), medium (up to 30 s) or large. `prova sizes`, `prova history` and the size distribution warning all rest on it.

Duration is the wrong measure:

- A pure computation that takes 3 s is called medium, though it needs nothing but its own process.
- A test that starts a database on localhost and finishes in 400 ms is called small, though it carries every risk sizes exist to expose: other processes, ports, startup order, shared state.
- Duration depends on the machine and its load. The same test changes size between a laptop and CI, and between a quiet and a busy run.

What makes a test fast and deterministic is what it touches. Size must say that.

## Decision

1. **A size is the set of resources a test used:**

   | | Processes | Network |
   |---|---|---|
   | small | one: the test's own | none |
   | medium | more than one, on this machine | this machine only (loopback, Unix sockets) |
   | large | any | another host |

   The largest resource seen decides. A test that started one child process is medium. A test that opened one connection to a non-loopback address is large, whatever else it did.

2. **Duration never decides a size.** It keeps its other jobs: slow budgets and kill ceilings per size ([0004](0004-liveness-and-fail-fast.md), [0005](0005-test-sizes.md) table), and duration history.

3. **What prova observes**, from the run's process group on every sample:
   - **Processes:** members of the group, as today.
   - **Network:** on Linux, the sockets the members hold (`/proc/<pid>/fd` matched to `/proc/net/{tcp,tcp6,udp,udp6}`), each classified as loopback or another host. On macOS, `lsof -a -p <pids> -i`, if present.
   - **Per run:** these observations always produce the run's size.

4. **Per test, a size is given only when it can be attributed.** That is when the test ran in its own process (nextest, or a framework that forks per test), or when it was the only test running between its start and finish events. A test never observed alone has no size of its own. It shows the run's size as an upper bound ("medium or less"), never a guess.

5. **Observations are lower bounds.** Sampling misses a child process or a connection that lives between two samples. prova reports only what it saw. A test is called small only after it has been attributed in at least 3 runs and nothing more was ever seen, and the reason is shown ("one process, no sockets, in 5 runs").

6. **Declared sizes are checked, not trusted.** A size declared in `prova.toml`, or in the framework's tags where the adapter reads them, stays an override for budgets. A test declared small that is seen using a second process or a socket is reported as a size violation, naming the resource ("declared small, started `postgres`").

7. **Where prova cannot see** (opaque level with no per-test events, an OS without the counters, sockets in another network namespace), sizes are reported as unknown, with the reason.

## Consequences

- `prova sizes` counts small, medium, large and unknown by resources, and lists the resource behind each medium or large test instead of the slowest tests. The 80/15/5 warning counts attributed tests only.
- `prova history` shows the observed size with its reason, or an upper bound.
- Network observation costs a scan of the group members' file descriptors on each sample. It runs only while the run is alive, at the existing sampling rate.
- Suites that run tests in parallel within one process (pytest-xdist workers, Go's `t.Parallel`, a Vitest worker pool) get per-test sizes only for tests seen alone. Many tests there will show only an upper bound, and prova says so.
- Skills (prova-test, the language skills) and AGENTS.md describe sizes by resources only.

## Open question

0005 also forbids disk access and sleep in small tests. This record defines size by processes and network only, as the owner stated. Disk and sleep would then no longer decide a size. If they should, the table gains a column; that needs file observation per test, which /proc does not give cheaply. To be settled before acceptance.

## Alternatives

- **Keep duration as a fallback when resources cannot be attributed:** it reintroduces machine-dependent sizes, and an upper bound is more honest.
- **Enforce sizes with namespaces (no network for small tests):** this would turn observations into guarantees. It is Linux-only and changes how the user's tests run, against ground rule 3. Possible later as an opt-in.
- **eBPF or audit for exact per-process attribution:** needs privileges prova does not assume ([0001](0001-architecture-overview.md)).
