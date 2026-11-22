# 0005: Small, medium, large test sizes

Status: Accepted (2026-11-22). Amended by [0013](0013-black-box-adoption.md): sizes are observed first; declared sizes are an optional override.

## Context

Models write broad, slow, flaky tests. Google's size taxonomy constrains what a test may touch, which is what makes small tests fast and deterministic ([01](../research/01-formats-liveness-sizes-flakiness.md), Software Engineering at Google ch. 11).

## Decision

Every suite in `toe.toml` declares a size. Sizes are defined by allowed resources:

| | small | medium | large |
|---|---|---|---|
| Processes | one | many, one machine | any |
| Network | none | localhost only | any |
| Disk, database | none (in-memory only) | yes | yes |
| Sleep, blocking calls | no | yes | yes |
| Slow budget default | 1 s per test | 30 s per test | 300 s per test |
| Hard ceiling per test | 60 s | 300 s | 900 s |

Ceilings follow Bazel's size-to-timeout mapping. Slow budget defaults are toe's own starting values, replaced by history once it exists ([0004](0004-liveness-and-fail-fast.md)).

Rules:

1. Size is **declared** per suite, optionally overridden per test through the framework's native tagging where the adapter can read it.
2. toe **reports violations it can observe cheaply**: a small test that exceeds the small ceiling or leaks a child process is flagged. Network and disk sandboxing are not enforced in v1.
3. `toe run` defaults to small suites; medium and large are named explicitly. This steers the inner loop to the fast tests.
4. Analysis reports the size distribution and warns when it drifts far from a small-heavy shape (rough guideline 80/15/5).
5. Skills instruct agents to write the smallest test that can fail for the right reason, and to justify any medium or large test.

## Consequences

- The inner loop stays fast by default.
- Declared sizes can lie until enforcement exists; observation-based flags catch the worst cases.

## Alternatives

- Enforce with sandboxing (network namespaces, seccomp) now: valuable, Linux-specific, deferred.
- Unit/integration/end-to-end by scope: scope is subjective; size is checkable.
