---
name: prova-csharp
description: prova setup details for .NET projects (dotnet test, xUnit, NUnit, MSTest). Use together with prova-adopt or prova-test in a C# codebase.
---

# prova for .NET

- `dotnet test --logger trx` writes `TestResults/*.trx`; prova ingests TRX
  after the run (outcome, duration, message per test).
- With Microsoft.Testing.Platform: `--report-trx`, or `--report-junit`.
- Single test: `dotnet test --logger trx --filter FullyQualifiedName~{filter}`.
- Plain `dotnet test` prints only failing tests (recognized live, with
  their error message); prova notes the hidden passes. Add `--logger trx`
  (or `--logger "console;verbosity=normal"`) for every test.
- A leaked process after `dotnet test` is usually the MSBuild build
  server; prova kills it. `DOTNET_CLI_USE_MSBUILD_SERVER=0` avoids it.
- Build once, then test with `--no-build` in the fix loop to shorten runs.
- Hang dumps (`--blame-hang`) are optional. TRX arrives at the end, so a
  hung test shows as a stall (no CPU, no output, `--stall-after`, 60 s by
  default); prova names the tests of the last finished run that have not
  reported yet, the hung one among them. Cancel it there.
