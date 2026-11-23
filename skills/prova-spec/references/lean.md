# Lean: an executable definition plus theorems

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
