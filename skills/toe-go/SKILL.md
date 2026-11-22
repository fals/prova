---
name: toe-go
description: toe setup details for Go projects (go test). Use together with toe-adopt or toe-test in a Go codebase.
---

# toe for Go

- Always add `-count=1`: otherwise Go replays cached results and toe
  records passes for tests that did not run (flaky tests look stable).
  When the command is the project's own (`make test`), opt in to
  `inject = ["go-nocache"]` instead: toe appends `-count=1` to `GOFLAGS`.
- Best fidelity: `go test -json -count=1 ./...`. toe reads the test2json stream
  natively: per-test start, result, duration and the failure output.
- Plain `go test ./...` (often in `make test`) prints only failures;
  `go test -v` prints every test and is recognized live too.
- Single test: `select = "go test -json -count=1 -run {filter} ./..."`. `-run` is a
  regular expression; anchor it (`^TestDiv$`) when names share prefixes.
- Subtests appear as `TestParent/sub`; select them with `-run 'TestParent/sub'`.
- A package that fails to build shows as test `<package>::(package)`, errored.
- Fail fast: add `-failfast` to `select` for the fix loop.
- Sizes: tests that need a network or files belong in a separate suite,
  e.g. behind a build tag: `run = "go test -json -tags integration ./..."`.
- Never rely on `-timeout` to catch hangs; toe reports slow tests by name
  and stalls on its own.
