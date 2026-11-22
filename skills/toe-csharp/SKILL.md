---
name: toe-csharp
description: toe setup details for .NET projects (dotnet test, xUnit, NUnit, MSTest). Use together with toe-adopt or toe-test in a C# codebase.
---

# toe for .NET

- `dotnet test --logger trx` writes `TestResults/*.trx`; toe ingests TRX
  after the run (outcome, duration, message per test).
- With Microsoft.Testing.Platform: `--report-trx`, or `--report-junit`.
- Single test: `dotnet test --logger trx --filter FullyQualifiedName~{filter}`.
- Plain `dotnet test` prints only failing tests (recognized live, with
  their error message); toe notes the hidden passes. Add `--logger trx`
  (or `--logger "console;verbosity=normal"`) for every test.
- A leaked process after `dotnet test` is usually the MSBuild build
  server; toe kills it. `DOTNET_CLI_USE_MSBUILD_SERVER=0` avoids it.
- Build once, then test with `--no-build` in the fix loop to shorten runs.
- Hang dumps (`--blame-hang`) are optional; toe already reports stalls
  and slow tests by name.
