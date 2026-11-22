# 0009: Conduit: CLI core plus thin MCP facade

Status: Proposed. **Deviates from the README**, which names MCP as the likely conduit while asking to check its constraints.

## Context

The conduit must give fast, event-driven feedback. Findings ([02](../research/02-conduit-skills-deployment.md)):

- No harness checked documents delivering MCP progress or notifications to the model during a tool call. The model sees the final result only.
- Default MCP tool timeouts are as low as 60 s in some harnesses.
- The MCP tasks extension (async calls) is supported by one harness, in beta.
- MCP 2026-07-28 is stateless: cross-call state is an explicit handle passed as an argument, which is the same shape as a run id.
- Every harness has a shell tool. At least one can stream a background command's stdout lines to the model as events.

So the fast path cannot rely on MCP streaming. It must rely on calls that return quickly.

## Decision

1. **The CLI is the primary conduit.**

   ```
   toe run <suite> [--detach]         # start; prints run id
   toe wait <run> --until first-failure|stall|done --max-wait 30s
   toe events <run> --follow          # JSONL stream, for harnesses that can monitor stdout
   toe cancel <run>
   toe status | report | history | diff
   toe exp ...                        # experiments, same pattern
   ```

   Output is compact, structured (JSON with `--json`, short text otherwise), with documented exit codes ([0004](0004-liveness-and-fail-fast.md)). `toe run` without `--detach` is `run --detach` followed by `wait` loops, for humans and simple cases.

2. **`toe mcp` is a thin facade** over the same engine: stdio transport, about five tools (`run_start`, `run_wait`, `run_status`, `run_cancel`, `run_report`) with output schemas and a `max_wait` capped below 45 s. It never depends on progress notifications, subscriptions, or the tasks extension for correctness. Built after the CLI is proven.

3. **Results are small by default**: failures first, passing tests summarised as counts, full detail on request by run id.

## Consequences

- Works in every harness with a shell, today.
- One long-poll pattern (`wait`) serves both CLI and MCP.
- MCP users get typed tools and permission scoping without a second implementation.
- MCP SDK churn is isolated in one small module.

## Alternatives

- MCP only: blocked by timeouts and model-invisible progress.
- CLI only: loses shell-less clients; the facade is cheap.
- Harness hooks as conduit: harness-specific; possible optional add-on.
