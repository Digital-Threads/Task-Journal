---
description: Append a typed event to a task in task-journal
argument-hint: <task_id> <event_type> <text>
---

The user wants to append a typed event to an existing task.

Arguments: $ARGUMENTS

Parse the arguments as: first token = task_id (e.g. `tj-x9rz1f`), second token = event_type (one of: hypothesis, finding, evidence, decision, rejection, constraint, correction, reopen, supersede, redirect), and the remainder = text body.

If parsing is ambiguous or arguments missing, ask the user for the missing piece(s).

Call the `event_add` MCP tool:
- `task_id`: parsed task id
- `event_type`: parsed event type (must be one of the 11 above; correction needs a `corrects` field, ask if not provided)
- `text`: the event body
- `corrects`: only if event_type is `correction`
- `supersedes`: only if event_type is `supersede`

Print the returned `event_id` so the user can later correct it.

Example: `/task-journal:event tj-x9rz1f decision Adopt PKCE flow over implicit grant`
