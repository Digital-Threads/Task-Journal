# Task Journal

[![Crates.io](https://img.shields.io/crates/v/task-journal-cli.svg)](https://crates.io/crates/task-journal-cli)
[![CI](https://github.com/Digital-Threads/Task-Journal/workflows/CI/badge.svg)](https://github.com/Digital-Threads/Task-Journal/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Reasoning chain memory for AI coding sessions.

Task Journal captures the *logical chain* of a coding task -- goals, hypotheses, decisions, rejections, evidence, corrections -- as an append-only event log. When you come back to a two-week-old task, the code is still there but the *why* is gone. Task Journal preserves it.

Unlike session-based memory tools that store raw chat history or flat semantic search, Task Journal records structured reasoning events tied to individual tasks, and renders compact resume packs so an agent (or you) can pick up exactly where work left off.

## Installation

**From crates.io (recommended)**

```bash
cargo install task-journal-cli task-journal-mcp
```

**As a Claude Code plugin**

```bash
claude plugin install github:Digital-Threads/Task-Journal
```

**Pre-built binary**

Download the right archive for your OS/arch from [GitHub Releases](https://github.com/Digital-Threads/Task-Journal/releases), unpack, and place `task-journal` and `task-journal-mcp` somewhere in your `$PATH`.

**From source**

```bash
git clone https://github.com/Digital-Threads/Task-Journal
cd Task-Journal
cargo install --path crates/tj-cli --path crates/tj-mcp
```

## Quick Start

```bash
# 1. Create a task
task-journal create "Add OAuth login"
# => tj-x9rz1f

# 2. Record reasoning events as you work
task-journal event tj-x9rz1f --type hypothesis --text "PKCE vs implicit grant"
task-journal event tj-x9rz1f --type decision   --text "Adopt PKCE flow"
task-journal event tj-x9rz1f --type rejection   --text "Implicit grant: deprecated by RFC"

# 3. Resume later with a context pack
task-journal pack tj-x9rz1f --mode full
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `create <title>` | Create a new task (writes an `open` event) |
| `event <id> --type X --text Y` | Append a typed event to a task |
| `close <id> [--reason "..."]` | Close a task |
| `event-correct --corrects <eid> --task <id> --text "..."` | Append a correction referencing an earlier event |
| `events list [--limit N]` | List events for the current project (most recent first) |
| `search <query> [--all-projects]` | Full-text search across events (FTS5) |
| `pack <id> --mode compact\|full` | Render a resume pack for a task |
| `rebuild-state` | Rebuild SQLite derived state from the JSONL log |
| `stats` | Show classifier accuracy and event counts |
| `export [--format md\|json] [--task <id>]` | Export tasks to stdout as Markdown or JSON |
| `backfill [--dry-run] [--limit N]` | Import events from existing Claude Code session history |
| `ui` / `tui` | Interactive terminal UI for browsing sessions |
| `install-hooks [--scope user\|project]` | Install Claude Code auto-capture hooks |
| `ingest-hook` | Hook entry point (called by Claude Code hooks) |

### Export

The `export` command writes task data to stdout so you can pipe it to a file or another tool:

```bash
# Export all tasks as Markdown
task-journal export > report.md

# Export a specific task as JSON
task-journal export --format json --task tj-x9rz1f > task.json

# Export from a different project directory
task-journal export --project /path/to/project
```

## TUI

The interactive terminal UI (`task-journal ui` or `task-journal tui`) lets you browse Claude Code sessions and read conversation history for the current project. Navigate sessions with arrow keys and inspect individual chat messages.

```bash
task-journal ui
task-journal ui --project /path/to/project
```

## MCP Integration

Task Journal ships an MCP server (`task-journal-mcp`) that exposes five tools to Claude Code and other MCP-compatible agents:

| MCP Tool | Description |
|----------|-------------|
| `task_create` | Create a new task |
| `event_add` | Append a reasoning event |
| `task_pack` | Render a resume pack for context restoration |
| `task_search` | Full-text search across events |
| `task_close` | Close a task with a reason |

**Plugin install (recommended)** -- the plugin registers the MCP server automatically:

```bash
claude plugin install github:Digital-Threads/Task-Journal
```

**Manual MCP registration** -- add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "task-journal": { "command": "task-journal-mcp" }
  }
}
```

The MCP server includes built-in instructions that guide Claude Code through the recommended workflow: search for open tasks at session start, record findings/decisions/rejections during work, and close tasks when done.

## Auto-Capture via Hooks

Claude Code hooks can automatically classify chat chunks and record events without manual `event` commands:

```bash
task-journal install-hooks --scope user
```

The classifier (powered by `claude -p` with your Pro/Max subscription, or the Anthropic API with `--backend=api`) analyzes each chat turn and appends events to the active task:

- Confidence >= 0.85 -- confirmed event (auto-recorded)
- Confidence < 0.85 -- suggested event (marked with `[?]`)

Hook commands are wrapped with `|| true` so classifier failures (network down, rate limit) never break Claude Code. Failed classifications are queued in `pending/` and retried on the next ingest.

### Configuration

| Env var | Effect | Default |
|---------|--------|---------|
| `TJ_CLASSIFIER_MODEL` | Model alias passed to `claude -p` (subscription backend) or to the Anthropic API. | `haiku` (CLI) / `claude-haiku-4-5-20251001` (API) |
| `ANTHROPIC_API_KEY`   | Required for the `--backend=api` HTTP classifier. | _unset_ |

## Event Types

| Type | Meaning |
|------|---------|
| `open` | Task created with a title and optional context |
| `hypothesis` | Unverified theory ("I think X might cause Y") |
| `finding` | Verified observation from reading code, logs, or docs |
| `evidence` | Result of a test or experiment that proves something |
| `decision` | Committed choice with rationale ("Use X because Y") |
| `rejection` | Explicitly rejected approach with reason |
| `constraint` | External limitation discovered (rate limits, API restrictions) |
| `correction` | Corrects an earlier event (references `corrects` event ID) |
| `reopen` | Reopens a previously closed task |
| `supersede` | Replaces an earlier event (references `supersedes` event ID) |
| `close` | Task completed with outcome summary |
| `redirect` | Task redirected to a different task or approach |

## Architecture

Task Journal is a Rust workspace with three crates:

| Crate | Package | Description |
|-------|---------|-------------|
| `tj-core` | `task-journal-core` | Event schema, JSONL storage, SQLite derived state, pack assembler, classifier client |
| `tj-cli` | `task-journal-cli` | `task-journal` CLI binary |
| `tj-mcp` | `task-journal-mcp` | `task-journal-mcp` MCP server binary (uses `rmcp`) |

**Source of truth = JSONL event log.** SQLite state is derived and fully rebuildable via `rebuild-state`. Pack output is Markdown wrapped in JSON metadata.

### Data Location

| OS | Path |
|----|------|
| Linux / WSL | `$XDG_DATA_HOME/task-journal` (default `~/.local/share/task-journal`) |
| macOS | `~/Library/Application Support/task-journal` |
| Windows | `%LOCALAPPDATA%\task-journal` |

```
task-journal/
  events/<project_hash>.jsonl    # source of truth (append-only)
  state/<project_hash>.sqlite    # derived state (rebuildable)
  metrics/<project_hash>.jsonl   # classifier telemetry
  pending/<id>.json              # failed classifications awaiting retry
```

Each project is identified by a hash of its canonical path, so multiple projects share the same data directory without collision.

## Development

```bash
cargo test --workspace
```

Smoke test scripts are available in `.beads/hooks/`:

```bash
.beads/hooks/p1-demo.sh          # P1 skeleton smoke
.beads/hooks/p2-demo.sh          # P2 task_pack smoke
.beads/hooks/p3-mock-demo.sh     # P3 hooks/classifier mock smoke
.beads/hooks/p4-demo.sh          # P4 polish smoke
```

## License

MIT
