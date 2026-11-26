# 0036: A flaky flip happens within one suite

Status: Accepted (2026-11-26). Amends [0018](0018-honest-history-and-live-nextest.md) §1.

## Context

ADR 0018 computes a test's history, flakiness, "seen before" and "last
passed" over every suite that ran it. Since tests keep their place (0031),
every ad hoc command (`prova run -- ...`) shares that history too. A test
that failed every time under one command (`BREAK=1 cargo test`, another
flag, another suite) and passed under another on the same code was called
`FLAKY`, and the run's `next:` said "only known flaky test(s) failed ...
nothing else failed" for a deterministic failure (release-candidate
acceptance, 2026-11-26). Versions up to 0.1.8 did not, because ad hoc
commands had histories of their own.

## Decision

A test is flaky when it passed and failed on the same code, in the same
environment, **in the same suite**: the same named suite, or the same ad
hoc command. A pass in another suite on the same code is still history
(the test is not "first time seen", and that pass is its "last passed"),
but it is no evidence of flakiness and no evidence that the failure
depends on the environment. Retries and repeats within one run count as
before.

## Consequences

- Two suites that run the same tests the same way (`make test` and a
  direct `default`) no longer pool their flips: a flaky test is called
  flaky once it flipped in one of them.
- A failure that only one way of running the tests produces reads as a
  failure (`new failure` or `one earlier pass`), with the other suite's
  pass as its last pass.
