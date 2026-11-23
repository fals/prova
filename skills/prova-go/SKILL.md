---
name: prova-go
description: Gives the go test flags and prova.toml settings that let prova see every Go test (test2json stream, -count=1 against cached results, -run filters, subtests). Use when writing or fixing a suite's `run` or `select` command in prova.toml, when `prova run` stays at level opaque or hides passing tests, or when selecting a single test, in a Go codebase.
---

# prova for Go

- Always add `-count=1`: otherwise Go replays cached results and prova
  records passes for tests that did not run (flaky tests look stable).
  When the command is the project's own (`make test`), opt in to
  `inject = ["go-nocache"]` instead: prova appends `-count=1` to `GOFLAGS`.
- Best fidelity: `go test -json -count=1 ./...`. prova reads the test2json stream
  natively: per-test start, result, duration and the failure output.
- Plain `go test ./...` (often in `make test`) prints only failures;
  `go test -v` prints every test and is recognized live too.
- Single test: `select = "go test -json -count=1 -run {filter} ./..."`. `-run` is a
  regular expression; anchor it (`^TestDiv$`) when names share prefixes.
- Subtests appear as `TestParent/sub`; select them with `-run 'TestParent/sub'`.
- A package that fails to build shows as test `<package>::(package)`, errored.
- Fail fast: add `-failfast` to `select` for the fix loop.
- Sizes are what a test uses, not how long it takes: a test that starts a
  server or a database, opens a socket or runs a subprocess is medium
  (large if it reaches another host). Keep those in a separate suite, e.g.
  behind a build tag: `run = "go test -json -tags integration ./..."`.
  `httptest` servers run in the test's own process but listen on a
  socket: medium.
- Never rely on `-timeout` to catch hangs; prova reports slow tests by name
  and stalls on its own.
