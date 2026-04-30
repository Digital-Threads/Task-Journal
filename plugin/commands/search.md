---
description: Full-text search across task-journal events (FTS5)
argument-hint: <query> [--all-projects]
---

The user wants to search across task-journal events.

Arguments: $ARGUMENTS

Parse: query text + optional `--all-projects` flag (search across every project on this machine, not just the current one).

Call the `task_search` MCP tool with `query`. The `task_search` tool searches the current project by default. (Cross-project search via `--all-projects` is currently CLI-only; if user passed `--all-projects`, mention this and run `task-journal search <query> --all-projects` via Bash instead.)

Render the result list as a bullet list. Each result is a `task_id`. Offer to follow up with `/task-journal:pack <id>` for any interesting hit.

Examples:
- `/task-journal:search OAuth` → search current project
- `/task-journal:search rmcp --all-projects` → cross-project (Bash)
