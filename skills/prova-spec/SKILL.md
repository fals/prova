---
name: prova-spec
description: Formalize a problem before implementing it, then let the specification judge every implementation. Use when asked to build or improve an algorithm, protocol, allocator, scheduler, parser, network plugin (CNI), search or solver, or anything whose correctness can be stated; when an experiment targets accuracy, step counts or correctness rather than speed; or when "it works" must be proven rather than claimed.
---

# Specification first

You are about to write code that solves a problem. If you write the code, the
tests and the success criterion in one go, you can always declare success.
Do it in the scientific order instead:

1. **State the problem formally** (this skill).
2. **Check the statement** with the model checker before any code exists.
3. **Implement.** The specification does not change while you do.
4. **Judge** every implementation with the specification (the oracle) and
   compare correct ones with `prova exp` (skill `prova-experiment`).

The specification is the contract between you, the human, and prova. Show it to
the human before implementing: it is short, and it is where misunderstandings
are cheap.

## 1. Write the specification

Use Quint unless the project already uses TLA+, Alloy or Lean. Quint needs
Node: `npm i -g @informalsystems/quint`. Put specs in `spec/` (or wherever the
project keeps them).

A specification answers four questions. Write them as comments first, then
as Quint:

| Question | Quint | Example (CNI address management) |
|---|---|---|
| What is the state? | `var` | `assigned: str -> int` (pod to address) |
| What can happen? | `action`, combined in `step` | `add(pod)`, `del(pod)` |
| What must always hold? | `val` invariants, combined in `safety` | no two pods share an address; only pool addresses |
| What is the right answer? | `pure def` reference functions | `dist(src)` for shortest paths |

Rules that keep it useful:

- **Small constants.** 3 to 5 pods, 6 nodes, 3 addresses. Bugs show up in
  small worlds; the checker explores them thoroughly.
- **Nondeterminism for what the implementation may choose** (`nondet ip =
  free.oneOf()`): any free address is fine, a used one is not. Do not specify
  the algorithm; specify the allowed outcomes.
- **Errors are behaviour.** Pool exhausted, unknown pod, duplicate request:
  say what happens (state unchanged, error returned).
- **The objective is not in the spec.** Speed, accuracy, steps are measured
  by the experiment. The spec says which answers are acceptable.
- One `run ...Test` per scenario the human cares about, with `assert`.

Worked examples in this skill, both model-checked:

- `examples/ipam.qnt`: CNI address allocation, a state machine with
  invariants (`unique`, `inPool`, `noLeak`).
- `examples/shortest.qnt`: shortest paths, a reference model (`dist`,
  `correct(src, dst, path)`): any algorithm must give these answers.

For accuracy on data (classification, ranking, extraction) the specification
is the frozen labelled dataset plus the output contract (every input gets
exactly one label from the label set). Register the dataset file as a spec
(step 4); the metric is the accuracy the workload prints.

### Lean: an executable definition plus theorems

Use Lean when a property needs a proof, not just bounded checking. The
pattern (as in AWS Cedar): write the obvious, slow definition, prove the
properties you rely on, and run the definition as the reference.

```lean
/-- The number of primes strictly below `n`. Obviously right, not fast. -/
def countPrimes (n : Nat) : Nat := ((List.range n).filter isPrime).length

/-- Adding one to `n` adds one exactly when `n` is prime. -/
theorem countPrimes_succ (n : Nat) :
    countPrimes (n + 1) = countPrimes n + (if isPrime n then 1 else 0) := by
  unfold countPrimes
  rw [List.range_succ, List.filter_append]
  cases h : isPrime n <;> simp [List.filter, h]

theorem countPrimes_excludes_n : countPrimes 7 = 3 ∧ countPrimes 8 = 4 := by decide
```

`lake build` fails if any theorem is false; a `lean_exe` prints the
definition's answers for a range of inputs, and a small program in the
project's language compares the implementation with them. Core Lean is
enough for this; mathlib is large and rarely needed. Pin the toolchain in
`lean-toolchain`.

## 2. Check the specification

```sh
quint typecheck spec/ipam.qnt
quint test spec/ipam.qnt                                   # your scenarios
quint run spec/ipam.qnt --invariant safety --max-steps 20  # random exploration
quint verify spec/ipam.qnt --invariant safety              # exhaustive, needs Java (Apalache)
lake build                                                 # Lean: every theorem is checked
```

Then **break it on purpose**: change one action so it is wrong (hand out any
address instead of a free one) and confirm the checker reports a violation
with a short counterexample. A spec that cannot fail checks nothing. Revert.

Commit the specification before implementing. It is now the goal.

## 3. Turn the specification into an oracle

The oracle is one command that succeeds only if an implementation satisfies
the specification. Pick the strongest you can afford:

1. **Trace conformance** (state machines). Generate traces from the spec and
   replay them against the implementation:
   ```sh
   quint run spec/ipam.qnt --mbt --max-samples 50 --max-steps 20 \
     --out-itf 'traces/ipam_{seq}.itf.json'
   ```
   A small driver in the project's language reads each trace, performs the
   action named in `mbt::actionTaken` with the values in `mbt::nondetPicks`,
   and compares the implementation's state with the trace's state after every
   step. See `references/conformance.md` for the trace format and drivers.
2. **Reference answers** (functions). Get a table of the spec's answers
   and check the implementation returns acceptable ones for every input,
   including edge cases. To get the table out of Quint, step through the
   inputs in a tiny state machine and write it as a trace:

   ```quint
   var n: int
   var answer: int
   action init = all { n' = 0, answer' = countPrimes(0) }
   action step = all { n' = n + 1, answer' = countPrimes(n + 1) }
   ```

   ```sh
   quint run spec/table.qnt --max-steps 3000 --max-samples 1 --n-traces 1 \
     --mbt --out-itf 'reference_{seq}.itf.json'
   ```

   Each state of `reference_0.itf.json` holds one `(n, answer)` pair
   (integers as `{"#bigint": "4"}`); the project's test compares them.
3. **Property tests** derived from the invariants (proptest, Hypothesis,
   fast-check, jqwik, FsCheck, Go's `testing/quick`).
4. **Proofs** (Lean): `lake build` is the oracle.

Wrap it as one command, e.g. `make conformance`, and add a prova suite so
`prova run conformance` works in the test loop:

```toml
[suite.conformance]
run = "make conformance"
```

## 4. Implement, then judge

First correct implementation: run the oracle through prova until it passes.

```sh
prova run conformance --detach && prova wait <run>
```

Improving a correct implementation (faster, fewer steps, more accurate):
register the experiment **with the specification and the oracle**. prova
freezes the spec files by hash, runs the oracle on both arms before any
trial, rejects a violating candidate without measuring it, and never accepts
one.

```sh
prova exp new ipam-fast --hypothesis "a free-list makes ADD O(1) without collisions" \
  --spec spec/ipam.qnt --oracle "make conformance" \
  --metric add_ns --better lower --min-effect 10% -- ./bench-add.sh
# change the code
prova exp candidate ipam-fast && prova exp run ipam-fast
```

Verdicts that mention the specification:

- `REJECT ... candidate violates the specification`: the oracle failed on
  the candidate (log path in the reason). Fix correctness first; speed is
  irrelevant.
- `... differs from the registered specification`: the candidate edited the
  spec. Never weaken a spec to make a candidate pass. If the spec was wrong,
  say so to the human, fix it, commit, and start a new experiment.
- Failed comparison, `baseline violates the specification`: the committed
  code does not satisfy the spec. Make it correct first.

## Report

State the specification file, what it requires in one sentence, the oracle,
and the prova run or experiment id. "Satisfies the spec" without an oracle run
id is a claim, not evidence.
