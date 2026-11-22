# 0011: Formalization: Quint spec, Lean deferred

Status: Proposed. **Deviates from the README**, which names Lean as a candidate to research.

## Context

The goal is a formal contract for the engine, so that skills, adapters and any test framework align with one definition. What needs formalising:

- lifecycle legality: run and experiment state machines and their invariants ([0002](0002-formal-model.md), [0007](0007-experiments.md));
- data contracts: events, run records, protocol;
- the statistical verdict procedure.

Lean 4 findings ([03](../research/03-formalization-experiments-storage.md)): the proven way to combine Lean with Rust is AWS Cedar's: an executable Lean model, differentially fuzzed against the Rust code, test-only. It works and it is expensive: a second language and toolchain for every contributor and agent, and non-trivial build integration. toe's likely defects are in process handling, IO and statistics usage, not in theorems about a small state machine.

Quint is a typed, programmer-readable specification language with TLA+ semantics, a model checker, and a Rust bridge (`quint-connect`) that replays spec traces against an implementation. The bridge is young.

## Decision

1. **Lifecycles are specified in Quint**, in `spec/`, with the invariants listed in 0002 and 0007. The spec is model-checked in CI.
2. **The Rust engine is tested against the spec** by replaying generated traces. If `quint-connect` proves too immature, the same traces are checked with `proptest` state-machine tests against a small reference model in Rust.
3. **Data contracts are Rust types** with `serde`, exported as JSON Schema (`schemars`), and enforced again by STRICT tables and CHECK constraints in the store.
4. **Statistics are verified by reference values**: test vectors computed with R or SciPy, plus property tests.
5. **Lean is not used in v1.** It is reconsidered if the verdict procedure needs machine-checked guarantees, following the Cedar pattern (executable model, differential fuzzing, no mathlib, never a runtime dependency).

## Consequences

- The formal artifact is small, readable, and executable in CI within seconds.
- The spec doubles as documentation for skills: the legal transitions are the legal agent actions.
- No proofs, only model checking within bounds and trace conformance.

## Alternatives

- Lean 4 now: strongest guarantees, highest cost, aimed at a risk toe barely has.
- TLA+ or Alloy: mature, no Rust trace bridge.
- Kani or Verus on the Rust code: useful later for small arithmetic kernels.
- No formal spec: invariants drift between code, docs and skills.

## Worked examples

The Quint below is illustrative: it has not been compiled yet, and the exact `quint-connect` API is unchecked, so trace replay is shown as a plain Rust test. The examples show the mechanism, which has three layers: the **spec** defines what is legal, the **model checker** searches for illegal sequences, and **trace replay** shows that the Rust engine behaves like the spec.

### Example 1: cancel races with a finishing test

Scenario: the agent sees the first failure and calls `toe cancel`. At the same moment the supervisor is still writing results. What ends up in the store?

Spec, `spec/run.qnt`, with a deliberate hole:

```quint
module run {
  type Status = Created | Running | Finished | Cancelled | Aborted
  type Event  = TestStarted(str) | TestFinished(str) | RunEnded

  var status: Status
  var log: List[Event]
  var open: Set[str]          // tests started, not finished

  const TESTS: Set[str]

  action init = all { status' = Created, log' = [], open' = Set() }

  action start = all {
    status == Created,
    status' = Running, log' = log, open' = open,
  }

  action testStarted(t) = all {
    status == Running, not(open.contains(t)),
    log' = log.append(TestStarted(t)), open' = open.union(Set(t)), status' = status,
  }

  action testFinished(t) = all {
    open.contains(t),                       // hole: no check on status
    log' = log.append(TestFinished(t)), open' = open.exclude(Set(t)), status' = status,
  }

  action cancel = all {
    status == Running,
    status' = Cancelled, log' = log.append(RunEnded), open' = open,
  }

  action step = any {
    start, cancel,
    nondet t = oneOf(TESTS) any { testStarted(t), testFinished(t) },
  }

  // Invariant: nothing is recorded after the run ended
  val nothingAfterEnd =
    log.length() == 0 or
    log.indices().forall(i => log[i] == RunEnded implies i == log.length() - 1)
}
```

Check:

```
quint run spec/run.qnt --invariant=nothingAfterEnd
```

The checker answers with the shortest path to a violation:

```
init -> start -> testStarted("a") -> cancel -> testFinished("a")
log = [TestStarted("a"), RunEnded, TestFinished("a")]     violates nothingAfterEnd
```

Meaning: a cancelled run later gains a passed test. History would say "run cancelled, test a passed", and flake statistics would count a result from a killed process. The hole is found before any Rust exists.

The fix is a design decision, recorded in the spec: `testFinished` requires `status == Running`. Results arriving after a terminal state are dropped and counted, never stored as outcomes.

### The same trace, replayed against Rust

The counterexample becomes a permanent regression test. The checker also generates many random legal traces, and each is replayed the same way.

```rust
#[test]
fn late_result_after_cancel_is_rejected() {
    let mut engine = Engine::in_memory();
    let run = engine.create_run(suite());
    engine.start(run).unwrap();
    engine.record(run, Event::TestStarted("a".into())).unwrap();
    engine.cancel(run).unwrap();

    let late = engine.record(run, Event::TestFinished("a".into(), Outcome::Passed));

    assert_eq!(late, Err(EngineError::RunTerminal));
    assert_eq!(engine.status(run), Status::Cancelled);
    assert_eq!(engine.log(run).last(), Some(&Event::RunEnded));
}
```

After every step, replay compares the engine state with the spec state. A divergence is a bug in the Rust code or in the spec.

### Example 2: the agent peeks and moves the goalposts

Scenario: the agent is optimizing a parser. The protocol says 10 trials per arm and a minimum effect of 2%. After 5 baseline and 3 candidate trials the numbers look good, so the agent asks for a verdict early. When that is refused, it tries to lower the minimum effect to 0.5%.

Invariants in `spec/experiment.qnt`:

```quint
val verdictNeedsEvidence =
  verdict != None implies
    (trials(Baseline) >= protocol.minTrials and trials(Candidate) >= protocol.minTrials)

val protocolFrozen =
  totalTrials > 0 implies protocolHash == hashAtFirstTrial
```

What the agent sees at runtime, because the engine enforces the same rules:

```
$ toe exp verdict parser-speed
error: verdict refused: candidate has 3 of 10 required trials
next:  toe exp trial parser-speed --arm candidate

$ toe exp protocol set parser-speed --min-effect 0.5%
error: protocol frozen at first trial (hash 9f3c...)
next:  toe exp new --from parser-speed
```

### Where each rule lives

| Layer | Role | Catches |
|---|---|---|
| Quint spec | definition of legal | design holes, before code |
| Model checker, in CI | explores interleavings | races nobody thought to test |
| Trace replay | spec versus Rust | engine drifting from the spec |
| Engine guards plus SQLite `CHECK` | runtime enforcement | an agent or a bug attempting an illegal step |
| Skills | tell the agent the legal moves | wasted attempts |

The error messages the agent reads are the spec invariants in plain words.

### What this does not give

- No proof for unbounded cases. The checker explores a bounded space (for example 3 tests, 20 steps).
- Nothing about the correctness of the statistics. Reference values from R or SciPy cover that.
- Nothing about process handling (signals, kills). Integration tests cover that.
