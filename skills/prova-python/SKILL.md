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

- Single test: `select = "python3 -m pytest -v -rfE -k {filter}"`. A plain
  name is a `-k` substring. For a node id (`tests/test_calc.py::test_add`,
  `tests/test_p.py::test_add_cases[two plus $two-2-2-4]`) or the name prova
  prints (`tests.test_calc.TestCalc::test_add`), prova keeps `-k` with the
  test function's name and also passes the node id to pytest as its own
  argument when the file exists, so exactly that test or case runs. If the
  template has its own paths (`pytest tests/ -k {filter}`), those paths are
  collected as well and `-k` alone selects. Keep `-k {filter}` in the
  template; prova does the rest.
- A test's place is the `rootdir:` line pytest prints (`-v`, not `-q`),
  relative to the repository. With `-q`, or a rootdir that is a path inside a
  container, prova reads the ids from the directory the run started in, or
  shows the place as unknown; an unknown place shares no history and no
  quarantine. Prefer `-v`, so two directories' `test_calc.py::t` stay apart.
- Use the project's virtualenv: `run = ".venv/bin/python -m pytest -v -rfE"`.
- Fail fast in the fix loop: add `-x` to `select`.
- Sizes are what a test uses, not how long it takes: a test that starts
  a server, a database or a subprocess, or opens a socket, is medium (large
  if it reaches another host). Mark those (`@pytest.mark.medium`) and run
  them as a separate suite with `-m medium`; a slow pure test stays small.
- Through a wrapper (`./dev`, `docker compose run`, `nix develop -c`): prova
  keeps the wrapper of your earlier runs in what `prova adopt` proposes. A
  wrapper that re-splits its arguments (`$*`) breaks a filter with spaces
  (`test_x[negative 2/-4]`): select with a `-k` substring that has none, or
  run the wrapper with the whole command as one quoted argument (`./dev
  "python3 -m pytest -v -rfE -k {filter}"`), where `{filter}` reaches the
  inner shell as one single-quoted word; write no quotes around it. The
  wrapper must forward `PYTEST_ADDOPTS` for `inject` recipes (and a report
  file such as `pytest-junit` must be written where the container can see it:
  mount the directory); prova warns when the report never appears. Tests that
  run in a container are out of sight: their size is "unknown (ran through
  docker)" unless the suite declares it (`size = "small"` under
  `[suite.<name>]`), and then it reads "small (declared; out of sight)".
  `prova cancel` stops the container named by `--name` or `--cidfile`; for any
  other it kills nothing and says the container may still run (`docker ps`,
  then `docker kill`): give it `--name` so prova stops it. A container is not
  called stalled before 60 s of idle host
  processes: prova sees only its output.
- pytest exit 4 with no test result (a selection or option it rejected) is
  not a test failure: prova reports it as errored and exits 4. `pytest-xdist` verbose lines (`[gw0] [ 33%] FAILED ...`) are read.
