---
name: toe-python
description: toe setup details for Python projects (pytest). Use together with toe-adopt or toe-test in a Python codebase.
---

# toe for Python

- Use `python3 -m pytest -v -rfE`: `-v` prints each test (recognized live)
  and `-rfE` prints the failure summary lines that carry messages.
- Best: the native stream. With `pytest-reportlog` installed, opt in to
  `inject = ["pytest-reportlog"]` in the suite: toe adds `--report-log`
  through `PYTEST_ADDOPTS` and reads every test live (level `native`), with
  its duration and failure message, whatever the command prints, even
  `pytest -q` inside `make test`.
- `pytest -q` prints dots: toe sees failures only at the end. If the
  project's own command must stay as is and pytest-reportlog is not
  available, opt in to `inject = ["pytest-junit"]`
  in the suite: toe adds `--junitxml` through `PYTEST_ADDOPTS` (shown in
  every summary) and gets every test with its message and duration.
- JUnit for full detail and durations: add `--junitxml=reports/junit.xml`;
  toe ingests it after the run.
- Single test: `select = "python3 -m pytest -v -rfE -k {filter}"`. `-k`
  matches substrings of test names; node ids also work as the filter.
- Use the project's virtualenv: `run = ".venv/bin/python -m pytest -v -rfE"`.
- Fail fast in the fix loop: add `-x` to `select`.
- Sizes: mark slow or network tests (`@pytest.mark.integration`) and run
  them as a separate suite with `-m integration`.
