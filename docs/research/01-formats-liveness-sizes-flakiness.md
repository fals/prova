# Research 01: result formats, liveness, test sizes, flakiness, agent tooling

Date: 2026-11-22. Web-researched by a subagent. Items marked **[unverified]** come from memory or a single secondary source.

## 1. Result formats

| Format | Live per-test events? | Native support | Notes |
|---|---|---|---|
| JUnit XML | No, written at end | Broadest: pytest, Vitest, Surefire/Gradle, GoogleTest, nextest, Bazel; .NET via preview extension | No official spec; flavors differ ([testmoapp/junitxml](https://github.com/testmoapp/junitxml)) |
| CTRF (JSON) | No (single report object) | Almost none natively; reporter plugins per framework ([ctrf-io](https://github.com/ctrf-io)) | Models retries, flaky, attachments, parameters ([spec](https://ctrf.io/docs/specification/test)) |
| Open Test Reporting | Yes: event format for file or socket streaming | JUnit Platform only; third-party `pytest-otr` | [repo](https://github.com/ota4j-team/open-test-reporting/) |
| TAP 14 | Yes (line stream) | Vitest `tap`; kernel KTAP | Spec stagnant ([spec](https://testanything.org/tap-version-14-specification.html)) |
| Bazel BEP | Yes, per target/attempt/shard, not per test case | Bazel only | [glossary](https://bazel.build/remote/bep-glossary.md) |
| TeamCity service messages | Yes | Needs dedicated support per framework | [docs](https://www.jetbrains.com/help/teamcity/service-messages.html) |
| Go `test2json` | Yes, unbuffered | Go built-in (`go test -json`) | [docs](https://pkg.go.dev/cmd/test2json) |
| libtest JSON | Yes | Unstable (`-Z unstable-options`) | [rustc book](https://doc.rust-lang.org/rustc/tests/index.html) |
| nextest | `libtest-json(-plus)` experimental; JUnit stable | Rust | JUnit includes `<flakyFailure>`/`<rerunFailure>` ([docs](https://nexte.st/docs/machine-readable/libtest-json/)) |
| pytest | `--junitxml` at end; `pytest-reportlog` JSON Lines flushed per line | junitxml built in; reportlog is a plugin | [PyPI](https://pypi.org/project/pytest-reportlog) |
| Vitest / Jest | Vitest reporter API `onTestCaseResult` gives live events | Vitest built in | Jest not verified ([docs](https://vitest.dev/guide/reporters)) |
| .NET TRX | End of run | Microsoft.Testing.Platform (`--report-trx`) | [docs](https://learn.microsoft.com/en-us/dotnet/core/testing/microsoft-testing-platform-test-reports) |

**JUnit XML weaknesses**: no published Ant schema (circulating XSD is Surefire's; Jenkins' is looser); retries and flaky results have no standard; attachments are conventions; per-testcase properties not universal.

**Conclusion**
- Best universal ingest format: JUnit XML, parsed leniently (superset of flavors).
- Best live streaming: no cross-language winner. Each ecosystem has its own.
- Realistic design: native streaming adapters per framework normalised into toe's own event schema, with JUnit XML as fallback and cross-check. CTRF is a good source of field names and an export target, not an ingest baseline.

## 2. Fail-fast and hang detection

- **nextest**: `slow-timeout` (default 60 s) only marks SLOW; kill is opt-in via `terminate-after`. Each test in its own process group: SIGTERM, grace period, SIGKILL. `leak-timeout` detects children that outlive the test ([slow tests](https://nexte.st/docs/features/slow-tests/)).
- **Go**: `-failfast`; `-timeout` defaults to 10 m for the whole binary; no per-test timeout flag ([go testflag](https://pkg.go.dev/cmd/go)).
- **pytest-timeout**: per-test; signal or thread method ([PyPI](https://pypi.org/project/pytest-timeout)).
- **Bazel**: size implies timeout: small 60 s, medium 300 s, large 900 s, enormous 3600 s. `TEST_PREMATURE_EXIT_FILE` catches silent `exit()` ([test encyclopedia](https://bazel.build/reference/test-encyclopedia)).
- **CI inactivity watchdogs**: CircleCI kills after 10 min with no output; buffered output causes false kills.

**Synthesis**: split "slow" from "dead". Report slowness as an event at a budget from test size or historical p95; keep hard kills as a separate, later step. Base liveness on structured test events, not raw stdout. Kill the whole process tree; treat leaked children as a result.

## 3. Google test sizes

- **Definitions (SWE at Google, ch. 11)**: small = single process, no sleep, no I/O, no blocking calls, no network or disk. Medium = single machine, multiple processes and threads, localhost only. Large = anything, multiple machines. No numeric time limits in the book ([ch. 11](https://abseil.io/resources/swe-book/html/ch11.html)).
- **Distribution**: about 80% unit, 15% integration, 5% end-to-end, "very rough guideline" (by scope, not size).
- **Flakiness**: tests lose value approaching 1% flakiness; Google's rate about 0.15%.
- **2010 blog table** ([post](https://testing.googleblog.com/2010/12/test-sizes.html)): confirmed rows are network (No / localhost only / Yes) and database (No / Yes / Yes). Other rows **[unverified]**: filesystem No/Yes/Yes; multiple threads No/Yes/Yes; sleep No/Yes/Yes; time limit 60 / 300 / 900+ s.
- **Bazel encoding**: `size` attribute sets default timeout; separate `timeout` attribute.

## 4. Flakiness and history

| Source | Definition / method | Identity |
|---|---|---|
| Google | pass and fail with the same code; about 1.5% of runs flaky ([blog](https://testing.googleblog.com/2016/05/)) | |
| Meta PFS | probability of failing in a good state; Bayesian ([post](https://engineering.fb.com/2020/12/10/developer-tools/probabilistic-flakiness/)) | |
| nextest | failed, then passed on retry within one run | |
| Trunk | pass-on-retry on same commit, failure rate over window ([docs](https://docs.trunk.io/flaky-tests/detection)) | variants by environment |
| Buildkite | same test, same commit, different results; transition count ([docs](https://buildkite.com/docs/test-engine/reduce-flaky-tests)) | suite + scope + name |
| Datadog | Early Flake Detection reruns new tests; quarantine states ([docs](https://docs.datadoghq.com/tests/flaky_management/)) | hash of repository + fully qualified name |
| Develocity | fails then passes on retry in one build | |
| BuildPulse | compares outcomes on equivalent git tree SHAs | |

**Cheap local statistics**: pass-on-retry within a run; pass/fail flips on an identical tree hash (dirty tree never counts as same code); transition count over last N runs; failure rate per window; duration p50/p95 against baseline; failure fingerprint by normalised message plus top stack frames.

## 5. Agent-oriented tools

- Vitest 4.1 agent-aware reporter (auto-selected when an agent is detected), Allure agent mode, testthat `LlmReporter`, pytest agent reporters.
- MCP test servers: `mcp-pytest-runner`, `mcp-test-runner` variants (pytest, Jest, Go, Flutter).
- nextest record/replay/rerun: experimental local run store; closest prior art for history, Rust-only ([docs](https://nexte.st/docs/features/record-replay-rerun/)).
- Caution: a 2026 study found tool-output reduction does not reliably predict lower billed cost (compression can induce extra turns) ([arXiv 2607.12161](https://arxiv.org/pdf/2607.12161)).
- **Gaps found**: single-language or few frameworks; results only after the run, no first-failure event or abort; no cross-run history, flake statistics, or size classification.

## Design recommendations

1. Internal model is an event stream (run started, test started, output, test finished with attempt, run finished) in JSON Lines.
2. Adapters in priority order: Go `-json`; pytest reportlog; Vitest reporter; JUnit Platform via Open Test Reporting; nextest; .NET and GoogleTest via end-of-run files. Lenient JUnit XML parser as universal fallback.
3. Liveness: `slow` events at a per-size budget (Bazel ceilings as outer bound, historical p95 times a factor as working budget); agent decides abort. Kill by process group with short grace. Record leaked children.
4. Fail-fast: surface first failure immediately; pass native flags through.
5. Sizes: declared; verify small tests by observation where cheap.
6. Identity: hash of repo + framework + file + suite path + name + normalised parameters; environment as a separate variant dimension.
7. Run record: command, arguments, selected env vars, tree hash plus dirty flag, toolchain versions, per-attempt outcomes.
8. Flake statistics: pass-on-retry, same-tree flips, transition count. Bayesian score later.

## Not verified

Google 2010 table rows beyond network and database; native TeamCity/TAP emitters; Jest `--json`/`--bail`; Catch2 reporters; whether nextest libtest-json flushes per test; maintenance status of the MCP test servers.
