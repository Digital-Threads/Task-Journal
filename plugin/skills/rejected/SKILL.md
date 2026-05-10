---
description: Cross-task lookup of rejection events by topic — surfaces approaches that were already turned down so the agent doesn't repeat them.
---

The user (or an agent acting on their behalf) is about to commit to an
approach and wants to confirm nobody has already rejected it. Wraps the
`task-journal rejected <topic>` CLI.

Arguments: $ARGUMENTS

Parse: a free-form `topic` plus optional flags:
- `--all-projects` — query every project on this machine, not just cwd
- `--limit N` — cap result count (default 20)
- `--since N` — restrict to events newer than N days

Run via Bash:

```
task-journal rejected "$TOPIC" [--all-projects] [--limit N] [--since N]
```

Each result block looks like:

```
tj-x9rz   2026-04-10  "Implicit grant — deprecated by RFC 9700"
          (in task: Add OAuth login)
```

If the result list is empty, say so explicitly — a silent zero-line
output is a footgun (looks like the command failed). Offer to follow
up with `/task-journal:pack <task_id>` for any interesting hit so the
user can read the surrounding context.

Examples:
- `/task-journal:rejected oauth` → matches in current project
- `/task-journal:rejected "session keys" --all-projects --limit 5`
- `/task-journal:rejected pkce --since 30` → last 30 days only
