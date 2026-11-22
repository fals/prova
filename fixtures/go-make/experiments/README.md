# Benchmark experiments

Benchmark command (Go's own `testing.B`; `make test` does not run benchmarks):

    go test -run '^$' -bench CountPrimes -count 3 .

toe metric: `BenchmarkCountPrimes.ns_per_op` (lower is better; median of the
three counts). Also available: `.bytes_per_op`, `.allocs_per_op`.

Overlays replace `primes.go` (copy the file and strip the `.txt` extension):

- `faster/primes.go.txt`: sieve of Eratosthenes, same results, about 20x faster.
- `same/primes.go.txt`: the baseline code with only a comment changed.
- `slower/primes.go.txt`: trial division by every divisor from 2, about 2x slower.

## Spec-gated experiment (Lean)

`spec/Primes.lean` states what `CountPrimes(n)` must return, independently
of how: a naive executable definition plus theorems Lean checks at build
time (the count grows by one exactly at primes, never decreases, excludes
`n` itself, known values). `spec/oracle.sh` is the oracle: `lake build`
(the theorems hold), then `lake exe reference 3000` prints the specified
answer for every n below 3000 and `go run ./cmd/conform` compares
`CountPrimes` with each.

    toe exp new primes --spec spec/Primes.lean --oracle "sh spec/oracle.sh" \
      --metric BenchmarkCountPrimes.ns_per_op --min-effect 5% -- \
      go test -run '^$' -bench CountPrimes -count 3 .

- `faster/primes.go.txt`: correct sieve, accepted.
- `wrong/primes.go.txt`: a sieve that counts primes up to and including n.
  Faster still, and wrong whenever n is prime: rejected by the oracle
  without a single trial (`CountPrimes(3) = 2, specification says 1`).

Needs Lean 4 (`elan`) and Go; the `toe-fixture-lean` image has both.
