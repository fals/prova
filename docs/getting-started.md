# Getting started

## Install

One static binary, no daemon. Pick one:

```
cargo install --git https://github.com/fals/prova prova-cli      # from source
COPY --from=ghcr.io/fals/prova:<version> /prova /usr/local/bin/prova   # in a Dockerfile
```

Release tarballs (Linux musl x86_64/aarch64, macOS) are attached to each
GitHub release with SHA-256 checksums. The container image holds only the
binary; copy it into the image where your tests run (devcontainer, CI,
agent sandbox). prova must run where the project's toolchain is.

## Five minutes on an existing project

```
cd your-project
prova run -- make test          # any command; zero config; history starts now
prova init                      # skills for agents + AGENTS.md section + .gitignore
prova adopt --write             # proposes prova.toml from what it finds
prova adopt --verify default --filter <a real test name>
```

Then, in the fix loop (humans and agents alike):

```
prova run default --detach      # returns a run id at once
prova wait <run>                # returns on first failure, slow test, stall, or end
prova run default --select <test>
prova history <test>            # new failure, flaky, or pre-existing?
```

Hidden detail behind the project's own command (`pytest -q`, Go's test
cache)? Opt in per suite, never by default:

```toml
[suite.make]
run = "make test"
inject = ["pytest-reportlog"] # every test live; or "pytest-junit", "go-nocache"
                              # also: prova run --inject <recipe> -- <command>
```

Every test live with its duration (native level): Rust via cargo-nextest's
libtest JSON (`prova adopt` proposes it when nextest 0.9.66+ is installed), Go via
`go test -json`, Python via `inject = ["pytest-reportlog"]`, Vitest via
`inject = ["vitest-stream"]` plus `--reporter=default --reporter="$PROVA_VITEST_REPORTER"`
in the suite command.

Exit codes: 0 passed, 1 test failure, 2 usage, 3 stall/slow/killed,
4 prova or infrastructure error, 5 still running.

## Is the change better?

Speed, step counts, accuracy: one procedure. See what a command yields, then
declare the experiment before measuring.

```
prova exp metrics -- go test -run '^$' -bench . -count 3       # BenchmarkX.ns_per_op, wall_ms, ...
prova exp new speed --hypothesis "sieve beats trial division" \
  --metric BenchmarkCountPrimes.ns_per_op --min-effect 5% -- go test -run '^$' -bench . -count 3
# change code (uncommitted; baseline is HEAD)
prova exp candidate speed
prova exp run speed             # interleaved trials, Mann-Whitney + Hodges-Lehmann CI, verdict
```

Native benchmark output is read directly (`go test -bench`, criterion,
pytest-benchmark, Vitest bench, JMH, BenchmarkDotNet); anything else prints
`{"metric": "accuracy", "value": 0.93}` lines. Use `--better higher` for
accuracy or throughput, `--setup "npm ci"` when the baseline checkout needs
dependencies installed.

## Specification first

For algorithms and protocols, state what a correct answer is before writing
code, check that statement, and let it judge (skill `prova-spec`):

```
quint run spec/ipam.qnt --invariant safety        # the specification holds
prova run conformance                                # the implementation follows it
prova exp new ipam-fast --hypothesis "free list makes ADD O(1)" \
  --spec spec/ipam.qnt --oracle "make conformance" \
  --metric add_ns --min-effect 10% -- ./bench-add.sh
```

The specification is frozen with the protocol; a candidate that fails the
oracle is rejected without being measured.

## For MCP clients

```
prova mcp                       # stdio; tools run_start, run_wait, run_status, run_cancel, test_history
```

Example Claude Code config: `claude mcp add prova -- prova mcp`.

## In CI

prova needs no service in CI: keep its home in the job and persist it.

```yaml
# GitHub Actions
- uses: actions/cache@v4
  with:
    path: .prova-home
    key: prova-${{ github.ref_name }}-${{ github.run_id }}
    restore-keys: prova-${{ github.ref_name }}-
- run: prova run --json -- make test
  env:
    PROVA_HOME: ${{ github.workspace }}/.prova-home
    PROVA_ACTOR: github-actions
```

```yaml
# GitLab CI
test:
  variables:
    PROVA_HOME: $CI_PROJECT_DIR/.prova-home
  cache:
    key: prova-$CI_COMMIT_REF_SLUG
    paths: [.prova-home/]
  script:
    - prova run --json -- make test
```

Parallel jobs saving the same cache overwrite each other (last save wins), and
SQLite must not be shared by several writers over network storage (NFS,
ReadWriteMany volumes): give each job its own home, or one writer per volume.

## What prova stores

One central home per user: `~/.local/share/prova` (or `PROVA_HOME`),
with one SQLite file per repository plus a small catalog. Clones and
worktrees of the same repository (same first commit) share one history; the
workspace itself keeps nothing. `prova status --all` and `prova flaky --all`
look across every repository you work on. Every run records where it ran
(host, pod, node, actor: `PROVA_ACTOR`, CI job or user).

Commands and output are stored as given, with owner-only permissions:
treat the home like a log directory and do not share it. A store left in a
workspace `.prova/` by an older prova is imported on first use. If the home is
not writable, prova falls back to `<workspace>/.prova/` and says so.
