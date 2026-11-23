# Research 03: formalization, experiment model, statistics, storage

Date: 2026-11-22. Web-researched by a subagent, mostly from search-result summaries.
Items marked **[unverified]** come from background knowledge and were not re-checked.

## 1. Lean 4 applicability

**Spec/model beside the code.** Feasible. A lifecycle state machine with proved invariants is small Lean and needs no mathlib.

**Differential testing against an executable Lean model.** The proven pattern, used by AWS Cedar.
- `cedar-spec` holds `cedar-lean` (model and proofs), `cedar-drt` (fuzzing and differential testing), `cedar-policy-generators`.
- `cargo-fuzz` feeds millions of inputs to both the Lean model and the Rust code; disagreement indicates a bug.
- No Cedar release ships unless model, proofs and DRT are current.
- Build only tested on Amazon Linux 2, Docker suggested: real integration cost.
- Sources: https://github.com/cedar-policy/cedar-spec, https://aws.amazon.com/blogs/opensource/lean-into-verified-software-development, https://arxiv.org/pdf/2407.01688, https://lean-lang.org/use-cases/cedar/

**Rust-to-Lean (Aeneas/Charon).** Active, aimed at crypto and kernel-grade code (SymCrypt, Linux Binder). Works on a safe-Rust subset. Overkill for a CLI with IO, SQLite and async.
- https://github.com/AeneasVerif/aeneas
- https://microsoft.com/en-us/research/blog/verifying-rust-cryptography-in-symcrypt-from-standards-to-code

**Rust-native alternatives.**
- Kani: contracts and harnesses only, easiest incremental adoption. https://model-checking.github.io/kani/tool-comparison.html
- Verus: richer invariants, roughly 5 lines of proof per line of implementation in reported case studies.
- Creusot: MIR to Why3, mostly safe Rust.

**Lean as a runtime dependency.** Bad idea **[unverified reasoning]**: C FFI to the Lean runtime, second toolchain in every build and container, no stable ABI. Cedar keeps Lean test-only.

**Toolchain cost.** elan plus lake are light without mathlib. Mathlib plus dependencies is around 3.9 GB on disk (anecdotal Zulip figure). No official current numbers found.

**Lighter alternatives.**
- **Quint**: TLA-style semantics, typed programmer-friendly syntax. `quint-connect` (Rust crate, v0.1.1 seen) generates traces from the spec and replays them through a Rust `Driver`, checking state after each step. Quint repo ships agent skills for writing specs. Crate is young: API stability risk. https://quint-lang.org/posts/quint_connect, https://docs.rs/quint-connect, https://github.com/quint-co/quint
- TLA+/PlusCal, Alloy 6, P: viable, no first-class Rust trace-replay bridge **[unverified]**.
- Data contracts: JSON Schema via `schemars`, TypeSpec or protobuf **[unverified]**.

**Cost/benefit.** prova's risk is lifecycle legality (no verdict without baseline, trials immutable after completion, resume idempotence) and statistical misuse, not deep algorithmic correctness. Model checking plus property tests cover lifecycle risk at a small fraction of the cost of proofs.

## 2. Experiment formalization prior art

- **MLflow**: Experiment has Runs; a Run carries Params, Metrics, Tags; artifacts in a separate store. https://www.mlflow.org/docs/latest/rest-api.html
- **W&B, DVC, Aim, Sacred [unverified]**: same shape. DVC ties experiments to git refs. Sacred captures config, host info, source snapshot.
- **Optuna**: Study contains Trials; `TrialState` is RUNNING, WAITING, COMPLETE, PRUNED, FAIL; NaN becomes FAIL. https://optuna.readthedocs.io/en/latest/reference/generated/optuna.trial.TrialState.html
- **Pre-registration [unverified, standard practice]**: declare before data the hypothesis with direction, one primary metric, guardrail metrics, minimum detectable effect, sample size, stopping rule, analysis method. Immutable after first trial.
- **Karpathy autoresearch**: appends `results.tsv` (`commit, val_bpb, memory_gb, status, description`), status keep/discard/crash. Keep if and only if metric beats best so far, otherwise `git reset`. Single-run strict comparison, no statistics. This is the gap prova fills. (Secondary sources: deepwiki.com/karpathy/autoresearch.)
- **OpenEvolve/AlphaEvolve**: island model with MAP-Elites archive. **AIDE**: solution tree with Draft, Debug, Improve. **AI Scientist v2**: best-first tree search. (Secondary sources.)

## 3. Benchmark statistics

| Tool | Behavior |
|---|---|
| criterion.rs | 3 s warmup; bootstrap 100,000 resamples for CIs; Tukey fences flag (not drop) outliers; t-test on bootstrap samples against previous run |
| hyperfine | at least 10 runs by default; `--warmup`; outlier warnings; JSON export; no built-in significance test found |
| benchstat | Mann-Whitney U by default; no longer rejects outliers; prints "too few samples" |
| pyperf | `system tune` sets governor, turbo, isolated CPUs; pins workers |
| Apache Otava (ex-Hunter) | E-divisive change-point detection with Student's t-test for determinism |
| Kalibera & Jones (ISMM 2013) | repeat at nested levels (build, execution, iteration); report effect-size CI |

Sources: https://github.com/bheisler/criterion.rs (book/src/analysis.md), https://pkg.go.dev/golang.org/x/perf/benchstat, https://pyperf.readthedocs.io/en/latest/system.html, https://otava.apache.org/docs/math, https://kar.kent.ac.uk/id/document/3127886

**Small-n guidance (n = 5..30).**
- Mann-Whitney has a p-value floor set by sample counts alone. Exact, two-sided, equal groups: n=3 gives 0.10, n=4 gives 0.029, n=5 gives 0.008. So at least 5 per arm for alpha 0.05; 10 or more preferable.
- Always report an effect-size CI (Hodges-Lehmann shift, or bootstrap CI of median/mean ratio), not just p.
- Bootstrap CIs unreliable below about n=10 **[judgment]**.
- Interleave baseline and candidate runs (ABAB) to cancel drift **[judgment]**.

**Environment noise.** Controls: cpuset pinning, performance governor, turbo off, isolated CPUs. Record per trial **[judgment]**: CPU model, core count, governor, kernel, cgroup limits, load average, toolchain version, git commit and dirty flag. Flag comparisons across differing fingerprints.

**Rust crates.** `anofox-statistics` (Mann-Whitney with Hodges-Lehmann CI, bootstrap; young, check maintenance). `statrs` for distributions. Exact Mann-Whitney plus bootstrap is around 200 lines, so vendoring is reasonable.

## 4. Embedded storage

- **SQLite (WAL)**: requires all processes on the same host; not for network filesystems (fall back to `journal_mode=DELETE`). Local Docker volume or bind mount is fine. https://www.sqlite.org/wal.html, https://sqlite.org/useovernet.html
- **DuckDB**: embedded file allows one read-write process or many read-only. DuckDB 2.0.0 LTS (2026-10-21) added the Quack client-server protocol for multi-process writes, described as not yet stable, and it needs a server process. Poor primary store for CLI plus daemon; good optional analytics reader. https://duckdb.org/docs/current/connect/concurrency.html, https://duckdb.org/2026/10/21/announcing-duckdb-200
- **Turso Database (Rust rewrite)**: sources conflict on production readiness. libSQL's value is replication, not needed here.
- **redb, fjall, sled [unverified]**: key-value, no SQL or ad hoc analytics.
- **Postgres**: too heavy; breaks single-file, zero-daemon.
- **Migrations**: `rusqlite_migration` (uses `user_version` pragma, no CLI, no history table); `refinery` (embedded SQL or Rust migrations, no rollbacks).

## 5. Formats worth aligning with

- **Bencher Metric Format (BMF)**: `{benchmark: {measure: {value, lower_value?, upper_value?}}}`. Simplest export target that carries a CI. https://bencher.dev/docs/reference/bencher-metric-format
- **OpenTelemetry**: `test.case.result.status`, `test.suite.run.status`, `cicd.*`. All at Development stability: map on export, do not use as storage schema. https://opentelemetry.io/docs/specs/semconv/registry/attributes/test
- **github-action-benchmark**: array of `{name, unit, value}`.
- **Ingest adapters [unverified]**: criterion `estimates.json`/`sample.json`, hyperfine `--export-json`, k6 summary/NDJSON, vegeta JSON, oha JSON, Gatling `simulation.log`.

## Recommendations

**Formalization.**
1. No Lean in v1: second toolchain and language, while prova's bugs will live in IO and statistics.
2. Run and experiment lifecycles as a Quint spec with invariants: no verdict without baseline and at least N trials; protocol frozen after first trial; terminal states absorbing; resume idempotent.
3. Model-check, then replay traces against Rust via `quint-connect`; fall back to `proptest` state-machine tests against a Rust reference model if the crate proves immature.
4. Data contracts: Rust types with `serde` + `schemars` producing JSON Schema; SQLite STRICT tables with CHECK constraints.
5. Kani only for pure arithmetic kernels.
6. Revisit Lean only if the statistical verdict procedure becomes a selling point; then follow the Cedar pattern (test-only executable model, differential fuzzing, no mathlib).

**Experiment data model.** `experiment`, immutable hashed `protocol`, `variant` (baseline or candidate, git commit, params, lineage), `trial` (Optuna-like state, env fingerprint), `sample`, `artifact`, `verdict` (statistic, p, effect CI, accept/reject/inconclusive, protocol hash). Improvement loop is an experiment with many candidate variants and an append-only decision log, each keep gated by a verdict.

**Statistics defaults.** 1 to 3 warmup runs discarded; n = 10 per arm, hard minimum 5, interleaved; exact two-sided Mann-Whitney at alpha 0.05 plus 95% CI of relative change that must exclude a practical threshold (default 2%); report median, MAD, CI; flag but keep outliers; "inconclusive" triggers more trials up to a declared maximum; multiple-comparison control (Holm) in improvement loops and re-validation of the final winner with fresh trials; comparison invalid if env fingerprints differ.

**Storage.** SQLite single file via `rusqlite` (bundled), WAL, `synchronous=NORMAL`, `busy_timeout` around 5 s, `foreign_keys=ON`, STRICT tables; append-only `event` table with JSON payload; `rusqlite_migration`; optional Parquet export for DuckDB analytics.

## Gaps

Official mathlib size and CI time; JMH, Google Benchmark, W&B, DVC, Aim, Sacred, Ax details; `anofox-statistics` maintenance; Turso-Rust production status; `quint-connect` repository home and latest release.
