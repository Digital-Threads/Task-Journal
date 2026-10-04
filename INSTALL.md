# Installation

Task Journal is one Rust core (the `task-journal` CLI and the `task-journal-mcp`
server) plus thin integrations for each agent:

| Client | What you install | What it gets |
|--------|------------------|--------------|
| Claude Code 2.1.287+ | the plugin (MCP server + skill + mod) | journal tools, the session's task kept in the system prompt, catch-up before compaction, status line |
| Claude Code, older | the plugin, optionally `install-hooks` | journal tools, resume packs on session start |
| Codex | `codex mcp add` + `install-hooks --client codex` | journal tools, resume packs, optional auto-capture |
| Any other MCP client | the MCP server | the seven journal tools |

## Prerequisites

- **Rust 1.88+** — `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- No API key is needed. Self-tagging through the MCP tools is free; the optional
  LLM-backed features reuse your logged-in `claude` or `codex` CLI, or
  `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` if you prefer.

## Install the binaries

```bash
cargo install task-journal-cli task-journal-mcp
```

or from a clone:

```bash
git clone https://github.com/Digital-Threads/Task-Journal
cd Task-Journal
cargo install --path crates/tj-cli
cargo install --path crates/tj-mcp
```

This installs `task-journal` (CLI) and `task-journal-mcp` (MCP server) into
`~/.cargo/bin/`. Check with `task-journal --version`.

## Claude Code

```bash
claude plugin install github:Digital-Threads/Task-Journal
```

The plugin brings the MCP server, the `task-journal` skill and, on Claude Code
2.1.287 or newer, the **Task Journal mod**. Restart Claude Code after installing.

### The mod (Claude Code 2.1.287+)

The mod runs inside Claude Code and needs no setup. It:

- keeps the session's active task (id, title, goal) in the system prompt, so a
  compaction can't lose it, and asks the compaction summary to keep its id;
- gives every session its own active task, even with several sessions open in
  one project;
- shows the task in the status line and a short toast when an entry is recorded;
- reminds the agent to log only after several prompts without an entry, instead
  of on every prompt;
- right before a compaction, asks the model which decisions, rejections and
  findings were never logged and records them as `suggested` events (one extra
  model call per compaction, billed to your plan, mostly served from the prompt
  cache).

Two options live in `/config` (or `claude plugin configure task-journal`):
`nudge_after_turns` (default 6, 0 turns the reminder off) and
`distill_on_compact` (default on).

The mod calls the `task-journal` CLI, so the binaries must be installed and
current (0.30+). If they aren't, it says so once and stays off.

**Classic hooks next to the mod.** If you ran `task-journal install-hooks` with
`--auto-capture` before, you can keep it: the mod tells those hooks it is active
(`TJ_MOD_ACTIVE=1`) and they stand down, except the session-start resume pack and
the model-switch record. To remove them anyway:
`task-journal install-hooks --scope user --uninstall`.

### Older Claude Code, or without the plugin

Wire the MCP server yourself in `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "task-journal": { "command": "task-journal-mcp" }
  }
}
```

and, for resume packs at session start, install the hooks:

```bash
task-journal install-hooks --scope user             # → ~/.claude/settings.json
task-journal install-hooks --scope project          # → .claude/settings.json
task-journal install-hooks --scope user --auto-capture   # also classify prompts and tool calls in the background
```

The default install adds only a read-only `SessionStart` hook (resume packs) and
a `UserPromptSubmit` reminder. `--auto-capture` adds background classification
of prompts, tool calls and session ends; it is a safety net under self-tagging,
not a replacement, and with `--backend hybrid` (the default) it may spawn
`claude -p` for chunks the local heuristic can't classify.

The installer keeps every unrelated key in the settings file, wraps each hook in
`|| true` so a failure never breaks the session, runs the capture hooks `async`
so the chat never waits, and gives `SessionEnd` an explicit timeout.

> The hook payload arrives as JSON on stdin. There are no `$CLAUDE_HOOK_*`
> environment variables.

## Codex

```bash
codex mcp add task-journal -- task-journal-mcp
task-journal install-hooks --scope user --client codex    # → ~/.codex/hooks.json
```

Codex asks before each MCP tool call. To let the agent journal without a prompt
every time, approve the server's tools in `~/.codex/config.toml`:

```toml
[mcp_servers.task-journal]
command = "task-journal-mcp"
default_tools_approval_mode = "approve"
```

In `codex exec` (non-interactive) an unapproved MCP call is refused outright, so
this setting is required there.

Codex sends its session id with every tool call, so events recorded from Codex
are bound to their session like Claude Code's: each Codex session resumes its own
task. `--client codex` trims the hook wiring to what Codex supports (its
`SessionEnd` budget is 3 seconds and it has no `PostModelSwitch` event). `dream`,
`backfill` and `complete --enrich` read Claude Code transcripts only.

## Other MCP clients

Point the client at the `task-journal-mcp` command (stdio). The server's
built-in instructions describe the workflow, and the seven tools work the same:
`task_create`, `event_add`, `artifact_add`, `task_close`, `task_pack`,
`task_search`, `task_check`. The project is the server's working directory; start
it with `--project-dir <path>` if the client launches it elsewhere. A client that
knows its session id can pass `session_id` to the write tools so each session
keeps its own active task.

## Verify

```bash
task-journal create "Test task" --goal "Check the install"
# → tj-xxxxxxxxxx
task-journal event tj-xxxxxxxxxx --type decision --text "Adopt my plan"
task-journal pack tj-xxxxxxxxxx --mode full
task-journal doctor
```

## Uninstall hooks

```bash
task-journal install-hooks --scope user --uninstall
# add --client codex to remove them from ~/.codex/hooks.json
```

This removes only task-journal's own hook entries (and its old
`TJ_CLASSIFIER_CLI` env key) from `~/.claude/settings.json`, or from
`~/.codex/hooks.json` with `--client codex`. Everything else stays. With
`--scope project` the file is `.claude/settings.json` or `.codex/hooks.json` in
the current directory.

## Where data lives

| OS | Path |
|----|------|
| Linux/WSL | `$XDG_DATA_HOME/task-journal` (default `~/.local/share/task-journal`) |
| macOS | `~/Library/Application Support/task-journal` |
| Windows | `%LOCALAPPDATA%\task-journal` |

`TASK_JOURNAL_DATA_DIR` overrides it. Inside:

```
task-journal/
├── events/<project_hash>.jsonl          # append-only event log (source of truth)
├── state/<project_hash>.sqlite          # derived state (rebuildable from the log)
├── metrics/<project_hash>.jsonl         # classifier telemetry
└── pending/<project_hash>.<id>.json     # auto-capture chunks waiting for the classifier
```

To reset one project's state, delete `state/<hash>.sqlite`; it is rebuilt from
the log on the next read, goals and external links included. To wipe the whole
journal: `rm -rf <data-dir>`.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `task-journal-mcp: command not found` | `cargo install` didn't run, or `~/.cargo/bin` isn't on PATH (`which task-journal-mcp`). |
| The plugin is listed but the tools are missing | Restart Claude Code completely. |
| "the task-journal CLI 0.30+ was not found" in the transcript | The mod needs the binaries: `cargo install task-journal-cli task-journal-mcp --force`. |
| Codex says an MCP call "requires approval" | Add `default_tools_approval_mode = "approve"` to the server's block in `~/.codex/config.toml`. |
| `pending/` keeps growing | Auto-capture's classifier is failing; `task-journal pending list` shows the last error, `task-journal pending retry` retries. |
| A pack says "task not found" | The id is wrong or from another project; `task-journal search ""` lists this project's tasks. |
| A pack looks cut | Packs are capped at 2 KB (compact) and 32 KB (full); `task_pack` reports `truncated`. |
