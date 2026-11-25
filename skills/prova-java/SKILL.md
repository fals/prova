---
name: prova-java
description: Gives the Maven and Gradle settings that let prova see every JVM test (Surefire and Gradle XML reports, -Dtest and --tests filters, up-to-date skips, JUnit). Use when writing or fixing a suite's `run` or `select` command in prova.toml, when `prova run` stays at level opaque or hides passing tests, or when selecting a single test, in a Java or Kotlin codebase.
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
- `-DdisableXmlReport=true` turns Surefire's XML report off: with `-q` prova
  then sees no test at all and says so in a warning. Remove the flag.
- Gradle may skip tests as up to date; add `--rerun-tasks` (or
  `cleanTest`) to the suite so every run really runs.
- Keep tests that start servers, containers or databases (often the
  Failsafe or `integrationTest` ones) as a separate medium suite, or large
  if they reach other hosts. Size is what a test uses, not its name or
  duration.
