# Benchmark experiments

Benchmark command (criterion; `make test` does not run benchmarks):

    cargo bench --bench primes -- --warm-up-time 1 --measurement-time 2 --noplot

prova metric: `count_primes.time_ns`, criterion's point estimate normalized to
nanoseconds (lower is better).

Overlays replace `src/primes.rs` (copy the file and strip the `.txt` extension):

- `faster/src/primes.rs.txt`: sieve of Eratosthenes, same results, about 25x faster.
- `same/src/primes.rs.txt`: the baseline code with only a comment changed.
- `slower/src/primes.rs.txt`: trial division by every divisor from 2, about 2x slower.
