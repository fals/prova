---
name: prova-ts
description: prova setup details for JavaScript and TypeScript projects (Vitest, Jest). Use together with prova-adopt or prova-test in a JS/TS codebase.
---

# prova for JavaScript and TypeScript

- Vitest, best: prova's own reporter streams every test live (level
  `native`, durations and messages). Opt in and add the reporter next to
  the default one; the project's files stay untouched:

  ```toml
  [suite.default]
  run = "npx vitest run --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\""
  select = "npx vitest run --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\" -t {filter}"
  inject = ["vitest-stream"]
  ```

  Through npm: `npm test -- --reporter=default --reporter="$PROVA_VITEST_REPORTER"`.
- Vitest without the reporter: `npx vitest run --reporter=verbose` prints
  each test with ✓/×, recognized live. Add `--reporter=junit --outputFile=reports/junit.xml`
  (as a second reporter) for durations and messages.
- Jest: add `jest-junit` and run `npx jest --ci --reporters=default --reporters=jest-junit`;
  prova ingests `junit.xml`.
- Single test: Vitest `-t {filter}`, Jest `-t {filter}` (test name pattern).
- Always `vitest run`, never watch mode: watch mode never ends.
- Node's own runner (`node --test`) prints TAP with `--test-reporter=tap`,
  which prova recognizes.
