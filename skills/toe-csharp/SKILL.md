---
name: toe-csharp
description: toe setup details for .NET projects (dotnet test, xUnit, NUnit, MSTest). Use together with toe-adopt or toe-test in a C# codebase.
---

# toe for .NET

- `dotnet test --logger trx` writes `TestResults/*.trx`; toe ingests TRX
  after the run (outcome, duration, message per test).
- With Microsoft.Testing.Platform: `--report-trx`, or `--report-junit`.
- Single test: `dotnet test --logger trx --filter FullyQualifiedName~{filter}`.
- Build once, then test with `--no-build` in the fix loop to shorten runs.
- Hang dumps (`--blame-hang`) are optional; toe already reports stalls
  and slow tests by name.
