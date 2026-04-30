---
description: Render a resume pack for a task in task-journal
argument-hint: <task_id> [compact|full]
---

The user wants to see the resume pack — a Markdown summary of the task's reasoning chain.

Arguments: $ARGUMENTS

Parse: first token = task_id, optional second token = mode (`compact` ~1-2KB or `full` ~5-10KB). Default mode: `compact`.

Call the `task_pack` MCP tool with `task_id` and `mode`.

Render the returned `text` field directly to the user (it's already valid Markdown). If `metadata.truncated` is true, mention this. If `metadata.cache_hit` is true, mention "served from cache" parenthetically.

Examples:
- `/task-journal:pack tj-x9rz1f` → compact mode
- `/task-journal:pack tj-x9rz1f full` → full mode
