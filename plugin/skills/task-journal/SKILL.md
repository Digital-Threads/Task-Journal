---
name: task-journal
description: Use when the user mentions tracking AI-coding tasks, recovering context for old tasks, recording decisions/findings/evidence with a reasoning chain, or asks "what was I working on?". Suggests appropriate task-journal MCP tools and explains the event lifecycle.
---

# Task Journal — Reasoning Chain Memory

The `task-journal` plugin captures the **logical chain** of an AI-coding task: hypotheses, decisions, rejections, evidence — not just the final code. Two weeks later, `task_pack` returns a Markdown summary that lets the agent (or you) pick up the task with full context restored.

## When to suggest task-journal tools

| User says... | Suggest |
|--------------|---------|
| "Remind me about task X", "what was I working on for X?" | `task_pack` (or `/task-journal:pack`) |
| "Find the task where I decided about Y" | `task_search` (or `/task-journal:search`) |
| "I just decided to use X", "we're going with X" | `event_add` with `event_type=decision` |
| "I'm gonna try X", "what if we used X?" | `event_add` with `event_type=hypothesis` |
| "Tests show X", "benchmark says X" | `event_add` with `event_type=evidence` |
| "We can't do X because Y" | `event_add` with `event_type=rejection` |
| "Wait, the previous claim was wrong" | `event_add` with `event_type=correction` (set `corrects` to the bad event_id) |
| "Let me start a new task: X" | `task_create` (or `/task-journal:create`) |
| "Done with X, shipped" | `task_close` (or `/task-journal:close`) |

## Event types (12 total)

`open`, `hypothesis`, `finding`, `evidence`, `decision`, `rejection`, `constraint`, `correction`, `reopen`, `supersede`, `close`, `redirect`

## Tools available

The plugin's MCP server exposes 5 tools:

- `task_pack(task_id, mode)` — return Markdown resume pack. `mode` is `compact` (~1-2KB, just essentials) or `full` (~5-10KB, all sections).
- `task_create(title, initial_context?)` — open new task, returns `task_id` like `tj-x9rz1f`.
- `event_add(task_id, event_type, text, corrects?, supersedes?)` — append event.
- `task_close(task_id, reason, outcome?)` — close task with reason.
- `task_search(query)` — FTS5 search current project's events; returns task_ids.

## Key invariants

- **Append-only**: events are never edited. To fix a misclassification, write a `correction` event linking back via `corrects: <event_id>`.
- **Confidence-aware**: events from the auto-classifier have a `confidence` field. Below 0.85 → `status=suggested`, rendered with `[?]` marker in `task_pack`. The user is supposed to see these and confirm/correct.
- **Markdown out**: `task_pack.text` is the primary product output. Inject it into the agent's context to restore reasoning state.

## Anti-patterns

- Don't call `task_create` every turn. One task = one logical objective; events accumulate under it.
- Don't paste raw `task_pack` JSON wrapper to the user — render the `text` field as Markdown.
- Don't write rejection without a paired decision earlier (or a clear "we considered X but…" framing).

## Storage

Events live at `$XDG_DATA_HOME/task-journal/events/<project_hash>.jsonl` (Linux/WSL) or platform-equivalent. SQLite cache in `state/<hash>.sqlite` is rebuildable from JSONL via `task-journal rebuild-state`.
