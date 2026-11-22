/-!
# Specification: counting primes

What `CountPrimes(n)` must return, written before any implementation and
independent of how it is computed. Lean checks the theorems when the spec is
built (`lake build`); `lake exe reference` prints the specified answers,
which the oracle compares with the Go implementation for every `n` in a
range (differential testing against an executable specification).

The definitions are deliberately naive: they are obviously right, not fast.
-/

/-- `k` is prime: at least 2 and no divisor strictly between 1 and `k`. -/
def isPrime (k : Nat) : Bool :=
  2 ≤ k && (List.range k).all (fun d => d < 2 || k % d != 0)

/-- The number of primes strictly below `n`. -/
def countPrimes (n : Nat) : Nat :=
  ((List.range n).filter isPrime).length

/-- Adding one to `n` adds one exactly when `n` itself is prime. -/
theorem countPrimes_succ (n : Nat) :
    countPrimes (n + 1) = countPrimes n + (if isPrime n then 1 else 0) := by
  unfold countPrimes
  rw [List.range_succ, List.filter_append]
  cases h : isPrime n <;> simp [List.filter, h]

/-- The count never decreases. -/
theorem countPrimes_mono {n m : Nat} (h : n ≤ m) : countPrimes n ≤ countPrimes m := by
  induction h with
  | refl => exact Nat.le_refl _
  | step _ ih =>
    rw [countPrimes_succ]
    exact Nat.le_trans ih (Nat.le_add_right _ _)

/-- The count grows by at most one per step. -/
theorem countPrimes_succ_le (n : Nat) : countPrimes (n + 1) ≤ countPrimes n + 1 := by
  rw [countPrimes_succ]
  split <;> simp

/-- "Below": `n` itself is never counted, so a prime `n` is excluded. -/
theorem countPrimes_excludes_n : countPrimes 7 = 3 ∧ countPrimes 8 = 4 := by decide

/-- 0 and 1 are not prime. -/
theorem small : countPrimes 0 = 0 ∧ countPrimes 1 = 0 ∧ countPrimes 2 = 0 ∧ countPrimes 3 = 1 := by
  decide

/-- Known values (checked by evaluation in the compiled kernel extension). -/
theorem known : countPrimes 100 = 25 ∧ countPrimes 1000 = 168 := by native_decide
