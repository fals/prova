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
