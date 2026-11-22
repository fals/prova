---
name: toe-adopt
description: Put an existing project's tests under toe without changing how the project runs them. Use when a repository has no toe.toml yet, when `toe run` reports level opaque, or when asked to set up toe, adopt toe, or make tests run through toe.
---

# Adopt toe in an existing project

toe wraps the test command the project already has. Never edit the
project's test scripts, Makefile or CI config to adopt toe.

## Steps

1. Run the existing command through toe and note the level it reaches:

   ```
   toe run -- make test          # or ./scripts/test.sh, npm test, ...
   ```

   The summary line says `level opaque | result files | recognized output | native stream`.

2. Let toe propose a `toe.toml`:

   ```
   toe adopt                     # prints a proposal, writes nothing
   toe adopt --write             # writes toe.toml if none exists
   ```

   Read the proposal. `run` is the project's own command. `select` is the
   framework command that runs only tests matching `{filter}`. toe passes
   the filter as one shell word: never put `{filter}` inside quotes.

3. Verify selection with one real test name taken from step 1 output:

   ```
   toe adopt --verify default --filter <test name>
   ```

   Passed means only matching tests ran. If it fails, fix `select` in
   `toe.toml` and verify again. Without a working `select`, every fix
   iteration reruns the whole suite.

4. If step 1 stayed at level opaque, toe sees only pass/fail of the whole
   run. Propose to the user (do not apply silently) one change that raises
   the level, as a separate commit they can refuse, for example enabling
   the framework's JUnit XML reporter or verbose output. See the
   `toe-<language>` skill for the exact flag.

5. Add `.toe/` to `.gitignore`. Commit `toe.toml`.

Then use the `toe-test` skill for the fix loop.
