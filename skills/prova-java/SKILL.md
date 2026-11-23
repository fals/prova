---
name: prova-java
description: prova setup details for Java and Kotlin projects (Maven, Gradle, JUnit). Use together with prova-adopt or prova-test in a JVM codebase.
---

# prova for Java and Kotlin

- Maven Surefire writes `target/surefire-reports/TEST-*.xml`; Gradle
  writes `build/test-results/test/*.xml`. prova ingests both after the run,
  including Surefire's `flakyFailure` reruns as attempts.
- Live per-test events need the build's console output; per-test results
  always come from the XML files.
- Single test: Maven `mvn -q test -Dtest={filter}` with Surefire's syntax,
  `CalcTest#divides` (or `CalcTest#div*`, `CalcTest#a+b`); prova names that
  test `calc.CalcTest::divides`, and `prova adopt --verify` accepts either
  form. Gradle: `./gradlew test --tests {filter}` (class or `Class.method`).
- `-q` hides Surefire's console lines, so prova sees tests only when the XML
  is written at the end. A hung test is still caught as a stall (no CPU, no
  output); prova then names the tests of the last finished run that never
  reported, the hung one among them.
- Gradle may skip tests as up to date; add `--rerun-tasks` (or
  `cleanTest`) to the suite so every run really runs.
- Keep integration tests (Failsafe, `integrationTest` task) as a separate
  medium or large suite.
