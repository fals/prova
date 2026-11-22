import Primes

/-- Prints `n countPrimes(n)` for every `n` below the limit (default 3000). -/
def main (args : List String) : IO Unit := do
  let limit := (args.head? >>= String.toNat?).getD 3000
  -- One pass, using countPrimes_succ, so the table costs one primality test per n.
  let mut count := 0
  for n in [0:limit] do
    IO.println s!"{n} {count}"
    if isPrime n then count := count + 1
