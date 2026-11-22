# Research 02: conduit (MCP versus CLI), skills, deployment topology

Date: 2026-11-22. Web-researched by a subagent. Items marked **[unverified]** come from third-party sources or were not checked.

**Headline:** no harness verified delivers MCP progress or notifications to the model mid-call. Fast feedback must come from short-returning calls (CLI or long-poll), not MCP streaming.

## 1. MCP spec state

Revisions: 2025-06-18, 2025-11-25, **2026-07-28 (current)**.

**2025-11-25** ([changelog](https://modelcontextprotocol.io/specification/2025-11-25/changelog)): experimental tasks (SEP-1686), URL-mode elicitation, sampling with tools, JSON Schema 2020-12 default, input validation errors returned as tool execution errors.

**2026-07-28** ([changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog.md)), breaking redesign:
- **Stateless**: no `initialize` handshake, no session id. Cross-call state is a server-minted handle passed as an ordinary tool argument (exactly a `run_id`).
- **Transports**: stdio and Streamable HTTP. HTTP+SSE deprecated. SSE resumability removed: a broken stream loses the in-flight request.
- **Progress and logging**: still flow, but only on the response stream of their own request. Logging deprecated.
- **Subscriptions**: `resources/subscribe` replaced by one `subscriptions/listen` stream.
- **Structured output**: `outputSchema` accepts any JSON Schema 2020-12.
- **Tasks**: moved out of core into the extension `io.modelcontextprotocol/tasks`. `tools/call` may return a task handle; client uses `tasks/get` (poll), `tasks/update`, `tasks/cancel`.

Stable core: tools, structured output, progress, stdio, Streamable HTTP. Tasks is an optional extension with uneven client support.

## 2. Harness behaviour as MCP clients

| | Claude Code | Codex CLI | Cursor | Gemini CLI |
|---|---|---|---|---|
| Tool timeout | `MCP_TOOL_TIMEOUT`, very long if unset | `tool_timeout_sec` = 60 s | IDE: idle timeout reset by progress; CLI path: hardcoded 60 s **[unverified]** | 600 000 ms per server |
| Output cap | 25 000 tokens (`MAX_MCP_OUTPUT_TOKENS`) | per-tool `output_token_limit` | not found | truncation merged 2026 **[unverified]** |
| Tasks extension | public beta | feature request only **[unverified]** | no evidence | no evidence |

Sources: [Claude Code MCP docs](https://code.claude.com/docs/en/mcp), [Codex MCP docs](https://learn.chatgpt.com/docs/extend/mcp?surface=cli), [Gemini CLI MCP docs](https://raw.githubusercontent.com/google-gemini/gemini-cli/main/docs/tools/mcp-server.md), Cursor forum threads.

Claude Code specifics: idle timeout reset by progress; a call still running after 2 min is auto-backgrounded; tasks polled via `tasks/get`; tool search defers MCP schemas by default.

**Does the model see progress mid-call?** No documentation found in any of the four harnesses that it does. Progress serves timeout keep-alive and, at best, UI. Working assumption: final result only.

**Can the model cancel an in-flight call?** Not in a blocking call (the model is not running). Claude Code can stop a backgrounded task. Cursor has reports of cancel not aborting.

Caveat: Gemini CLI docs carry a notice it was replaced by Antigravity CLI on 2026-06-18. Re-check before targeting.

## 3. Fast-feedback patterns

- **(a) Async start plus long-poll tools.** `run_start` returns `run_id`; `run_wait(run_id, until, max_wait)` returns on next significant event. Works on every MCP client and matches the 2026-07-28 "explicit handle" guidance. Keep `max_wait` at or below about 45 s because of 60 s defaults.
- **(b) CLI with JSONL and exit codes.** Every harness has a shell tool: most portable. In Claude Code, background Bash plus `Monitor` feeds each stdout line back to the model as an event; this is the only verified mechanism that wakes the model per event ([tools reference](https://code.claude.com/docs/en/tools-reference)). Other harnesses use the same CLI in long-poll form.
- **(c) Hooks.** Harness-specific, no cross-harness standard. Optional add-on, not the conduit.

CLI versus MCP evidence (blogs and vendor write-ups, indicative only): CLI cheaper in context and more reliable in one comparison; MCP schema dumps can cost tens of thousands of tokens; tool search/deferred loading erases much of that. Sources: [PubNub](https://www.pubnub.com/blog/mcp-vs-cli-context-window-cost-blocks-ai), [Firecrawl](https://firecrawl.dev/blog/mcp-vs-cli).

## 4. Skills and AGENTS.md

- **Agent Skills is an open standard** at [agentskills.io](https://agentskills.io/home.md), donated to the Agentic AI Foundation ([announcement](https://www.anthropic.com/news/donating-agent-skills-to-the-agentic-ai-foundation)).
- **Format**: folder with `SKILL.md` (frontmatter `name`, `description`), optional `scripts/`, `references/`, `assets/`. Progressive disclosure: metadata, body, files.
- **Paths**: Claude Code `.claude/skills` and plugins; Codex `.agents/skills`; Gemini CLI `.gemini/skills` or `.agents/skills`; Cursor `.cursor/skills` and `.agents/skills` **[unverified]**. `.agents/skills` is the nearest common path.
- **Limits (Claude Code)**: description truncated at 1 536 chars; keep `SKILL.md` under 500 lines; non-standard frontmatter is Claude-only.
- **Distribution**: Claude Code plugin marketplace (git repo with `.claude-plugin/marketplace.json`); Codex plugins; community `skills` CLI.
- **AGENTS.md**: plain Markdown, no schema, read natively by Codex, Cursor, Copilot. Claude Code reads `CLAUDE.md` **[unverified]**; workaround is `CLAUDE.md` containing `@AGENTS.md`.
- **Packaging advice**: one repo; standard-only frontmatter; workflow skills (`toe-test`, `toe-experiment`, `toe-improve`) plus short language skills (`toe-rust`, `toe-go`, `toe-python`, `toe-ts`, `toe-java`, `toe-csharp`) with details in `references/`; ship as a Claude Code plugin, mirror into `.agents/skills` via `toe init`, plus an AGENTS.md snippet.

## 5. Rust

- **`rmcp`** (official, `modelcontextprotocol/rust-sdk`), 3.11.0 at time of research. Transports: stdio, child process, Streamable HTTP. Documents tasks extension, progress, cancellation, stateless serving. README still says "draft"; three major versions in about a year: expect churn.
- Alternatives: `rust-mcp-sdk` 2.0.0; hand-rolled JSON-RPC over stdio is viable for a five-tool facade.

## 6. Container topology

| Topology | Precedent | Trade-off |
|---|---|---|
| Host CLI plus engine in container | Dagger | clean split, but tests then run in containers, not on host toolchains |
| Container mounting workspace and docker socket | Testcontainers-in-container, act **[unverified]** | socket is root-equivalent; path mismatch; host toolchains still invisible |
| Tool inside dev environment | devcontainers **[unverified]** | works only if the system under test already lives there |
| Static host binary, container optional | typical Rust CLI | sees host toolchains, zero path mapping; needs per-OS builds |

The harness shell and the system under test's toolchains are on the host. A container-only toe cannot run `cargo test` or `go test` against host toolchains without a host-side runner.

## Recommendation: CLI core plus thin MCP facade

1. `toe` is a static host binary: runner and event source, JSONL output, documented exit codes. Container image carries toe for containerized workspaces (devcontainer, CI) and any optional long-lived service.
2. Primary conduit is the CLI: `toe run --detach` returns `run_id`; `toe wait <id> --until first-failure|hang|done --max-wait 30s`; `toe events <id> --follow`; `toe cancel <id>`.
3. `toe mcp` is a thin facade (stdio first), about five tools mirroring the CLI, with `outputSchema`, small results, `max_wait` at or below 45 s. Never depend on progress, subscriptions, channels or tasks for correctness.
4. Skills teach the loop (start, wait, fix, rerun) and carry per-language knowledge.

Why not MCP only: 60 s defaults, model-invisible progress, tasks only in one harness. Why not CLI only: MCP adds typed schemas, permission scoping, and reach into shell-less clients at low cost.
