# 0019: One command per intent, no shell loops around prova

Status: Accepted (2026-11-23). Amends [0004](0004-liveness-and-fail-fast.md) (how long a wait may last, and who decides) and [0012](0012-skills-packaging.md) (what the skills teach).

## Context

prova exists so that an agent runs tests with one structured command instead
of ad hoc shell. Yet in this repository's own session, the agent building
prova ran the large suite like this:

```sh
R=$(.prova/stable/bin/prova run --detach --stall-after 5m -- sh -c 'cargo test -p prova-cli --test fixtures_large -- --ignored --test-threads 1' 2>/dev/null | head -1 | awk '{print $NF}'); echo $R
for i in $(seq 1 19); do
  .prova/stable/bin/prova wait $R --until done --max-wait 30s >/dev/null 2>&1
  s=$(.prova/stable/bin/prova status $R 2>&1 | sed -n 2,3p)
  case "$s" in *RUNNING*) ;; *) break;; esac
done; echo "$s"
```

Hard to read, easy to get wrong, and every failure in it is silent. Each
piece exists for a reason that prova should have handled:

1. **The harness limits one tool call.** A shell call in Claude Code may last
   10 minutes at most (2 by default); other harnesses have similar ceilings.
   The large suite takes 15. One `prova run large`, attached, would be killed
   by the harness before it ends.
2. **`prova wait` returns every 30 s by default** (0004: "a short maximum").
   That keeps waits bounded, but "tell me when it is done" then needs one call
   per 30 s. Each call is a model turn: slow and paid for. The agent folds the
   turns into a loop, and picks `seq 1 19` × 30 s to stay under the harness
   ceiling.
3. **The loop has to read human output.** It extracts the run id with
   `head -1 | awk`, recognises "still running" by matching `*RUNNING*` in the
   second line of `status`, and throws stdout and stderr away so the loop
   stays quiet. A changed message, a warning line or an error breaks it
   without a sound.
4. **Nobody can see anything for minutes.** The loop throws all output away,
   so the person watching the session sees a shell that hangs for a quarter
   of an hour with no sign of life, no failing test, no progress, and cannot
   tell a working run from a stuck one. prova is built for agents, but people
   run it and watch it too; a tool that is silent while it works is poor
   developer experience even when the agent reads only the end.
5. **`-- sh -c '…'`** wrapped a command that is a configured suite, because
   the stable binary of the moment could not read the renamed config, and a
   quoted command slipped past the repository's test hook. Both were
   accidents of the session, and both are exactly the kind of drift the
   loop's opacity hides.

The pattern is general: whenever a command cannot express the intent
("run this suite and give me its result", "wait for this run to end"), the
agent builds the missing command out of shell, and the result is less
reliable than either.

## Decision

1. **The intent is one command.**
   - `prova run <suite> --until done` (or `--until first-failure`, `stall`)
     starts the run detached, waits for that condition, and prints the
     summary with the usual exit code. Without `--until`, `run` behaves as
     before (attached, or `--detach`).
   - `prova wait <run> --until done` does the same for a run already going.
2. **The time budget fits the harness, and prova owns it.** A wait for `done`
   lasts up to 9 minutes by default (under the common 10-minute ceiling); a
   wait for the first failure or a stall keeps 30 s. `--max-wait` overrides
   both. When the budget ends first, prova exits 5 and prints the one
   command to continue (`next: prova wait <run> --until done`). Continuing is
   one more call to that command, never a loop.
3. **A long wait shows progress, for people.** While `--until done` waits,
   each new failure, slow test and stall is printed on stderr as it happens,
   and one line every 30 s says how long the run has been going, what passed
   and failed so far, and what is running now. The final summary goes to
   stdout, so an agent that only reads the result is not disturbed; a person
   sees a run that is alive.
4. **Nothing has to be parsed from text.** `--json` prints one object;
   `--detach` prints only the run id; the exit code says passed, failed,
   stalled or still running (0, 1, 3, 5).
5. **The skills teach the single commands** and say plainly: no shell loops,
   pipes or `sleep` around prova. For work longer than a tool call, repeat
   the `next:` command, or run it with the harness's background facility
   when there is one.
6. **When an agent reaches for a loop again, that is a prova bug.** It is
   recorded as a `dogfood` issue with the loop, as AGENTS.md already asks for
   friction.

## Consequences

- The large suite is `prova run large --until done`, then at most one
  `prova wait <run> --until done`: two readable commands, each bounded.
- A wait now blocks a tool call for up to 9 minutes by default when the
  caller asked for `done`; for the first failure it stays short, as 0004
  intends. Liveness is unchanged: a stall or a slow test ends the wait only
  for `--until first-failure` or `stall`, as before.
- Harnesses with a shorter ceiling set `--max-wait` lower; the skills say so.

## Alternatives

- **Longer default waits everywhere:** the first failure would arrive late,
  defeating 0004.
- **Silence until the end:** cheaper to build, but a person watching cannot
  tell a working run from a hung one; the progress lines cost one stderr line
  per event and per 30 s.
- **Leave it to the skills alone:** the skill can forbid loops, but without
  `--until` on `run` and a harness-sized budget the agent has no single
  command to use instead.
