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
- Single test: Maven `mvn -q test -Dtest={filter}`, Gradle
  `./gradlew test --tests {filter}` (class or `Class.method`).
- Gradle may skip tests as up to date; add `--rerun-tasks` (or
  `cleanTest`) to the suite so every run really runs.
- Keep integration tests (Failsafe, `integrationTest` task) as a separate
  medium or large suite.
