#!/usr/bin/env bash
# PreToolUse hook (Bash): this repository dogfoods toe, so tests run through
# toe, never directly (AGENTS.md, Dogfooding). Blocks `cargo test` and
# `cargo nextest` unless the command is a toe invocation.
cmd=$(jq -r '.tool_input.command // empty')
# A toe invocation runs cargo itself; that is the allowed path.
if printf '%s' "$cmd" | grep -Eq '(^|[;&|(]|\s)(\S*/)?toe\s+(run|wait|exp)\b'; then
  exit 0
fi
if printf '%s' "$cmd" | grep -Eq '(^|[;&|(`]\s*|\s)cargo(\s+\+\S+)?\s+(test|nextest)\b'; then
  cat >&2 <<'MSG'
Blocked: this repository tests itself with toe (AGENTS.md, Dogfooding).
Use the stable toe instead of running cargo test directly:
  .toe/stable/bin/toe run small [--select <test>]   # unit tests
  .toe/stable/bin/toe run medium                    # integration tests
  .toe/stable/bin/toe run spec                      # full gate (scripts/ci.sh)
  .toe/stable/bin/toe run large                     # fixtures, all languages
If toe itself is broken, say so and open a `dogfood` issue.
MSG
  exit 2
fi
exit 0
