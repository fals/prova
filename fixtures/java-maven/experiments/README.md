# Primes benchmark experiments (Java / JMH)

Workload: `calc.Primes.countPrimes(500000)` counts primes below n by trial
division (2 handled separately, then odd divisors only); the result must be
41538 (the benchmark throws otherwise).

## Benchmark command

```sh
mvn -q -B -Pbench package -DskipTests && java -jar target/benchmarks.jar -f 1 -wi 1 -w 200ms -i 3 -r 200ms -bm avgt -tu ms
```

(`./bench.sh` wraps the same thing.) The `bench` Maven profile adds JMH 1.37,
compiles `src/jmh/java` via build-helper and shades `target/benchmarks.jar`.
It is inactive by default, so `mvn -q test` is unaffected. About 3 s per run
once the Maven repository is warm.

## Metric

`calc.PrimesBench.countPrimes`, average time per operation in **ms/op**
(lower is better). Final table line:

```
PrimesBench.countPrimes  avgt    3  15.538 ± 1.674  ms/op
```

## Overlays

Each overlay replaces `src/main/java/calc/Primes.java`; copy the file and
strip the `.txt` extension:

| overlay  | change                                             | expected (approx.) |
|----------|----------------------------------------------------|--------------------|
| `faster` | sieve of Eratosthenes                              | ~1.2 ms/op         |
| `same`   | baseline with only a comment changed               | ~15.5 ms/op        |
| `slower` | trial division by every d >= 2 (no even skipping)  | ~31 ms/op          |

Baseline is ~15.5 ms/op.
