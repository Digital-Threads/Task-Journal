# Installation

## Prerequisites

- **Rust toolchain 1.83+** — `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- (Optional, for auto-capture) **`ANTHROPIC_API_KEY`** exported in your shell

## Build & install

```bash
git clone <this repo>
cd claude-memory
cargo build --release --workspace
cargo install --path crates/tj-cli
cargo install --path crates/tj-mcp
```

This installs two binaries to `~/.cargo/bin/`:

- `task-journal` — CLI
- `task-journal-mcp` — MCP server

Verify:

```bash
task-journal --version
task-journal-mcp --help 2>/dev/null || true   # MCP server speaks JSON-RPC over stdin
```

## MCP server (Claude Code)

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "task-journal": { "command": "task-journal-mcp" }
  }
}
```

Restart Claude Code. The 5 tools (`task_pack`, `task_search`, `task_create`, `event_add`, `task_close`) become available to the agent.

## Auto-capture hooks

Install once for your user:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
task-journal install-hooks --scope user
# → /home/<you>/.claude/settings.json
```

Or per-project:

```bash
cd /path/to/project
task-journal install-hooks --scope project
# → .claude/settings.json
```

The installer:

- Preserves all unrelated keys in `settings.json` (e.g. `theme`, other MCP servers).
- Wraps the hook command with `|| true` so a classifier failure (network down, rate limit, missing key) **never** breaks Claude Code.
- Failures land in `<data-dir>/pending/<id>.json` and are replayed on next successful ingest.

- Gives each hook the timing it needs: the capture hooks run `async` so the chat never waits on them, and `SessionEnd` carries an explicit `timeout` — without one Claude Code cancels it after 1.5 seconds and the tail of the session is lost.

> **Note**: the hook payload arrives as JSON on stdin (`hook_event_name`, `source`, `cwd`, `transcript_path`, …). There are no `$CLAUDE_HOOK_*` environment variables — an older template used them and always fed the classifier empty text.

## Codex

Codex speaks the same hook protocol, so the same journal works there:

```bash
codex mcp add task-journal -- task-journal-mcp
task-journal install-hooks --scope user --client codex
# → /home/<you>/.codex/hooks.json
```

`--client codex` writes to Codex's own hooks file and trims the wiring to what Codex supports: its `SessionEnd` budget caps at 3 seconds, and it has no `PostModelSwitch` event. Everything else — the resume pack on `SessionStart`, the nudge, the background capture — behaves the same.

## Verify install

```bash
task-journal create "Test task"
# → tj-xxxxxx
task-journal event tj-xxxxxx --type decision --text "Adopt my plan"
task-journal pack tj-xxxxxx --mode full
```

You should see Markdown output with the title and the `[decision]` event.

## Uninstall hooks

```bash
task-journal install-hooks --scope user --uninstall
# add --client codex to remove them from ~/.codex/hooks.json
```

This removes only the `hooks` block from `settings.json` (keeps theme, other servers, everything else).

## Where data lives

| OS | Path |
|----|------|
| Linux/WSL | `$XDG_DATA_HOME/task-journal` (default `~/.local/share/task-journal`) |
| macOS | `~/Library/Application Support/task-journal` |
| Windows | `%LOCALAPPDATA%\task-journal` |

Inside:

```
task-journal/
├── events/<project_hash>.jsonl    # append-only event log (source of truth)
├── state/<project_hash>.sqlite    # derived state (rebuildable from JSONL)
├── metrics/<project_hash>.jsonl   # classifier telemetry
└── pending/<id>.json              # failed classifications awaiting retry
```

To reset one project's state: `rm <data-dir>/state/<hash>.sqlite` (regenerated on next read).

To wipe entire journal: `rm -rf <data-dir>`.

## Diagnostics

| Issue | Check |
|-------|-------|
| Hook didn't write anything | `ls <data-dir>/events/` — JSONL exists? |
| `[?]` markers everywhere | Classifier confidence consistently below 0.85; consider `task-journal stats` for accuracy ratio |
| Pack is huge | Check `metadata.truncated` flag; pack is capped at 10 KB (full) / 2 KB (compact) |
| `ANTHROPIC_API_KEY env var not set` | Hook runs with same env as Claude Code; export the key in your shell rc file |
| `pending/` growing | Real classifier is failing; inspect `<data-dir>/pending/*.json` for last error |

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `task-journal-mcp: command not found` | Не сделал `cargo install`, либо `~/.cargo/bin` не в PATH. Проверь `which task-journal-mcp`. |
| Plugin виден, но MCP tools не подцепились | Перезапусти Claude Code полностью (закрыть, открыть). |
| Hook ничего не пишет в `events/` | `ANTHROPIC_API_KEY` не выставлен — экспортируй в `~/.bashrc`. |
| Pack возвращает "task not found" | Сначала `task-journal create`, потом `pack` — порядок важен. Или после нескольких ингестов запусти `task-journal rebuild-state`. |
