# Benchmark experiments

Benchmark command (Go's own `testing.B`; `make test` does not run benchmarks):

    go test -run '^$' -bench CountPrimes -count 3 .

toe metric: `BenchmarkCountPrimes.ns_per_op` (lower is better; median of the
three counts). Also available: `.bytes_per_op`, `.allocs_per_op`.

Overlays replace `primes.go` (copy the file and strip the `.txt` extension):

- `faster/primes.go.txt`: sieve of Eratosthenes, same results, about 20x faster.
- `same/primes.go.txt`: the baseline code with only a comment changed.
- `slower/primes.go.txt`: trial division by every divisor from 2, about 2x slower.
