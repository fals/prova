# Primes benchmark experiments (C# / BenchmarkDotNet)

Workload: `Calc.Primes.CountPrimes(500000)` counts primes below n by trial
division (2 handled separately, then odd divisors only); the result must be
41538 (the benchmark throws otherwise).

## Benchmark command

```sh
dotnet run -c Release --project bench -- --filter '*'
```

`bench/Bench.csproj` (BenchmarkDotNet 0.14.0) compiles `../Primes.cs`. The job
is in-process (`InProcessEmitToolchain`), 1 launch, 1 warmup, 3 iterations of
~200 ms, summary unit fixed to milliseconds. `Calc.Tests.csproj` excludes
`bench/**` and `experiments/**`, so `dotnet test` is unaffected. About 2-3 s
per run once NuGet packages are restored.

## Metric

`PrimesBench.CountPrimes` mean time per operation in **ms** (lower is
better), from the summary table:

```
| Method      | Mean     | Error    | StdDev   |
|------------ |---------:|---------:|---------:|
| CountPrimes | 15.52 ms | 0.240 ms | 0.013 ms |
```

## Overlays

Each overlay replaces `Primes.cs`; copy the file and strip the `.txt`
extension:

| overlay  | change                                             | expected (approx.) |
|----------|----------------------------------------------------|--------------------|
| `faster` | sieve of Eratosthenes                              | ~0.86 ms           |
| `same`   | baseline with only a comment changed               | ~15.5 ms           |
| `slower` | trial division by every d >= 2 (no even skipping)  | ~30.5 ms           |

Baseline is ~15.5 ms.
