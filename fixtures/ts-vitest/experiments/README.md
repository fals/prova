# Benchmark experiments

Benchmark command (run from the fixture root after `npm install`; `npm test` / `vitest run` does not run benches):

    npx vitest bench --run

Metric: Vitest bench timing of `primes > countPrimes` (`countPrimes(500000)`, checked to equal 41538);
compare the `mean` column in ms (lower is better) or `hz` (higher is better).

Overlays replace `src/primes.ts` (copy the file and strip the `.txt` extension):

- `faster/src/primes.ts.txt`: sieve of Eratosthenes, same results, much faster.
- `same/src/primes.ts.txt`: the baseline code with only a comment changed.
- `slower/src/primes.ts.txt`: trial division by every divisor from 2 (no even skipping), about 2x slower.
