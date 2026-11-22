# Brownfield fixtures

Small projects that run their tests the way real projects do (`make test`),
with no toe configuration. Used by large tests and by the usability check
(AGENTS.md, M5): a fresh agent adopts one with only the skills.

Each fixture has:

- a real bug: one test fails until the code is fixed;
- a flaky test: fails on roughly one run in three;
- a slow test: about 1.5 s;
- a hanging test: hangs only when `HANG=1` is set.
