# Real test-framework output for the quarantine guard (round 16)

Each `<name>.txt` is the combined stdout and stderr of the command below,
run on a scratch copy of the matching `fixtures/` project in which the
`flaky clock` test fails every time and the other tests pass; `<name>.code`
is its exit code. Paths are rewritten to `/w`. Used by
`crates/prova-cli/tests/quarantine_corpus.rs`.

| name | command | exit |
|---|---|---|
| go-test | `go test ./...` | 1 |
| go-test-v | `go test -v ./...` | 1 |
| go-test-json | `go test -json ./...` | 1 |
| gotestsum-pkgname | `gotestsum --format pkgname -- ./...` | 1 |
| gotestsum-testname | `gotestsum --format testname -- ./...` | 1 |
| gotestsum-standard-verbose | `gotestsum --format standard-verbose -- ./...` | 1 |
| cargo-test | `cargo test --no-fail-fast` | 101 |
| cargo-nextest | `cargo nextest run --no-fail-fast` | 100 |
| cargo-nextest-libtest-json | `NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1` (what `prova adopt` proposes for Rust) | 100 |
| vitest-default | `npx vitest run` (Vitest 3.2.4, colours on) | 1 |
| vitest-verbose | `npx vitest run --reporter=verbose` | 1 |
| pytest-q | `python -m pytest -q` | 1 |
| pytest-v | `python -m pytest -v` | 1 |
| pytest-rA | `python -m pytest -rA` | 1 |
| dotnet-test | `dotnet test --nologo` | 1 |
| dotnet-test-normal | `dotnet test --nologo --logger "console;verbosity=normal" --logger "trx;LogFileName=/w/results.trx"` | 1 |
| mvn-q | `mvn -q test` | 1 |
| mvn | `mvn test` | 1 |

Round 18 captures (the flaky test fails on demand through `FLAKY_FAIL=1`;
paths rewritten to `/w`, durations to 0.01s where the tests are replayed):

| name | command | exit |
|---|---|---|
| go-test-v-full-pass | `go test -v -count=1 ./...` (go-make fixture, bugs fixed) | 0 |
| go-test-v-full-fail | `FLAKY_FAIL=1 go test -v -count=1 ./...` | 1 |
| go-test-v-failfast | `FLAKY_FAIL=1 go test -v -count=1 -failfast ./...` | 1 |
| cargo-test-ws-full-pass | `cargo test` in a two-crate workspace (calc, geo) | 0 |
| cargo-test-ws-full-fail | `FLAKY_FAIL=1 cargo test --no-fail-fast` | 101 |
| cargo-test-ws-stops | `FLAKY_FAIL=1 cargo test` (geo's tests never run) | 101 |
| pytest-v-full-pass | `python3 -m pytest -v` in `prova-fixture-python` | 0 |
| pytest-v-full-fail | `FLAKY_FAIL=1 python3 -m pytest -v` | 1 |
| pytest-v-maxfail | `FLAKY_FAIL=1 python3 -m pytest -v -x` | 1 |
| vitest-coverage | `vitest run --reporter=verbose --coverage` (Vitest 3.2.4, a failing `flaky clock` test) | 1 |

Result files written by the same runs: `dotnet-test-normal.trx` (the TRX of
`dotnet-test-normal`; the quiet `dotnet-test` writes none, the test gives it
the same one) and `mvn.junit.xml` (surefire's `TEST-calc.CalcTest.xml`).
Python, .NET and Maven ran in the `prova-fixture-*` images.

Also: `cargo-nextest-retry` is `cargo nextest run --retries 1 -E 'test(retried)'` with a test that panics on its first try and passes on the second (exit 0). `jest-suites` is hand-made in Jest's format (no Jest here): one failed file of three.
