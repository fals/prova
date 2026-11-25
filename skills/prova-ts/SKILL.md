---
name: prova-ts
description: Gives the Vitest, Jest and node --test settings that let prova see every JavaScript or TypeScript test (prova's Vitest reporter, JUnit reporters, -t filters, no watch mode). Use when writing or fixing a suite's `run` or `select` command in prova.toml, when `prova run` stays at level opaque or hides passing tests, or when selecting a single test, in a JavaScript or TypeScript codebase.
---

# prova for JavaScript and TypeScript

- Vitest, best: prova's own reporter streams every test live (level
  `native stream`, durations and messages). Opt in and add the reporter next to
  the default one; the project's files stay untouched:

  ```toml
  [suite.default]
  run = "npx vitest run --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\""
  select = "npx vitest run --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\" -t {filter}"
  inject = ["vitest-stream"]
  ```

  Through npm: `npm test -- --reporter=default --reporter="$PROVA_VITEST_REPORTER"`.
  Vitest 3.x sends a worker's task updates in batches (its `throttle` in
  `@vitest/runner` can leave the last update unsent until the next one), so a
  start or an early failure can reach prova seconds late, until another test
  starts or ends. A test that hangs is still named by the stall, a few seconds
  later than the others.
- Vitest without the reporter: `npx vitest run --reporter=verbose` prints
  each test with ✓/×, recognized live. Add `--reporter=junit --outputFile=reports/junit.xml`
  (as a second reporter) for durations and messages.
- Jest: add `jest-junit` and run `npx jest --ci --reporters=default --reporters=jest-junit`;
  prova ingests `junit.xml`.
- Single test: Vitest `-t {filter}`, Jest `-t {filter}` (test name pattern).
- Always `vitest run`, never watch mode: watch mode never ends.
- Node's own runner (`node --test`) prints TAP with `--test-reporter=tap`,
  which prova recognizes.
