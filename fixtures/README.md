# Brownfield fixtures

One small project per language toe claims, each run the way real projects
run their tests, with no toe configuration. Used by the large tests
(`toe run large`) and by usability checks (a fresh agent adopts one with
only the skills).

| Fixture | Language, framework | Own command | Where it runs in large tests |
|---|---|---|---|
| `rust-make` | Rust, libtest | `make test` (`cargo test --quiet`) | host |
| `go-make` | Go, testing | `make test` (`go test ./...`) | host |
| `python-make` | Python, pytest | `make test` (`pytest -q`) | `python:3.13` |
| `ts-vitest` | TypeScript, Vitest | `npm test` | `node:22` |
| `java-maven` | Java, JUnit 5 + Surefire | `mvn -q test` | `maven:3.9-eclipse-temurin-21` |
| `csharp-dotnet` | C#, xUnit + VSTest | `dotnet test` | `mcr.microsoft.com/dotnet/sdk:9.0` |

Every fixture has the same four tests:

- a real bug: `div` multiplies, so the division test fails until fixed;
- a flaky test: fails on roughly one run in three (clock based);
- a slow test: about 1.5 s;
- a hanging test: hangs only when `HANG=1` is set.

The large tests assert, for each language, that toe finds the bug per test
(not just "the run failed") with an explanation, and that a hang is
reported as a stall with no configuration.

`docker/` holds one Dockerfile per toolchain image: the official image
plus `COPY --from=toe:dev /toe /usr/local/bin/toe`, the deployment pattern
toe documents for containers. Dependencies are cached in the `toe-fixture-cache` volume.
