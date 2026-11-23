# 0015: Spec-first experiments: the specification is the oracle

Status: Accepted (2026-11-22). Amends [0007](0007-experiments.md) (protocol, decision rule) and [0011](0011-formalization-quint-not-lean.md) (Lean is usable by projects).

## Context

Experiments in 0007 answer one kind of question: is the candidate faster? Agents
are asked many other questions about a change:

- does a new algorithm give **better accuracy** on a fixed dataset?
- does it reach the goal in **fewer steps** (graph search, solver iterations, retries)?
- does a change to a **CNI** keep it correct (no two pods share an address,
  every allocated address is released) while cutting allocation latency?

Two things are missing.

1. **Objectives other than time.** prova already reads any metric a workload
   prints, but deterministic metrics (accuracy on a fixed set, step counts)
   broke the rank test: with zero variance it divided by zero. Native
   benchmark tools (`go test -bench`, criterion, pytest-benchmark, Vitest,
   JMH, BenchmarkDotNet) print results prova did not read.
2. **Correctness defined before the work starts.** An agent that writes the
   code, the tests and the success criterion in one sitting can always
   declare success. Pre-registration (0007) freezes the metric and the
   threshold; nothing froze *what a correct solution is*. A faster or more
   accurate candidate that breaks an invariant must never be accepted.

The scientific order is: state the problem formally, check that the statement
is consistent, then implement, then measure implementations that satisfy the
statement against each other.

## Decision

1. **Formalize first.** Before implementing, the problem is written as a
   specification: state, allowed transitions, invariants and properties, and
   the objective metric. Quint is recommended (model checker, random
   simulation, ITF traces for conformance). Lean is accepted when a property
   needs a proof rather than bounded checking. Any checker works: for prova a
   specification is a set of files plus a command that decides.
2. **The specification is registered with the protocol.** `prova exp new
   --spec <file>` (repeatable) stores each file's path and sha256 in the
   protocol, so the protocol hash covers the specification. The file must be
   committed; a variant whose copy differs is a violation. Changing the
   specification means a new experiment.
3. **The oracle gates measurement.** `prova exp new --oracle "<command>"`
   runs once in each variant's directory before any trial:
   - baseline violates it: the comparison fails without a verdict (only
     implementations that satisfy the specification are compared);
   - candidate violates it: verdict `reject`, no trial runs, and it can never
     be accepted.
   This is part of the lifecycle and is specified in `spec/experiment.qnt`
   (`checkSpec`, `rejectViolation`, invariants `trialsNeedTheSpec`,
   `violatorNeverAccepted`, `violatingBaselineDecidesNothing`), model-checked
   and replayed against the engine.
4. **Conformance is the usual oracle.** The specification generates traces
   (`quint run --mbt --out-itf`); a small driver in the project's language
   replays each trace against the implementation and fails on the first
   divergence. Model checking the spec, property tests derived from its
   invariants, and `lake build` for Lean proofs fit the same slot.
5. **Any objective.** The metric is whatever the workload prints as a JSON
   metric line or a native benchmark tool reports (`prova exp metrics -- <cmd>`
   lists them). Deterministic metrics are valid: identical samples give
   p = 1; constant but different arms are decided by the rank test with the
   usual minimum of five trials per arm. `--better higher` for accuracy.
6. **Skills carry the method.** `prova-spec` teaches formalizing a problem,
   checking the spec, writing the driver and registering the experiment;
   `prova-experiment` covers objectives, benchmark tools and the gate.

prova does not parse, generate or interpret specifications. It freezes them,
runs the oracle, and refuses to measure what does not satisfy them.

## Consequences

- "Correct" is fixed before code exists; an agent cannot move it after seeing
  results without starting a new, visible experiment.
- Speed, accuracy and step counts are judged by one procedure, and only among
  correct implementations.
- The first implementation of a problem has no correct baseline to compare
  against: it is checked with `prova run` on the oracle (or a suite running
  it), and experiments start from the first correct version.
- Projects need the checker's toolchain (Quint needs Node; Lean needs elan).
  prova needs neither.

## Alternatives

- Correctness as a guardrail metric (`violations = 0`): measured, so a
  violating candidate costs full trials and the rank test could still
  call a tiny violation rate "no effect".
- prova generating specifications: out of scope; the agent writes them with
  the skill, the human reviews them.
- Embedding a Quint or Lean runtime: large footprint, one checker forever.
