#!/usr/bin/env bash
# PreToolUse hook (Bash): this repository dogfoods prova, so tests run through
# prova, never directly (AGENTS.md, Dogfooding). Blocks `cargo test` and
# `cargo nextest` unless the command is a prova invocation.
raw=$(jq -r '.tool_input.command // empty')
# Only the shell code counts: drop heredoc bodies and quoted strings, so
# commit messages or PR text that mention `cargo test` are not blocked.
cmd=$(printf '%s\n' "$raw" | awk '
  heredoc { if ($0 ~ "^[[:space:]]*" tag "$") heredoc = 0; next }
  { line = $0
    if (match(line, /<<-?[[:space:]]*[\047"]?[A-Za-z_]+/)) {
      tag = substr(line, RSTART, RLENGTH); gsub(/<<-?[[:space:]]*[\047"]?/, "", tag); heredoc = 1 }
    print line }' | sed -E "s/'[^']*'//g; s/\"([^\"\\\\]|\\\\.)*\"//g")
# prova's output is the answer: compact, failures first, and a `next:` line.
# Piping it through grep, head, tail and the like drops warnings, notes and
# `next:` (ADR 0019: one command per intent, no output parsing).
# (`2>&1` is a redirection, not a command separator.)
if printf '%s\n' "$cmd" | sed -E 's/[0-9]*>&[0-9-]+//g' \
  | grep -Eq '(^|[;&(`]|\s|\$\()(\S*/)?prova\s+[a-z_]+[^;&]*[^|]\|[^|]'; then
  cat >&2 <<'MSG'
Blocked: prova's output is piped or filtered (AGENTS.md, Dogfooding; ADR 0019).
Run the prova command on its own and read all of it: it is already compact,
failures first, and it ends with the `next:` command to run. Filtering drops
warnings, notes and `next:`. Need one thing? Ask prova for it:
  prova status <run>            the result, warnings, failures
  prova output <run> --tail N   the run's output
  prova history <test>          one test
  prova ... --json              structured, read whole
If prova's output is genuinely too long or lacks what you need, that is a
prova bug: open a `dogfood` issue with the command.
MSG
  exit 2
fi
# A prova invocation runs cargo itself; that is the allowed path.
if printf '%s' "$cmd" | grep -Eq '(^|[;&|(]|\s)(\S*/)?prova\s+(run|wait|exp)\b'; then
  exit 0
fi
if printf '%s' "$cmd" | grep -Eq '(^|[;&|(`]\s*|\s)cargo(\s+\+\S+)?\s+(test|nextest)\b'; then
  cat >&2 <<'MSG'
Blocked: this repository tests itself with prova (AGENTS.md, Dogfooding).
Use the stable prova instead of running cargo test directly:
  .prova/stable/bin/prova run small --until first-failure [--select <test>]
  .prova/stable/bin/prova run medium --until done
  .prova/stable/bin/prova run spec --until done         # full gate (scripts/ci.sh)
  .prova/stable/bin/prova run large --until done        # fixtures; repeat `next:` once
If prova itself is broken, say so and open a `dogfood` issue.
MSG
  exit 2
fi
exit 0
