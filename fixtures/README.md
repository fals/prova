# Brownfield fixtures

One small project per language prova claims, each run the way real projects
run their tests, with no prova configuration. Used by the large tests
(`prova run large`) and by usability checks (a fresh agent adopts one with
only the skills).

| Fixture | Language, framework | Own command | Where it runs in large tests |
|---|---|---|---|
| `rust-make` | Rust, libtest | `make test` (`cargo test --quiet`) | host |
| `go-make` | Go, testing | `make test` (`go test ./...`) | host |
| `python-make` | Python, pytest | `make test` (`pytest -q`) | `python:3.13` |
| `ts-vitest` | TypeScript, Vitest | `npm test` | `node:22` |
| `java-maven` | Java, JUnit 5 + Surefire | `mvn -q test` | `maven:3.9-eclipse-temurin-21` |
| `csharp-dotnet` | C#, xUnit + VSTest | `dotnet test` | `mcr.microsoft.com/dotnet/sdk:9.0` |

Every fixture has the same four tests, and the same benchmark workload for
experiments (see below):

- a real bug: `div` multiplies, so the division test fails until fixed;
- a flaky test: fails on roughly one run in three (clock based);
- a slow test: about 1.5 s;
- a hanging test: hangs only when `HANG=1` is set.

## Experiments

Each fixture also counts the primes below N by trial division and benchmarks
it with the language's own benchmark tool. `experiments/` holds three prepared
changes to the primes module (stored as `.txt` so no tool picks them up):
`faster` (a sieve), `same` (a comment only) and `slower` (no even skipping).
Each fixture's `experiments/README.md` gives the command and the metric.

| Fixture | Benchmark tool | prova metric |
|---|---|---|
| `rust-make` | criterion | `count_primes.time_ns` |
| `go-make` | `go test -bench` | `BenchmarkCountPrimes.ns_per_op` |
| `python-make` | pytest-benchmark | `test_count_primes.median_ns` |
| `ts-vitest` | Vitest bench | `countPrimes.mean_ns` |
| `java-maven` | JMH | `PrimesBench.countPrimes.ns_per_op` |
| `csharp-dotnet` | BenchmarkDotNet | `CountPrimes.mean_ns` |

`go-make` also carries a Lean specification of the workload
(`spec/Primes.lean`: an executable definition plus checked theorems) and a
fourth change, `wrong`: a faster sieve that miscounts. With `--spec` and
`--oracle`, prova accepts the correct sieve and rejects the wrong one before
measuring it (`faster=accept wrong=reject(spec)`, image
`docker/lean.Dockerfile`).

`exp-check.sh` registers the experiment on the committed baseline and runs
the three changes against it. The large tests assert, for each language,
that prova finds the bug per test (not just "the run failed") with an
explanation, that a hang is reported as a stall with no configuration, and
that the experiments end `faster=accept same=reject slower=reject`, with
the metric read from the native benchmark output.

`docker/` holds one Dockerfile per toolchain image: the official image
plus `COPY --from=prova:dev /prova /usr/local/bin/prova`, the deployment pattern
prova documents for containers. Dependencies are cached in the `prova-fixture-cache` volume.
