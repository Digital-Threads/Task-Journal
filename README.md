# Task Journal

[![CI](https://github.com/shahinyanm/claude-memory/workflows/CI/badge.svg)](https://github.com/shahinyanm/claude-memory/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Append-only journal for AI-coding tasks. Captures the *reasoning chain* — goals, hypotheses, decisions, rejections, evidence — and renders a compact resume pack on demand so an agent can pick up a 2-week-old task with full context.

**Why:** existing memory tools store sessions, issues, or do flat semantic search. None store the *logical chain* of a single task. After two weeks, the code remains but the *why* is gone. Task Journal fixes that.

> Task Journal is the first plugin in the `claude-memory` marketplace.

## Install

**Option 1 — from crates.io (recommended)**

```bash
cargo install task-journal-cli task-journal-mcp
```

**Option 2 — pre-built binary**

Download the right archive for your OS/arch from [GitHub Releases](https://github.com/shahinyanm/claude-memory/releases), unpack, put `task-journal` and `task-journal-mcp` somewhere in your `$PATH`.

**Option 3 — build from source**

```bash
git clone https://github.com/shahinyanm/claude-memory
cd claude-memory
cargo install --path crates/tj-cli --path crates/tj-mcp
```

## Quick start

```bash
# Open a task
task-journal create "Add OAuth login"
# → tj-x9rz1f

# Record decisions / findings as you work
task-journal event tj-x9rz1f --type hypothesis --text "PKCE vs implicit grant"
task-journal event tj-x9rz1f --type decision --text "Adopt PKCE flow"
task-journal event tj-x9rz1f --type rejection --text "Implicit grant: deprecated"

# Get a resume pack (Markdown)
task-journal pack tj-x9rz1f --mode full
```

Sample output:

```markdown
# Add OAuth login  [status: open]

## Lifecycle
- 2026-04-30T... opened

## Active decisions
- Adopt PKCE flow

## Rejected
- Implicit grant: deprecated

## Recent events (last 10)
- 2026-04-30T... [rejection] Implicit grant: deprecated
- 2026-04-30T... [decision] Adopt PKCE flow
- 2026-04-30T... [hypothesis] PKCE vs implicit grant
- 2026-04-30T... [open] Add OAuth login
```

## Claude Code integration (two paths)

### Path A — Plugin (recommended)

Adds slash-commands like `/task-journal:create` and `/task-journal:pack`, plus declarative hooks for auto-capture, plus the MCP server, all in one install.

```bash
# In Claude Code (the in-chat slash):
/plugin install /path/to/this/repo/plugin

# OR (if you have a marketplace):
/plugin install task-journal@<your-marketplace>
```

Six slash-commands become available: `/task-journal:create`, `:event`, `:pack`, `:search`, `:close`, `:stats`.

### Path B — Plain MCP (no plugin)

If you don't want slash-commands or auto-capture hooks, just register the MCP server in `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "task-journal": { "command": "task-journal-mcp" }
  }
}
```

Five tools become available: `task_pack`, `task_search`, `task_create`, `event_add`, `task_close`.

## Auto-capture via Claude Code hooks

```bash
export ANTHROPIC_API_KEY=sk-ant-...
task-journal install-hooks --scope user
```

Hooks send chat chunks to Claude Haiku for classification:

- Confidence ≥ 0.85 → confirmed event
- Confidence < 0.85 → suggested event (rendered with `[?]` marker; you decide)

Hook commands are wrapped with `|| true` so a classifier failure (network down, rate limit, missing key) **never** breaks Claude Code.

See [INSTALL.md](./INSTALL.md) for the full hook walkthrough.

## CLI commands

| Command | Purpose |
|---------|---------|
| `task-journal create <title>` | Open a task |
| `task-journal event <id> --type X --text Y` | Append a typed event |
| `task-journal close <id> --reason "..."` | Close a task |
| `task-journal event-correct --corrects <event_id> --task <id> --text "..."` | Correction event |
| `task-journal pack <id> --mode compact\|full` | Render resume pack |
| `task-journal search <query>` | FTS5 search in current project |
| `task-journal search <query> --all-projects` | Search across all projects |
| `task-journal events list` | List events for current project |
| `task-journal rebuild-state` | Rebuild SQLite from JSONL |
| `task-journal stats` | Classifier accuracy + counts |
| `task-journal install-hooks [--scope user\|project]` | Install Claude Code hooks |

## Architecture

`task-journal` is a Rust workspace with three crates:

- **`tj-core`** — event schema (JSONL, append-only), SQLite derived state, pack assembler, classifier client (Anthropic API)
- **`tj-cli`** — `task-journal` binary
- **`tj-mcp`** — `task-journal-mcp` binary (MCP server using `rmcp`)

**Source of truth = JSONL event log.** SQLite is rebuildable from it via `rebuild-state`. Pack output is Markdown wrapped in JSON metadata.

Where data lives:

| OS | Path |
|----|------|
| Linux/WSL | `$XDG_DATA_HOME/task-journal` (default `~/.local/share/task-journal`) |
| macOS | `~/Library/Application Support/task-journal` |
| Windows | `%LOCALAPPDATA%\task-journal` |

Layout:

```
task-journal/
├── events/<project_hash>.jsonl    # source of truth (append-only)
├── state/<project_hash>.sqlite    # derived state (rebuildable)
├── metrics/<project_hash>.jsonl   # classifier telemetry
└── pending/<id>.json              # failed classifications awaiting retry
```

## Design docs

- [`.docs/plans/2026-04-29-tz-task-journal-v2.md`](.docs/plans/2026-04-29-tz-task-journal-v2.md) — original spec
- [`.docs/plans/2026-04-29-task-journal-v1-design.md`](.docs/plans/2026-04-29-task-journal-v1-design.md) — design doc (9 architectural decisions)
- [`.docs/plans/2026-04-29-task-journal-v1-p1-skeleton.md`](.docs/plans/2026-04-29-task-journal-v1-p1-skeleton.md) — P1 plan
- [`.docs/plans/2026-04-30-task-journal-v1-p2-task-pack-core.md`](.docs/plans/2026-04-30-task-journal-v1-p2-task-pack-core.md) — P2 plan
- [`.docs/plans/2026-04-30-task-journal-v1-p3-hooks-classifier.md`](.docs/plans/2026-04-30-task-journal-v1-p3-hooks-classifier.md) — P3 plan
- [`.docs/plans/2026-04-30-task-journal-v1-p4-polish-dogfood.md`](.docs/plans/2026-04-30-task-journal-v1-p4-polish-dogfood.md) — P4 plan

## Development

```bash
cargo test --workspace                 # all unit + integration tests
.beads/hooks/p1-demo.sh                # P1 skeleton smoke
.beads/hooks/p2-demo.sh                # P2 task_pack smoke
.beads/hooks/p3-mock-demo.sh           # P3 hooks/classifier mock smoke
.beads/hooks/p4-demo.sh                # P4 polish smoke
.beads/hooks/p3-demo.sh                # Real Anthropic API (requires ANTHROPIC_API_KEY)
```

License: MIT.
