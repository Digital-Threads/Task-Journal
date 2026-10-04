---
name: task-journal
description: |
  MANDATORY for every coding session. You — the agent — are the journal's primary
  recorder. Do NOT rely on automatic capture; it only catches keyword-obvious lines
  and misses real reasoning, especially in non-English prose. Instead, call the MCP
  tools yourself at the decisive moments below.
  Open a task with an explicit goal at the start, append a typed event the moment you
  decide / reject / discover / prove something, and close with a written outcome so the
  resume pack comes out clean: Goal -> Decisions -> Outcome. Keep the project
  chronicle too: every task belongs to modules (parts of the system), each with a history.
  Triggers: start of any task/bug/investigation; a choice is committed; an approach is
  ruled out; a fact is verified from code or logs; a test/benchmark proves something; an
  earlier belief turns out wrong; task finished; "what was I working on?", "remind me
  about X", "почему мы сделали так?"; "map the project", "what do we know about <module>?".
---

# Task Journal — Reasoning Chain Memory (self-tagging first)

You are the recorder. The point of this journal is the **"why" layer**: weeks later the
code shows *what* changed, not *why*. Capture the decisions and the outcome as they
happen, in your own words, terse and specific. Automatic capture is a weak fallback —
treat it as if it does nothing and record explicitly.

## Session-start ritual (do this before real work)

1. `task_search(status="open")` (add `query=<a few words about the work>` to narrow) — is
   there an open task for this? Each entry in `tasks` shows its title and goal. If yes,
   `task_pack(task_id)` and continue it. **Do not** open a duplicate.
2. If nothing fits, `task_create(title=<short>, goal=<one sentence: what the user is trying to accomplish>)`. **Always pass `goal`** — it is the first line of every pack and
   the anchor for "why was this done?". Pass `modules=[...]` when you know the part of the
   system it touches (see Chronicle below).
3. Hold the returned `task_id` for the whole task. One task = one logical objective.
   Events accumulate under it; do not spawn a new task per turn.

## The decisive moments — when you MUST call `event_add`

Call `event_add(task_id, event_type, text, ...)` the moment one of these happens. Don't
batch it to the end; record at the point of commitment while the reasoning is fresh.

| The moment | `event_type` | What goes in `text` (1–2 terse sentences, specifics) |
|------------|--------------|------------------------------------------------------|
| You commit to an approach / architecture choice | `decision` | The choice + the because. Include file/lib/IDs. |
| You rule an approach out | `rejection` | What you tried + why it won't work. Prevents repeat work. |
| You verify a fact from code/logs | `finding` | The fact + where (file:line, config key). |
| A test / benchmark / repro proves something | `evidence` | What ran + the result (numbers, pass/fail). |
| You hit an external limit | `constraint` | The limit (rate, version, platform). |
| An earlier event turns out wrong | `correction` | The correction. Set `corrects=<event_id>`. |
| This task replaces another | `supersede` | Set `supersedes=<task_id>`. |
| You form an unverified theory worth tracking | `hypothesis` | "maybe X because Y" — only if you'll act on it. |

Capture in the user's language; keep it short and concrete. A good event is one line a
human can read cold in two weeks and understand.

### Decisions: record the alternatives you weighed

For a `decision`, pass the structured `alternatives` array so the considered options and
the final pick are explicit (this is decision-only; it errors on other types):

```
event_add(
  task_id="tj-x9rz1f",
  event_type="decision",
  text="Use fd-lock for the cross-platform JSONL file lock.",
  alternatives=[
    {"option": "fd-lock", "chosen": true,  "rationale": "single API across Win/Unix, maintained"},
    {"option": "rustix flock", "chosen": false, "rationale": "more code, manual Windows path"},
    {"option": "advisory-only", "chosen": false, "rationale": "races under 5–6 parallel CC instances"}
  ]
)
```

### Corrections never edit — they append

Events are append-only. To fix a wrong earlier event, write a `correction` with
`corrects=<event_id>` (the prior call returned that id). Same for `supersede`.

## Closing — REQUIRED, and this is what makes the pack clean

When the objective is met (or abandoned), call `task_close`:

```
task_close(
  task_id="tj-x9rz1f",
  reason="off-by-one fixed, regression test green, PR #1284 merged",
  outcome="Token no longer dropped on refresh boundary; covered by test_token_refresh_boundary.",
  outcome_tag="done"   # done | abandoned | superseded
)
```

- `outcome` is the human-readable result line that renders as **Outcome [tag]** in the pack.
  Without it the pack has no conclusion and reads like raw log. Always write it.
- `outcome_tag` must be `done`, `abandoned`, or `superseded`.
- Don't leave tasks open. An open task with no close = no Outcome line = the exact "I see
  noise, no signal" failure this skill exists to prevent.

## Reading back — `task_pack` / `task_search`

| User says | Action |
|-----------|--------|
| "remind me about task X", "what was I working on?" | `task_pack(task_id, mode="compact")` |
| "find where I decided about Y" | `task_search(query="Y", event_type="decision")` → `task_pack` |
| Resuming an existing project | `task_search(status="open")` → `task_pack` on the match |

`task_pack` mode: `compact` (~2KB, default for resume) or `full` (~10KB, full trail).
`task_search` filters: `status` (`open` | `closed` | `any`, default `any`), `project`
(absolute path of another project directory), `event_type` (decision/finding/evidence/…).
An empty or absent `query` lists the tasks, newest activity first. The result has
`results` (task ids) and `tasks` — the same ids in the same order, each with `title`,
`status`, `last_event_at` and `goal`.

## Chronicle — modules and their history

The project is a map of **modules**: parts of the system by meaning — not folders, not
tickets. Every task belongs to one or more modules, and each module keeps a page: how it
works now (`state`), what is decided, rejected and constrained there across all its tasks,
and every task in order. Six months later, `module_page` is how you learn a part of the
system before touching it.

- **Start:** `task_create(..., modules=["stars"])`. Not sure which? The reply carries
  `suggested_modules` — link with `module_link`, or create a module with `module_save`.
  Read `module_page(module_id)` before you change a module.
- **Done:** `task_close(..., module_notes=[{"module": "stars", "text": "ranking by Wilson score"}])`
  — one line per module on what changed. If the module works differently now, rewrite its
  `state` with `module_save(module_id, state=...)`.
- **Gaps:** a line starting with `📚 Chronicle:` (session start, `task_create`, `module_list`)
  names what the chronicle is missing — no map, tasks without a module, a module whose
  state lags behind its tasks. Close it once the current work is done.

### Mapping a project (no map yet, or `/task-journal:map`)

1. `module_list` — extend an existing map rather than starting over.
2. Study the project: layout, README and docs, the key code paths.
3. Propose 5–15 modules: id (`stars`, `auth-refresh`), name, one to three sentences,
   `hints.paths` (code path prefixes) and `hints.terms` (the words people use for it).
4. Show the list to the user and **confirm** it; adjust from the answer.
5. `module_save` each confirmed module.
6. `module_backfill_candidates(limit=50)`, page by page (`offset` steps past a page): assign
   each old task to modules by its title, goal, outcome, files and the suggestions; list the
   doubtful ones separately.
7. Show the assignment to the user, **confirm**, then one `module_link` call per page. Tasks
   that fit no module go to a catch-all module (`other`, "Other") once the user agrees, so
   the chronicle has no loose ends.
8. For each module with tasks, read its `module_page` and write its first `state`.

Chronicle tools:

- `module_list()` → `modules` (id, name, description, task_count, last_activity) + `gaps`.
- `module_page(module_id)` → `text`: the module's page in Markdown.
- `module_save(module_id, name?, description?, hints?{paths, terms}, state?, status?, merged_into?)`
  — partial update; a new module needs `name`. `status`: active | retired | merged.
- `module_link(links=[{task_id, add?: [...], remove?: [...]}])` — many tasks at once.
- `module_backfill_candidates(limit?, offset?)` → `total_unlinked`, `candidates` (id, title,
  goal, outcome, files, suggestions).

## The 5 MCP tools (exact params)

- `task_create(title, goal?, initial_context?, parent?, modules?)` → `task_id` like `tj-x9rz1f`. **Pass `goal`.**
- `event_add(task_id, event_type, text, corrects?, supersedes?, alternatives?)` → `event_id`.
  `event_type` ∈ hypothesis | finding | evidence | decision | rejection | constraint |
  correction | reopen | supersede | redirect. `alternatives` is decision-only.
- `task_close(task_id, reason, outcome?, outcome_tag?, module_notes?)` — **always pass `outcome` + `outcome_tag`.**
- `task_pack(task_id, mode?)` — `compact` | `full`.
- `task_search(query?, status?, project?, event_type?, module?)` — full-text search over the project's
  events; no `query` lists its tasks. Returns `results` (ids) and `tasks` (id, title, status, goal).

## Invariants

- **Append-only.** Never edit; correct via a new `correction`/`supersede` event.
- **One task = one objective.** Don't fragment a single goal across many tasks.
- **Record at the moment, not at the end.** Decisions logged after the fact get lost.
- **Rejections are as valuable as decisions** — they stop you re-walking dead ends.
- **Close with an outcome, every time.**

## Why explicit (not automatic)

The hybrid auto-classifier only fires confidently on English keyword patterns; ambiguous
or non-English prose needs an LLM it can't reach without an API key (and post-2026-06-15
that path consumes a separate credit pool). You, the agent in the live session, already
know what you decided — so record it directly. That is both free (it rides the interactive
session) and higher fidelity than any after-the-fact classifier. Auto-capture stays on as a
backstop; do not depend on it.
