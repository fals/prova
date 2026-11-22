# Getting started

## Install

One static binary, no daemon. Pick one:

```
cargo install --git https://github.com/fals/toe toe-cli      # from source
COPY --from=ghcr.io/fals/toe:<version> /toe /usr/local/bin/toe   # in a Dockerfile
```

Release tarballs (Linux musl x86_64/aarch64, macOS) are attached to each
GitHub release with SHA-256 checksums. The container image holds only the
binary; copy it into the image where your tests run (devcontainer, CI,
agent sandbox). toe must run where the project's toolchain is.

## Five minutes on an existing project

```
cd your-project
toe run -- make test          # any command; zero config; history starts now
toe init                      # skills for agents + AGENTS.md section + .gitignore
toe adopt --write             # proposes toe.toml from what it finds
toe adopt --verify default --filter <a real test name>
```

Then, in the fix loop (humans and agents alike):

```
toe run default --detach      # returns a run id at once
toe wait <run>                # returns on first failure, slow test, stall, or end
toe run default --select <test>
toe history <test>            # new failure, flaky, or pre-existing?
```

Hidden detail behind the project's own command (`pytest -q`, Go's test
cache)? Opt in per suite, never by default:

```toml
[suite.make]
run = "make test"
inject = ["pytest-junit"]     # or "go-nocache"; also: toe run --inject <recipe> -- <command>
```

Exit codes: 0 passed, 1 test failure, 2 usage, 3 stall/slow/killed,
4 toe or infrastructure error, 5 still running.

## Performance claims

```
toe exp new speed --hypothesis "cache cuts p50" --metric wall_ms --min-effect 2% -- ./bench.sh
# change code (uncommitted; baseline is HEAD)
toe exp candidate speed
toe exp run speed             # interleaved trials, Mann-Whitney + Hodges-Lehmann CI, verdict
```

## For MCP clients

```
toe mcp                       # stdio; tools run_start, run_wait, run_status, run_cancel, test_history
```

Example Claude Code config: `claude mcp add toe -- toe mcp`.

## In CI

toe needs no service in CI: keep its home in the job and persist it.

```yaml
# GitHub Actions
- uses: actions/cache@v4
  with:
    path: .toe-home
    key: toe-${{ github.ref_name }}-${{ github.run_id }}
    restore-keys: toe-${{ github.ref_name }}-
- run: toe run --json -- make test
  env:
    TOE_HOME: ${{ github.workspace }}/.toe-home
    TOE_ACTOR: github-actions
```

```yaml
# GitLab CI
test:
  variables:
    TOE_HOME: $CI_PROJECT_DIR/.toe-home
  cache:
    key: toe-$CI_COMMIT_REF_SLUG
    paths: [.toe-home/]
  script:
    - toe run --json -- make test
```

Parallel jobs saving the same cache overwrite each other (last save wins), and
SQLite must not be shared by several writers over network storage; see
[ADR 0014](adr/0014-central-store-and-server.md#3-no-server-for-now-local-storage-everywhere-deferred).

## What toe stores

One central home per user (ADR 0014): `~/.local/share/toe` (or `TOE_HOME`),
with one SQLite file per repository plus a small catalog. Clones and
worktrees of the same repository (same first commit) share one history; the
workspace itself keeps nothing. `toe status --all` and `toe flaky --all`
look across every repository you work on. Every run records where it ran
(host, pod, node, actor: `TOE_ACTOR`, CI job or user).

Commands and output are stored as given, with owner-only permissions:
treat the home like a log directory and do not share it. A store left in a
workspace `.toe/` by an older toe is imported on first use. If the home is
not writable, toe falls back to `<workspace>/.toe/` and says so.
