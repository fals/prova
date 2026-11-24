# 04: Where coding-agent harnesses find instructions, skills and MCP servers

Checked against each vendor's own documentation or repository on
2026-11-24. "Unconfirmed" means the primary source did not say. This decides
what `prova init` writes.

## Changes worth knowing

- Windsurf is now Devin Desktop / Devin Local (docs.windsurf.com redirects
  to docs.devin.ai).
- Google Jules is read-only since 2026-11-11 and closes 2026-12-31; Google
  points to Antigravity.
- Gemini CLI's free and AI Pro/Ultra users moved to Antigravity CLI (`agy`)
  on 2026-06-18; Gemini CLI is still maintained.
- Roo Code's repository was archived in May 2026.
- Kimi CLI (MoonshotAI/kimi-cli) is archived; its successor is Kimi Code CLI
  (MoonshotAI/kimi-code). Kimchi is a separate harness by Cast AI
  (getkimchi/kimchi), built on pi's SDK.
- pi moved from badlogic/pi-mono to earendil-works/pi.
- Claude Code reads AGENTS.md (v2.1.277+) only when no CLAUDE.md,
  `.claude/CLAUDE.md` or CLAUDE.local.md exists in the working directory or
  above; it never reads `.agents/`.

## By harness

| Harness | Instructions | Skills (project) | Project MCP | Sources |
|---|---|---|---|---|
| Claude Code | CLAUDE.md or .claude/CLAUDE.md; AGENTS.md only without CLAUDE.md (or via `@AGENTS.md`) | `.claude/skills` only | `.mcp.json` | code.claude.com/docs/en/memory.md, skills.md, mcp.md |
| Codex CLI | AGENTS.override.md, AGENTS.md, fallback names | `.agents/skills` | `.codex/config.toml` (trusted projects) | developers.openai.com/codex/guides/agents-md.md, codex/skills.md, codex/mcp.md |
| GitHub Copilot (coding agent, CLI) | .github/copilot-instructions.md, .github/instructions/, AGENTS.md, CLAUDE.md, GEMINI.md | `.github/skills`, `.claude/skills`, `.agents/skills` | CLI: `.mcp.json` or `.github/mcp.json`; cloud agent: repository settings | docs.github.com/en/copilot (about-agent-skills, add-repository-instructions, copilot-cli add-mcp-servers) |
| Copilot in VS Code | .github/copilot-instructions.md or AGENTS.md | `.github/skills` (`.agents/skills` per GitHub's doc only) | `.mcp.json` (guided flow) | code.visualstudio.com/docs/copilot/customization |
| Cursor | AGENTS.md, `.cursor/rules` | `.agents/skills`, `.cursor/skills`, `.claude/skills`, `.codex/skills` | `.cursor/mcp.json` | cursor.com/docs/context/skills, rules, mcp |
| Gemini CLI | GEMINI.md (AGENTS.md only if configured) | `.gemini/skills`, `.agents/skills` | `.gemini/settings.json` | github.com/google-gemini/gemini-cli docs/cli/skills.md, gemini-md.md |
| Antigravity | AGENTS.md or GEMINI.md, `.agents/rules` | `.agents/skills` | `.agents/mcp_config.json` | antigravity.google/docs/rules, skills, mcp |
| OpenCode | AGENTS.md (CLAUDE.md fallback) | `.opencode/skills`, `.claude/skills`, `.agents/skills` | `opencode.json` | opencode.ai/docs/rules, skills, mcp-servers |
| Amp | AGENTS.md (AGENT.md, CLAUDE.md fallback) | `.agents/skills`, `.claude/skills` | `.amp/settings.json` | ampcode.com/docs/markdown/customize |
| Kimi Code CLI | AGENTS.md, `.kimi-code/AGENTS.md` | `.kimi-code/skills`, `.agents/skills` | `.kimi-code/mcp.json` | moonshotai.github.io/kimi-code |
| Kimchi (Cast AI) | AGENTS.md, else CLAUDE.md | `.kimchi/skills`, `.cursor/skills`, `.claude/skills` (`.agents/skills` unconfirmed) | `.kimchi/mcp.json` | github.com/getkimchi/kimchi |
| pi | AGENTS.override.md, AGENTS.md or CLAUDE.md | `.pi/skills`, `.agents/skills` | `.pi/mcp.json` (trusted) | github.com/earendil-works/pi docs |
| Goose | AGENTS.md, `.goosehints` | `.agents/skills`, `.goose/skills`, `.claude/skills` | global config only (project unconfirmed) | goose-docs.ai |
| Devin Desktop (Windsurf) | AGENTS.md, `.devin/rules`, legacy `.windsurf/rules` | `.devin/skills`, `.agents/skills`, `.windsurf/skills` | `.devin/mcp_config.json` | docs.devin.ai/desktop/devin-local/customizations.md |
| Cline | `.clinerules`, `.cline/rules`, AGENTS.md | `.cline/skills`, `.clinerules/skills`, `.claude/skills` (not `.agents/skills`) | global only | docs.cline.bot |
| Roo Code (archived) | AGENTS.md, `.roo/rules` | `.roo/skills`, `.agents/skills` | `.roo/mcp.json` | roocodeinc.github.io/Roo-Code |
| Zed | first match only: `.rules`, `.cursorrules`, `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md`, AGENT.md, AGENTS.md, CLAUDE.md, GEMINI.md | `.agents/skills` | settings `context_servers` | github.com/zed-industries/zed docs/src/ai |
| Aider | nothing automatic (`--read AGENTS.md`) | none | none | aider.chat/docs/usage/conventions.html |
| Factory Droid | AGENTS.md, CLAUDE.md | `.factory/skills`, `.agents/skills` | `.factory/mcp.json` | docs.factory.com/harness |
| Kiro | `.kiro/steering`, AGENTS.md | `.kiro/skills` only | `.kiro/settings/mcp.json` | kiro.dev/docs |
| JetBrains Junie | `.junie/AGENTS.md`, AGENTS.md | `.junie/skills` only | `.junie/mcp/mcp.json` | junie.jetbrains.com/docs |
| Warp | AGENTS.md (WARP.md) | `.agents/skills` and most others' directories | `.warp/.mcp.json`, also `.mcp.json` | docs.warp.dev |

## What `prova init` writes, and why

- **Skills in `.agents/skills` and `.claude/skills`, always.** The first
  reaches Codex, Copilot, Cursor, Gemini CLI, Antigravity, OpenCode, Amp,
  Kimi Code, pi, Goose, Devin, Roo, Zed, Factory and Warp; Claude Code,
  Cline and Kimchi read only the second.
- **`.kiro/skills`, `.junie/skills`** only when `.kiro/` or `.junie/`
  exists: those harnesses read no other directory, and the project uses
  them.
- **The prova section in AGENTS.md**, always, saying where the skills are,
  for harnesses that see AGENTS.md but not the skills.
- **The same section in instruction files that already exist** and would
  otherwise keep a harness from AGENTS.md: CLAUDE.md or .claude/CLAUDE.md
  (unless it imports `@AGENTS.md`), GEMINI.md, and Zed's first-match files.
  None is created: a new `.github/copilot-instructions.md` would hide
  AGENTS.md from Zed, and Copilot reads AGENTS.md anyway.
- **No MCP file.** About 15 different project paths exist and the skills
  drive the CLI; `.mcp.json` at the root would reach the most harnesses if
  one is ever wanted.
- **Aider** reads nothing by itself: `aider --read AGENTS.md`.
