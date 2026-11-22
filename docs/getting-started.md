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

## What toe stores

Everything lives in `<project>/.toe/` (owner-only permissions): the
SQLite database, captured output and result files. Commands and output
are stored as given, so treat `.toe/` like a log directory: keep it out of
version control and do not share it.
