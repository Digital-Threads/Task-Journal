---
name: task-journal
description: |
  MANDATORY for every coding session. Use task-journal to track ALL tasks, bugs, and investigations.
  Create a task at the start, log every significant discovery/decision/error, close when done.
  This is NOT optional — without it, context is lost between sessions and compactions.
  Triggers: beginning of work, significant finding, decision made, hypothesis formed, error found,
  test results obtained, task completed, "what was I working on?", "remind me about X".
---

# Task Journal — Reasoning Chain Memory

**MANDATORY WORKFLOW — follow without exceptions:**

1. **Start any task/bug/investigation** → `task_create` with descriptive title
2. **Every significant discovery** → `event_add` with appropriate type (see below)
3. **Every decision or rejection** → `event_add` (decision/rejection)
4. **Test results, QA outcomes** → `event_add` with `event_type=evidence`
5. **Wrong hypothesis corrected** → `event_add` with `event_type=correction` + `corrects=<event_id>`
6. **Task done** → `task_close` with reason and outcome

## Event type guide — choose the RIGHT one

| Situation | Type | Example |
|-----------|------|---------|
| "I think the bug might be in X" | `hypothesis` | Unverified theory, needs checking |
| "The code shows X does Y at line Z" | `finding` | Verified fact from reading code/logs |
| "Tests pass", "QA verified on staging" | `evidence` | Proof something works or fails |
| "We'll use approach X because Y" | `decision` | Committed choice |
| "Tried X but it won't work because Y" | `rejection` | Explicitly rejected approach |
| "API rate limit is 100/min" | `constraint` | External limitation discovered |
| "Actually, previous finding was wrong" | `correction` | Corrects earlier event (set `corrects` field) |
| "Done, PR merged, verified" | `close` | Task completed |

**Key distinctions:**
- `hypothesis` = "I think" / "maybe" / "could be" → NOT yet verified
- `finding` = "I see" / "the code shows" / "confirmed" → verified by reading code/logs
- `evidence` = ran a test/experiment that PROVES something (set `evidence_strength`: weak/medium/strong)
- `decision` ≠ `hypothesis`: decision = committed; hypothesis = exploring

## Tools available

The plugin's MCP server exposes 5 tools:

- `task_pack(task_id, mode)` — return Markdown resume pack. `mode`: `compact` (~2KB) or `full` (~10KB).
- `task_create(title, initial_context?)` — open new task, returns `task_id` like `tj-x9rz1f`.
- `event_add(task_id, event_type, text, corrects?, supersedes?)` — append event.
- `task_close(task_id, reason, outcome?)` — close task with reason.
- `task_search(query)` — FTS5 search current project's events; returns task_ids.

## When to use task_pack

| User says... | Action |
|--------------|--------|
| "Remind me about task X", "what was I working on?" | `task_pack` |
| "Find the task where I decided about Y" | `task_search` → `task_pack` |
| Session start on existing project | `task_search` for recent open tasks → `task_pack` |

## Key invariants

- **Append-only**: events are never edited. To fix a mistake, write a `correction` event with `corrects: <event_id>`.
- **One task = one logical objective**: don't create a new task every turn. Events accumulate under one task.
- **Always close**: when a task is done, call `task_close`. Don't leave tasks open.
- **Log rejections**: wrong paths are as valuable as correct ones — they prevent repeated mistakes.

## Auto-capture

Hooks are installed via `task-journal install-hooks --scope user`. Auto-classification runs through a two-stage hybrid: a local heuristic catches obvious decisions, rejections, evidence, and findings for free; ambiguous chunks fall back to the Anthropic API when `ANTHROPIC_API_KEY` is set. Without the key, the heuristic still works and uncertain chunks queue for later retry. Manual recording via MCP tools always works as a complement.

## Storage

Events: `$XDG_DATA_HOME/task-journal/events/<project_hash>.jsonl`
State: `state/<hash>.sqlite` (rebuildable via `task-journal rebuild-state`)
