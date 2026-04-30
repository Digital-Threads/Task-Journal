---
description: Close a task in task-journal with a reason
argument-hint: <task_id> [reason]
---

The user wants to close a task.

Arguments: $ARGUMENTS

Parse: first token = task_id, remainder = reason (optional but strongly recommended).

If no reason provided, ask the user one short line: "What outcome closed this task?".

Call the `task_close` MCP tool with `task_id` and `reason`.

After close, optionally render `task_pack` in compact mode so the user sees the final state.

Example: `/task-journal:close tj-x9rz1f shipped to production`
