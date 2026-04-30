---
description: Open a new task in task-journal
argument-hint: <title>
---

The user wants to open a new task in the task-journal.

Title (required): $ARGUMENTS

If no title was provided in arguments, ask the user for one in 1 short line. Then call the `task_create` MCP tool with that title. Output the returned `task_id` to the user so they can reference it later (e.g. `tj-x9rz1f`).

Optionally, if the user added context after the title (separated by `--`), pass it as `initial_context`.

Example calls:
- `/task-journal:create Add OAuth login` → `task_create(title="Add OAuth login")`
- `/task-journal:create Pick storage -- between Postgres and SQLite` → `task_create(title="Pick storage", initial_context="between Postgres and SQLite")`
