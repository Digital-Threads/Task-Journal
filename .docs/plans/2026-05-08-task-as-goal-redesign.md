# Task-as-Goal Redesign — Plan (v0.4.0)

**Date**: 2026-05-08
**Status**: Draft → awaiting approval before implementation
**Owner**: Mher

## Problem

Current journal records *events*, but a "task" is just whatever
events the classifier piles into the same `task_id`. There is no
explicit **goal**, no explicit **outcome**, no structured
**artifacts**. Reading a closed task in TUI gives a stream of
"updated this file / ran this test", not a narrative.

Concretely: tj-ne9tsb "Тест plugin" accumulated 16 events spanning
two unrelated work sessions. There's no field saying "the goal was
to ship v0.2.11 TUI classifier filter; outcome — done, commit
44b9126". Through 1 month, the user can't reconstruct *why* we did
*what* without re-reading every event by hand.

## Goal of this redesign

After v0.4.0, opening a closed task should answer four questions
without scrolling through events:

1. **Goal** — what was the task trying to achieve?
2. **Decisions** — what approach was chosen and why?
3. **Rejections** — what alternatives were considered and discarded?
4. **Outcome + Artifacts** — what shipped (commit hash, files, PR),
   was the goal met?

## Non-goals (explicitly out of scope)

- AI-generated executive summaries ("describe this task in one
  sentence"). Out of scope; users write their own goal/outcome.
- Task hierarchies (epics/subtasks). Single-level only.
- Cross-project task linking. Out of scope.
- Real-time TUI watcher. Separate item.

## Data model changes

### `tasks` table

Add columns (migration `004_task_goal_outcome.sql`):

```sql
ALTER TABLE tasks ADD COLUMN goal       TEXT;        -- nullable; user-set
ALTER TABLE tasks ADD COLUMN outcome    TEXT;        -- nullable; set on close
ALTER TABLE tasks ADD COLUMN external   TEXT;        -- nullable; "beads:claude-memory-rsw" / "github:#42"
```

Backwards-compat: existing rows get NULLs. Pack rendering treats
NULL goal/outcome as "(not set)". Migration is additive — no data
loss.

### `events_index` / event payload

Add typed `artifacts` JSON column to `events_index`:

```sql
ALTER TABLE events_index ADD COLUMN artifacts TEXT;  -- JSON object
```

`artifacts` shape (extend incrementally):

```json
{
  "commit_hash": "44b9126",
  "commit_message": "fix(tui): hide classifier sessions",
  "files": ["crates/tj-cli/src/tui/app.rs"],
  "pr_url": "https://github.com/.../pull/42",
  "linked_issue": "claude-memory-rsw",
  "test_summary": { "passed": 222, "failed": 0 }
}
```

Stored alongside `event.text` so legacy display still works.

### Event JSON on disk (`*.jsonl`)

Add optional top-level `artifacts` field. `serde_json` flatten
keeps it absent for old entries.

## CLI surface

New flags / subcommands:

```
task-journal create "<title>" --goal "<one-liner>"
task-journal goal <id> "<text>"          # set/update goal post-hoc
task-journal close <id> --outcome "<text>"
task-journal external <id> --add github:#42
task-journal artifacts <id>              # print all extracted artifacts
```

Existing commands unchanged (no breaking removal).

## Classifier prompt changes

The classifier currently returns:
```json
{event_type, task_id_guess, confidence, evidence_strength, suggested_text}
```

Extend to:
```json
{
  "event_type": "...",
  "task_id_guess": "...",
  "confidence": 0.92,
  "evidence_strength": "strong",
  "suggested_text": "...",
  "artifacts": { "commit_hash": "...", "files": [...], ... }
}
```

`artifacts` populated when the chunk text contains:
- `git commit` output → `commit_hash`, `commit_message`
- File path lists ("Modified N files: ...") → `files[]`
- `gh pr create`/PR URLs → `pr_url`
- `claude-memory-XXX` mentions → `linked_issue`
- `cargo test` summaries → `test_summary { passed, failed }`

Failure of artifact extraction is non-fatal — fields stay null.

## Pack rendering (`tj_core::pack::assemble`)

New `Full` mode layout:

```
# <title>  · [status]
**Goal**: <task.goal or "(not set)">
**Outcome**: <task.outcome or "(open)">
**External**: <task.external or "—">

## Lifecycle
- opened  YYYY-MM-DD HH:MM
- closed  YYYY-MM-DD HH:MM

## Artifacts
- commit 44b9126 — "fix(tui): hide classifier sessions"
- files: crates/tj-cli/src/tui/app.rs (+19/-1)
- pr:    —
- linked: claude-memory-bxl

## Decisions
- <list>
## Rejected
- <list>
## Evidence (chronological)
- <list with timestamps>
## Other events
- <findings/hypotheses/etc, less prominent>
```

`Compact` mode shrinks to: title, goal, outcome, top-3 decisions.

## TUI changes

- `task_list` already shows status/title — add goal teaser when set.
- `task_detail` already renders Full pack — no changes needed
  beyond the new pack layout flowing through.

## Migration plan

### Schema
- Migration v004 runs on next `db::open`. Idempotent ALTER ADD
  COLUMNs with defaults (NULL).

### Existing data
- Old tasks: `goal=NULL`, `outcome=NULL`, `external=NULL`. Pack
  shows "(not set)" / "(open)" — informative, not broken.
- Old events: `artifacts=NULL`. Pack falls back to plain text.
- No re-classification of historical events. User can backfill via
  `task-journal goal <id>` ad-hoc.

## Phased delivery

| Phase | Scope | Risk |
|------|-------|------|
| **P1** | Migration v004; CLI flags `--goal`, `goal <id>`, `close --outcome`. No classifier changes; pack renders new sections from existing fields. | Low. Schema only. |
| **P2** | Classifier prompt extracts `commit_hash`, `files`, `linked_issue`. Pack shows Artifacts block. | Medium. Prompt stability + parsing. |
| **P3** | `pr_url`, `test_summary`, `external` linking. `artifacts` subcommand. | Low. |
| **P4** | Auto-open new task on first UserPromptSubmit when no open task exists, title = first 80 chars, `goal` left blank for user to fill. | Medium. UX risk — could spam tasks. Gate behind `--auto-open` flag, default off in P4. |

## Acceptance criteria (v0.4.0 = P1+P2)

- [ ] Migration v004 lands; tests confirm idempotency on existing
      sqlite files.
- [ ] `task-journal create "x" --goal "y"` writes both; `task-journal
      pack` shows them.
- [ ] `task-journal close <id> --outcome "shipped X"` writes outcome;
      pack shows it.
- [ ] Classifier output schema accepts new `artifacts` field; old
      classifiers (no field) still parse cleanly (serde optional).
- [ ] Given a chunk containing `git commit -m "..."` and a hash
      printed via `[main 44b9126]`, classifier returns `commit_hash`
      and `commit_message` in artifacts.
- [ ] Pack Full mode displays `## Artifacts` block when any event
      has artifacts; absent otherwise.
- [ ] Tests: 5+ new (migration idempotent, goal CRUD, outcome on
      close, artifact parsing happy path + missing path, pack
      Artifacts block).

## Open questions (require user input before P1 starts)

1. Should `task-journal create` *require* `--goal`, or allow
   blank-then-fill-later? (Recommend: allow blank, prompt in TUI.)
2. Should `outcome` be free-text or enum (`done`/`abandoned`/
   `superseded`)? (Recommend: free-text + optional enum tag.)
3. P4 auto-open: opt-in via flag, or default-on with heuristic?
   (Recommend: opt-in via env `TJ_AUTO_OPEN_TASKS=1`.)
4. Backfill: should we re-run a one-shot classifier pass over
   existing events to populate artifacts retroactively? (Recommend:
   no — too expensive, opt-in via `task-journal reclassify <id>`.)

## Definition of done

User reopens any closed task in TUI and sees, without scrolling:
- one-line goal
- one-line outcome
- artifacts block (commit + files at minimum)
- decisions / rejections distinct from raw evidence

If a task has none of those (legacy), TUI shows "(not set)"
placeholders so absence is explicit, not silent.
