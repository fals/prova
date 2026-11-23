---
name: prova-python
description: Gives the pytest flags and prova.toml settings that let prova see every Python test (pytest-reportlog stream, JUnit XML, -v output, -k filters, virtualenv). Use when writing or fixing a suite's `run` or `select` command in prova.toml, when `prova run` stays at level opaque or hides passing tests, or when selecting a single test, in a Python codebase.
---

# prova for Python

Pick the first that applies:

1. `pytest-reportlog` is installed: opt in to
   `inject = ["pytest-reportlog"]` in the suite. prova adds `--report-log`
   through `PYTEST_ADDOPTS` and reads every test live (level
   `native stream`), with its duration and failure message, whatever the
   command prints, even `pytest -q` inside `make test`.
2. The suite command is yours to write: `python3 -m pytest -v -rfE`. `-v`
   prints each test (recognized live) and `-rfE` prints the failure summary
   lines that carry messages. Add `--junitxml=reports/junit.xml` for
   durations; prova ingests it after the run.
3. The project's own command must stay as is (`pytest -q` prints dots, so
   prova sees failures only at the end): opt in to
   `inject = ["pytest-junit"]` in the suite. prova adds `--junitxml` through
   `PYTEST_ADDOPTS` (shown in every summary) and gets every test with its
   message and duration.

Then:

- Single test: `select = "python3 -m pytest -v -rfE -k {filter}"`. `-k`
  matches substrings of test names; node ids also work as the filter.
- Use the project's virtualenv: `run = ".venv/bin/python -m pytest -v -rfE"`.
- Fail fast in the fix loop: add `-x` to `select`.
- Sizes: mark slow or network tests (`@pytest.mark.integration`) and run
  them as a separate suite with `-m integration`.
