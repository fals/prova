# Benchmark experiments

Benchmark command (run from the fixture root; `make test` does not collect `benchmarks/`):

    python3 -m pytest benchmarks -q -p no:cacheprovider --benchmark-only --benchmark-min-rounds=5 --benchmark-columns=min,max,mean,median,stddev,rounds

Metric: pytest-benchmark wall time of `test_count_primes` (`count_primes(50000) == 5133`);
compare the Mean/Median columns (lower is better).

Overlays replace `primes.py` (copy the file and strip the `.txt` extension):

- `faster/primes.py.txt`: sieve of Eratosthenes, same results, much faster.
- `same/primes.py.txt`: the baseline code with only a comment changed.
- `slower/primes.py.txt`: trial division by every divisor from 2 (no even skipping), about 2x slower.
