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
# A prova invocation runs cargo itself; that is the allowed path.
if printf '%s' "$cmd" | grep -Eq '(^|[;&|(]|\s)(\S*/)?prova\s+(run|wait|exp)\b'; then
  exit 0
fi
if printf '%s' "$cmd" | grep -Eq '(^|[;&|(`]\s*|\s)cargo(\s+\+\S+)?\s+(test|nextest)\b'; then
  cat >&2 <<'MSG'
Blocked: this repository tests itself with prova (AGENTS.md, Dogfooding).
Use the stable prova instead of running cargo test directly:
  .prova/stable/bin/prova run small [--select <test>]   # unit tests
  .prova/stable/bin/prova run medium                    # integration tests
  .prova/stable/bin/prova run spec                      # full gate (scripts/ci.sh)
  .prova/stable/bin/prova run large                     # fixtures, all languages
If prova itself is broken, say so and open a `dogfood` issue.
MSG
  exit 2
fi
exit 0
