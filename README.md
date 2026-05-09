# Task Journal

[![Crates.io](https://img.shields.io/crates/v/task-journal-cli.svg)](https://crates.io/crates/task-journal-cli)
[![CI](https://github.com/Digital-Threads/Task-Journal/workflows/CI/badge.svg)](https://github.com/Digital-Threads/Task-Journal/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/Digital-Threads/Task-Journal/branch/main/graph/badge.svg)](https://codecov.io/gh/Digital-Threads/Task-Journal)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

**The "why" memory for Claude Code.**

Two weeks after a coding session, the code remembers *what* you did. You don't remember *why*. Why did you reject the simpler approach? What did the failing test prove? What did you decide before the meeting interrupted you?

Task Journal records every hypothesis, decision, rejection, and piece of evidence as you (and Claude) work, then plays it back as a compact briefing the next time you open the task — so a fresh Claude session picks up with full context, not a blank slate.

## Demo

You ask Claude to fix a bug. While you work, Task Journal silently records the reasoning chain:

```
You:    "the auth middleware drops the token on refresh"

Claude: <investigates, finds the bug>
        ↓ recorded automatically:
        finding   — "src/auth/refresh.rs:42 — uses < instead of <=, off-by-one on expiry"
        decision  — "fix with <= and a regression test (issue #1284)"
        evidence  — "added test_token_refresh_boundary; previously failing, now green"
        artifacts — commit a3f81c2, file src/auth/refresh.rs, issue #1284
```

Two weeks later, in a fresh session, you ask:

```
You:    "remind me about the auth refresh fix"

Claude: <calls task_pack(tj-9k2x4z)>

# tj-9k2x4z — Fix auth middleware token refresh

**Goal**: Stop dropping the token on refresh boundary
**Outcome [done]**: Off-by-one fixed in src/auth/refresh.rs; regression test added.

## Decisions
- fix with `<=` and a regression test (issue #1284)

## Findings
- src/auth/refresh.rs:42 — uses `<` instead of `<=`, off-by-one on expiry

## Evidence
- added test_token_refresh_boundary; previously failing, now green

## Artifacts
- commits: a3f81c2
- files:   src/auth/refresh.rs
- issues:  #1284
```

No re-reading the diff. No re-explaining what you tried. The reasoning is right there.

## Install

**Claude Code plugin (recommended).** One command, no manual setup:

```bash
claude plugin install github:Digital-Threads/Task-Journal
```

Then install the Rust binaries that the plugin calls into:

```bash
cargo install task-journal-cli task-journal-mcp
```

That's it. Restart Claude Code, start working, and the journal fills itself.

**Alternative installs:** [pre-built binaries](https://github.com/Digital-Threads/Task-Journal/releases), `cargo install` only (manual MCP wiring), or build from source — see [Manual Setup](#manual-setup).

## How it works

- **Auto-capture via Claude Code hooks.** Every prompt, tool call, and Claude reply is classified by a small `claude -p` invocation and appended as a typed event (`finding` / `decision` / `evidence` / `rejection` / …). Hook returns in <100 ms — classification happens in the background, never blocks your session.
- **Artifact extraction.** Each event scans its text for commit hashes, PR URLs, file paths, issue IDs, and branch names. Aggregated artifacts are how Task Journal links related tasks: when you start a new task touching the same issue or file, the prior task is surfaced automatically.
- **Resume packs.** `task_pack` (MCP tool or CLI) renders a task into a compact Markdown briefing — Goal, Outcome, decisions, rejections, evidence, artifacts — that fits in a fresh agent's context window without dumping the raw event log.

Source of truth is an append-only JSONL log per project. SQLite holds derived state and is fully rebuildable. Nothing is sent off-machine except the classifier prompt to your own `claude -p` (subscription) or the Anthropic API.

## Daily use (no manual commands needed)

With the plugin installed, the typical flow is:

1. Just work. Hooks fire on every prompt and tool use.
2. When you want context back: ask Claude *"remind me about the X work"* — it calls `task_search` + `task_pack` and you get the full reasoning chain.
3. When you finish a task, tell Claude *"close this task — outcome: …"* — it calls `task_close`.

The MCP server ships with built-in instructions that nudge Claude through this workflow.

## Manual CLI (power users)

For scripted use, CI, or when you want explicit control:

```bash
# Open a task with an explicit goal
task-journal create "Add OAuth login" \
  --goal "Users sign in via Google/GitHub with PKCE; refresh tokens persisted server-side"
# => tj-x9rz1f

# Record events manually
task-journal event tj-x9rz1f --type decision --text "Adopt PKCE flow"

# Close with outcome
task-journal close tj-x9rz1f \
  --outcome "PKCE shipped behind feature flag oauth_v2" \
  --outcome-tag done

# Resume later
task-journal pack tj-x9rz1f --mode full
```

### All commands

| Command | What it does |
|---------|--------------|
| `create <title> [--goal "..."]` | Open a task with optional goal |
| `goal <id> "..."` | Set or replace a task's goal |
| `event <id> --type X --text Y` | Append a typed event |
| `event-correct --corrects <eid> --task <id> --text "..."` | Correct an earlier event |
| `external <id> "..."` | Append an external reference (URL, ticket, linked task) |
| `close <id> --outcome "..." --outcome-tag done\|abandoned\|superseded` | Close with outcome |
| `reopen <id> --reason "..."` | Reopen a closed task |
| `pack <id> --mode compact\|full` | Render a resume pack |
| `events list [--limit N]` | List recent events |
| `search <query> [--all-projects]` | Full-text search (FTS5) |
| `stale [--days N]` | List open tasks idle >N days |
| `reclassify <id>` | Re-extract artifacts from a task's events |
| `pending list \| retry` | Inspect or retry queued classifier failures |
| `pending-gc [--days N]` | GC stale pending entries |
| `export [--format md\|json] [--task <id>]` | Export to stdout |
| `backfill` | Import events from existing Claude Code session history |
| `ui` / `tui` | Interactive terminal UI |
| `stats` | Classifier accuracy + event counts |
| `doctor` | Self-check the install |
| `rebuild-state` | Rebuild SQLite from JSONL |
| `migrate-project` | Re-key data when a project moves on disk |
| `install-hooks [--scope user\|project]` | Wire Claude Code auto-capture hooks |

## MCP tools

The MCP server exposes five tools to Claude Code (and any MCP client):

| Tool | Purpose |
|------|---------|
| `task_create` | Open a task with optional goal |
| `event_add` | Append a typed reasoning event |
| `task_pack` | Render a resume pack |
| `task_search` | Full-text search across events |
| `task_close` | Close with outcome and outcome tag |

## Configuration

| Env var | Effect | Default |
|---------|--------|---------|
| `TJ_CLASSIFIER_CLI` | Classifier command. When set to bare `claude`, Task Journal automatically injects `--strict-mcp-config --mcp-config '{"mcpServers":{}}'` so the inner classifier session does not load every plugin in your settings (keeps classifier startup fast). | `claude` |
| `TJ_CLASSIFIER_MODEL` | Model alias for the classifier (`claude -p` or HTTP API). | `haiku` (CLI) / `claude-haiku-4-5-20251001` (API) |
| `ANTHROPIC_API_KEY` | Required for `--backend=api` (HTTP classifier). | _unset_ |
| `TJ_AUTO_OPEN_TASKS` | Set to `0` / `false` to disable auto-opening a task from `UserPromptSubmit` when no open task exists. | `1` |

## Event types

| Type | Meaning |
|------|---------|
| `open` | Task created with a title and optional goal |
| `hypothesis` | Unverified theory ("I think X might cause Y") |
| `finding` | Verified observation from reading code, logs, or docs |
| `evidence` | Test/experiment result that proves something |
| `decision` | Committed choice with rationale |
| `rejection` | Explicitly rejected approach with reason |
| `constraint` | External limitation discovered |
| `correction` | Corrects an earlier event (references its event ID) |
| `reopen` | Reopens a previously closed task |
| `supersede` | Replaces an earlier event |
| `close` | Task completed with outcome and outcome tag |
| `redirect` | Task re-routed to a different task |

## Manual Setup

### Without the plugin

Install the binaries:

```bash
cargo install task-journal-cli task-journal-mcp
```

Wire the MCP server into Claude Code (`~/.claude/settings.json`):

```json
{
  "mcpServers": {
    "task-journal": { "command": "task-journal-mcp" }
  }
}
```

Wire auto-capture hooks (one-shot):

```bash
task-journal install-hooks --scope user
```

### Updating

Plugin install:

```
/plugin marketplace update task-journal
/plugin update task-journal@task-journal
```

Then refresh the binaries (the plugin doesn't bundle them):

```bash
cargo install task-journal-cli task-journal-mcp --force
```

Restart Claude Code and verify:

```bash
task-journal --version       # 0.6.3
task-journal-mcp --version   # 0.6.3
```

If you installed from source:

```bash
git pull && cargo install --path crates/tj-cli --path crates/tj-mcp --force
```

## Architecture

Rust workspace with three crates:

| Crate | Package | Description |
|-------|---------|-------------|
| `tj-core` | `task-journal-core` | Event schema, JSONL storage, SQLite derived state, pack assembler, classifier client, artifact extractor |
| `tj-cli` | `task-journal-cli` | `task-journal` CLI binary |
| `tj-mcp` | `task-journal-mcp` | `task-journal-mcp` MCP server (built on `rmcp`) |

**Source of truth = JSONL event log.** SQLite is derived and rebuildable via `rebuild-state`. Pack output is Markdown wrapped in JSON metadata.

### Data Location

| OS | Path |
|----|------|
| Linux / WSL | `$XDG_DATA_HOME/task-journal` (default `~/.local/share/task-journal`) |
| macOS | `~/Library/Application Support/task-journal` |
| Windows | `%LOCALAPPDATA%\task-journal` |

```
task-journal/
  events/<project_hash>.jsonl              # source of truth (append-only)
  state/<project_hash>.sqlite              # derived state (rebuildable)
  state/classifier-<project_hash>.lock     # in-flight classify-worker lock
  metrics/<project_hash>.jsonl             # classifier telemetry
  pending/<id>.json                        # queued events awaiting classification
```

Each project is identified by a hash of its canonical path, so multiple projects share the same data directory without collision.

## Development

```bash
cargo test --workspace
```

Smoke test scripts live in `.beads/hooks/` (used by phased dev work — see [CONTRIBUTING.md](CONTRIBUTING.md)).

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for release notes.

## Contributing

Pull requests welcome — read [CONTRIBUTING.md](CONTRIBUTING.md) first. File bugs and feature requests via the [issue templates](.github/ISSUE_TEMPLATE/). All participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

MIT
