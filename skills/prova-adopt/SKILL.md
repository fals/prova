---
name: prova-adopt
description: Puts an existing project's tests under prova without changing how the project runs them. Use when a repository has no prova.toml yet, when `prova run` reports level opaque, or when asked to set up prova, adopt prova, or make tests run through prova.
---

# Adopt prova in an existing project

prova wraps the test command the project already has. Never edit the
project's test scripts, Makefile or CI config to adopt prova.

## Steps

1. Run the existing command through prova and note the level it reaches:

   ```sh
   prova run -- make test          # or ./scripts/test.sh, npm test, ...
   ```

   The summary line says `level opaque | result files | recognized output | native stream`.

2. Let prova propose a `prova.toml`:

   ```sh
   prova adopt                     # prints a proposal, writes nothing
   prova adopt --write             # writes prova.toml if none exists
   ```

   Read the proposal. `run` is the project's own command. `select` is the
   framework command that runs only tests matching `{filter}`. prova passes
   the filter as one shell word: never put `{filter}` inside quotes of your
   own, with one exception: a wrapper that runs `sh -c "$*"` (`./dev`) takes
   the whole command as one quoted argument, and then `{filter}` goes inside
   that argument: `./dev "dotnet test --filter FullyQualifiedName~{filter}"`.
   There prova hands the filter to the inner shell as one single-quoted
   word, so spaces and `$` (`CalcTest$Division`) survive; write no quotes
   around `{filter}` there.
   `prova adopt` keeps such a wrapper and its quoting from your earlier runs
   (the one most runs used).

3. Verify selection with one real test name taken from step 1 output:

   ```sh
   prova adopt --verify default --filter <test name>
   ```

   Passed means only matching tests ran. If it fails, fix `select` in
   `prova.toml` and verify again. Without a working `select`, every fix
   iteration reruns the whole suite.

4. If step 1 stayed at level opaque, prova sees only pass/fail of the whole
   run. First try an injection recipe: it raises the level through the
   environment, with no edit to the project. Add it to the suite in
   `prova.toml` (`inject = ["pytest-reportlog"]`) or try it once with
   `prova run --inject <recipe> -- <command>`:

   - `pytest-reportlog`: every pytest test live (needs the pytest-reportlog plugin);
   - `pytest-junit`: pytest writes a JUnit report prova reads;
   - `vitest-stream`: prova's Vitest reporter; also add
     `--reporter=default --reporter="$PROVA_VITEST_REPORTER"` to the suite command;
   - `go-nocache`: `go test` reruns tests instead of replaying cached results.
   - `gotestsum-json`: a command that runs gotestsum writes Go's own event
     stream (`GOTESTSUM_JSONFILE`), read live: every test, with packages
     (gotestsum's default output shows only failures).

   Only if no recipe fits, propose to the user (do not apply silently) one
   change that raises the level, as a separate commit they can refuse, for
   example enabling the framework's JUnit XML reporter or verbose output.
   See the `prova-<language>` skill for the exact flag.

5. `.prova/` must be in `.gitignore`: `prova init` (the command that
   installs these skills) adds it; otherwise add the line. `prova.toml`
   belongs in version control; commit it when the user's workflow allows.

Then use the `prova-test` skill for the fix loop.

If the project's tests run through a wrapper (`./dev make test`,
`docker compose run app pytest`), run it once with `prova run -- <wrapper
command>` before `prova adopt`: the proposal keeps that wrapper in `run` and
`select`. `prova adopt --verify` says first why a command could not run
(exit 127, `No module named pytest`) and gives the filter syntax of the
framework it found. When a selection ran 0 tests it says the filter did
not reach the framework or matched nothing, and shows the output to read.
Surefire and VSTest select by method: a parameterized case runs with its
siblings.
