# Task Journal v1 — Design Document

**Date**: 2026-04-29
**Epic**: `claude-memory-d36`
**Brainstorm task**: `claude-memory-dlq`
**Status**: AWAITING USER APPROVAL
**Source spec**: [`.docs/plans/2026-04-29-tz-task-journal-v2.md`](./2026-04-29-tz-task-journal-v2.md)

---

## 0. Read-First — The Anchor

`task_pack` is the only product output that matters. Every other component (event log, classifier, SQLite cache, MCP tools, hooks) is in service of one capability:

> *Two months later, ask the agent for a task — get a compact text restoring the full reasoning chain (goal, active decisions, rejected options with reasons, evidence, next steps).*

When two design choices conflict, the one that makes `task_pack` better wins.

---

## 1. Architecture Overview

```
                       ┌──────────────────────────────────────────────────┐
                       │                Claude Code session                │
                       │  (main agent — never blocked by Task Journal)    │
                       └──────────────────────────────────────────────────┘
                            │ stdio MCP                       │ hooks
                            │ (task_pack, event_add, ...)     │ (UserPromptSubmit,
                            ▼                                 │  PostToolUse, Stop)
        ┌──────────────────────────────────────┐             │
        │       task-journal MCP server        │  ◀──────────┘
        │            (Rust, rmcp)              │
        │                                      │
        │  ┌──────────┐ ┌─────────────────┐   │
        │  │ 5 MCP    │ │ event writer    │   │
        │  │ tools    │ │ (append-only)   │   │
        │  └──────────┘ └────────┬────────┘   │
        │  ┌──────────────────┐ │             │
        │  │ task_pack        │ │             │
        │  │ assembler        │ │             │
        │  └────────┬─────────┘ │             │
        └───────────┼───────────┼─────────────┘
                    │           │
                    ▼           ▼
              ┌─────────────────────────────────────┐
              │  Storage (per-machine, per-project) │
              │  events/{hash}.jsonl   ← truth      │
              │  state/{hash}.sqlite   ← derived    │
              │  config.toml                        │
              └─────────────────────────────────────┘
                    ▲
                    │ events written async
                    │
        ┌──────────────────────────────┐
        │  classifier subprocess       │
        │  (Claude Haiku 4.5)          │
        │  spawned per-batch, NOT in   │
        │  main loop                   │
        └──────────────────────────────┘
```

**Key invariant:** the JSONL event log is the source of truth. SQLite is rebuildable from it. Any tool can drop SQLite and reconstruct state on next read.

---

## 2. Resolved Design Questions

### Q1. Tech stack — **Rust**

**Decision**: Rust + [`rmcp`](https://docs.rs/rmcp) (official Rust MCP SDK) + Tokio.

**Rationale**:
- Single static binary distribution (no `npm install`, no `pip install` for end users).
- Strong, validated event schema via `serde` + `schemars::JsonSchema`.
- Hook subprocess is fast-startup (no JIT/interpreter warmup) — important since hooks fire frequently and must not slow the main agent.
- `rmcp` provides clean macro API: `#[tool_router]` on impl block, `#[tool(name=..., description=...)]` on async fn.

**Rejected**:
- **TypeScript/Node** — better MCP SDK maturity but loses single-binary story; `npx` startup adds ~200-400ms per hook fire.
- **Python** — fine type hints but `pip install` friction and slower hook startup.

**Tradeoff accepted**: longer compile cycles during dev, mitigated by `cargo watch` + workspace splitting.

---

### Q2. The 5 MCP tools

| # | Tool | Purpose | Primary caller |
|---|------|---------|----------------|
| 1 | `task_pack` | Return resume text for a task in `compact` or `full` mode | agent |
| 2 | `task_search` | Find tasks by query / status / project / time | agent |
| 3 | `task_create` | Open a new task with title + initial context | agent or user via slash-command |
| 4 | `event_add` | Append a typed event to a task | agent, classifier, hook |
| 5 | `task_close` | Close a task with reason + outcome summary | agent or user |

**Folded into `event_add` as event types** (not separate tools): `correction`, `reopen`, `supersede`, `redirect`, `hypothesis`, `finding`, `evidence`, `decision`, `rejection`, `constraint`.

**Rationale**:
- ТЗ caps at 5 tools.
- Verb-noun split is more discoverable than mega-tools dispatched on `action` field.
- "Explicit correction tools first-class" (per ТЗ principle) is preserved by surfacing `correction` events distinctly in `task_pack` with `[corrected]` markers — the *event* is first-class even if the *tool* call is `event_add`.

---

### Q3. `task_pack` shape

**Output format**:
```jsonc
{
  "task_id": "tj-7f3a",
  "mode": "compact" | "full",
  "schema_version": "1.0",
  "text": "<Markdown content here>",
  "metadata": {
    "generated_at": "2026-05-15T12:00:00Z",
    "source_event_count": 42,
    "confidence_floor": 0.85,        // lowest confidence in any event included
    "has_unconfirmed_events": true,  // any classifier-suggested events surfaced
    "cache_hit": false
  }
}
```

The `text` field is Markdown (LLM-friendly, easy to inject). The wrapper exposes provenance metadata so callers can decide whether to trust the pack.

**Two modes**:

| Mode | Target size | What's included |
|------|-------------|-----------------|
| `compact` | ~1-2 KB | Title, status, ONE-LINE goal, active decisions only, top 3 rejections, last 5 events, immediate next step |
| `full` | ~5-10 KB | Everything: lifecycle history, all hypotheses, all decisions (active + superseded), all rejections with rationale, all evidence, all events with confidence markers, refs, suggested next steps, open questions |

**Markdown skeleton (full mode)**:

```markdown
# {Title}  [status: open · age: 14d]

## Goal
{one-paragraph, derived from open + later refinements}

## Lifecycle
- 2026-04-29 opened
- 2026-05-02 → discuss
- 2026-05-08 → decide
- 2026-05-15 reopened (reason: ...)

## Active decisions
- **D1**: Use Rust for MCP server. *Why*: single-binary distribution. *Evidence*: rmcp matures, npx startup blocks hooks.
- **D2**: ...

## Rejected
- ❌ TypeScript — loses single-binary story (D1 conflict)
- ❌ Python — pip install friction

## Evidence & findings
- 🔍 Hook latency measured at 380ms with Node, 12ms with Rust binary (strong)
- 🔍 ...

## Open questions
- ?: How to handle classifier downtime — fail-open or queue?

## Recent events (last 5)
1. 2026-05-14 [decision] adopt schema_version field
2. 2026-05-13 [evidence] benchmark hook startup
3. ...

## Refs
- commits: a3f2dd, b9c1e5
- files: src/event.rs, schema/v1.json

## Suggested next step
{derived from latest open question or unresolved hypothesis}
```

---

### Q4. Event schema

**Top-level JSONL record (one event per line)**:

```jsonc
{
  "event_id": "01HZX5K8...",        // ULID — sortable by time
  "schema_version": "1.0",
  "task_id": "tj-7f3a",
  "type": "decision",                // see enum below
  "timestamp": "2026-05-14T12:00:00+04:00",  // RFC 3339 with offset
  "author": "agent",                 // user | agent | classifier | hook
  "source": "chat",                  // chat | hook | manual | cli
  "confidence": 0.92,                // 0.0-1.0, optional
  "evidence_strength": "strong",     // weak | medium | strong, optional
  "text": "Adopt Rust + rmcp.",
  "refs": {
    "commits": ["a3f2dd"],
    "files": ["Cargo.toml"],
    "events": ["01HZX5J2..."]
  },
  "corrects": null,                  // event_id when type=correction
  "supersedes": null,                // event_id when type=supersede
  "status": "confirmed",             // confirmed | suggested
  "meta": { /* extensible */ }
}
```

**Event types** (closed enum in v1):

| Type | When |
|------|------|
| `open` | Task created |
| `hypothesis` | A working assumption being explored |
| `finding` | Empirical observation |
| `evidence` | Data supporting/refuting a decision |
| `decision` | A binding choice |
| `rejection` | An option considered and rejected (always linked to a decision) |
| `constraint` | An external/non-negotiable requirement |
| `correction` | Fix for a misclassified earlier event (uses `corrects`) |
| `reopen` | Task moved from closed back to open |
| `supersede` | Earlier decision/finding replaced (uses `supersedes`) |
| `close` | Task closed |
| `redirect` | Task renamed or merged into another (target in `meta`) |

**Versioning**: top-level `schema_version: "1.0"`. Read path tolerates older versions; write path always writes the current version. Migrations are read-time projections.

---

### Q5. SQLite schema (derived state)

```sql
-- Tasks projection
CREATE TABLE tasks (
  task_id          TEXT PRIMARY KEY,
  title            TEXT NOT NULL,
  status           TEXT NOT NULL,  -- open | discussing | deciding | closed | superseded
  project_hash     TEXT NOT NULL,
  opened_at        TEXT NOT NULL,
  closed_at        TEXT,
  last_event_at    TEXT NOT NULL
);
CREATE INDEX idx_tasks_project ON tasks(project_hash, last_event_at DESC);

-- Event index for fast filtering
CREATE TABLE events_index (
  event_id     TEXT PRIMARY KEY,
  task_id      TEXT NOT NULL,
  type         TEXT NOT NULL,
  timestamp    TEXT NOT NULL,
  confidence   REAL,
  status       TEXT NOT NULL,  -- confirmed | suggested
  FOREIGN KEY (task_id) REFERENCES tasks(task_id)
);
CREATE INDEX idx_events_task_time ON events_index(task_id, timestamp DESC);

-- Decisions projection (extracted from events for fast pack assembly)
CREATE TABLE decisions (
  decision_id    TEXT PRIMARY KEY,  -- event_id of the decision event
  task_id        TEXT NOT NULL,
  text           TEXT NOT NULL,
  status         TEXT NOT NULL,  -- active | rejected | superseded
  superseded_by  TEXT,
  FOREIGN KEY (task_id) REFERENCES tasks(task_id)
);

-- Evidence projection
CREATE TABLE evidence (
  evidence_id            TEXT PRIMARY KEY,
  task_id                TEXT NOT NULL,
  text                   TEXT NOT NULL,
  strength               TEXT NOT NULL,
  refers_to_decision_id  TEXT,
  FOREIGN KEY (task_id) REFERENCES tasks(task_id)
);

-- task_pack cache (invalidated on event_add to a task)
CREATE TABLE task_pack_cache (
  task_id              TEXT NOT NULL,
  mode                 TEXT NOT NULL,  -- compact | full
  text                 TEXT NOT NULL,
  generated_at         TEXT NOT NULL,
  source_event_count   INTEGER NOT NULL,
  PRIMARY KEY (task_id, mode)
);

-- Full-text search across event text + task titles
CREATE VIRTUAL TABLE search_fts USING fts5(
  task_id UNINDEXED,
  event_id UNINDEXED,
  text,
  type
);
```

**Rebuild policy**: `task-journal rebuild-state` reads the JSONL log start-to-end and recreates SQLite. Should run in <5s for 100k events. Source of truth never moves.

---

### Q6. Classifier

| Aspect | Choice |
|--------|--------|
| **Live model** | Claude Haiku 4.5 (`claude-haiku-4-5-20251001`) — fast, cheap |
| **Batch model** | Claude Sonnet 4.6 — for `task-journal reclassify` recovery jobs |
| **Process** | Subprocess of MCP server, spawned per-batch (debounced ~500ms) |
| **Input** | A text chunk + recent task context (last 5 events from candidate task) |
| **Output** | `{ event_type, task_id_guess, confidence, evidence_strength, suggested_text }` |
| **Threshold** | `confidence >= 0.85` → write as `status: confirmed`. `< 0.85` → `status: suggested`, surfaced in `task_pack` with `[?]` marker. |
| **Failure mode** | Classifier down/error → event NOT written, hook input persisted to `pending/` for later replay. Main agent unaffected. |

**Prompt template** (rough):
```
You are an event classifier for an AI-coding-agent task journal.
Recent task context: {top 3 candidate tasks with last 3 events each}
New text chunk (from {author=user|assistant}): {text}

Decide:
1. Is this related to an existing task? Which one (if any)?
2. What event type best describes this? (one of: hypothesis, finding, evidence, decision, rejection, constraint, correction, ...)
3. What is your confidence? (0.0-1.0)

Respond as strict JSON.
```

**Why subprocess (not in-process)**: classifier failure must not crash the MCP server; LLM calls have variable latency we don't want to block tool calls behind. Subprocess also allows hot-swapping the classifier model without redeploying the MCP server.

---

### Q7. Storage layout

| OS | Base directory |
|----|---------------|
| Linux / WSL | `$XDG_DATA_HOME/task-journal/` (default `~/.local/share/task-journal/`) |
| macOS | `~/Library/Application Support/task-journal/` |
| Windows | `%LOCALAPPDATA%\task-journal\` |

**Directory layout**:
```
task-journal/
├── config.toml                    # global settings (api_key_ref, models, thresholds)
├── events/
│   ├── {project_hash_1}.jsonl     # one append-only log per project
│   ├── {project_hash_2}.jsonl
│   └── ...
├── state/
│   ├── {project_hash_1}.sqlite    # derived per-project; rebuildable
│   └── ...
├── pending/                       # events queued when classifier was unreachable
│   └── {timestamp}-{rand}.json
└── projects.toml                  # project_hash → canonical_path mapping
```

**Project hash**: SHA-256 of the canonical absolute path (resolved via `dunce::canonicalize` to handle Windows weirdness), truncated to 16 hex chars.

**Sync v1**: NONE. Local only. (ТЗ explicitly defers cross-machine sync to v2.)

---

### Q8. Hooks integration

**Hooks used in v1** (Claude Code lifecycle events):

| Hook | Action | Why |
|------|--------|-----|
| `UserPromptSubmit` | Classify user input → potential `hypothesis`, `constraint`, or `redirect` event | Captures user intent at the start |
| `PostToolUse` | Sweep tool result for `finding` candidates (e.g., test output, file content) | Tools produce evidence |
| `Stop` | Sweep last assistant turn for `decision`, `rejection`, `evidence` events | Most decisions emerge in assistant text |
| `SessionStart` | No-op v1 (reserved for classifier warm-up in v2) | Fast session start |

**Strategy**: every hook command spawns the `task-journal` binary in NON-BLOCKING mode (the binary returns immediately after enqueueing, classifier runs async). Main agent never waits on classifier.

**Installer**: `task-journal install-hooks [--scope=user|project]` writes hook entries to:
- `--scope=user` → `~/.claude/settings.json`
- `--scope=project` → `.claude/settings.json`

Idempotent: re-running detects existing hooks and updates only its own entries.

**Disabling**: `task-journal install-hooks --uninstall` removes entries cleanly.

---

### Q9. Phase breakdown (target: 12-16 working days = 2.5-3 weeks)

| Phase | Duration | Deliverables |
|-------|----------|--------------|
| **P1 — Skeleton** | 3-4 days | Cargo workspace, rmcp + tokio + serde + rusqlite deps, event schema (`schemars` derive), JSONL writer (append + fsync policy), SQLite migrations, 5 MCP tools stubbed (return mocks), storage path resolution, project_hash logic, basic CLI (`task-journal create`, `events list`, `rebuild-state`) |
| **P2 — task_pack core** | 3-4 days | `task_pack` assembler (full + compact modes), Markdown rendering, decisions/evidence projections, FTS5 search, real `task_create` / `event_add` / `task_close` / `task_search` impls, golden-fixture tests (curated event sequences → expected pack output), pack cache invalidation |
| **P3 — Hooks + classifier** | 4-5 days | Hook installer (idempotent), classifier subprocess (Anthropic API client, structured JSON output), confidence-gated write path, `suggested` event surfacing in pack, `pending/` retry queue, manual `event_add(type=correction)` UX |
| **P4 — Polish + dogfood** | 2-3 days | Cross-project search, token budget tuning (truncate strategies for full mode), local telemetry (classifier accuracy, suggested-vs-confirmed ratio), README, MCP install instructions, E2E test (real Claude Code session, observe events, simulate 1-day gap, assert pack restores chain) |

**Critical path**: P1 → P2. P3 can start once P2 has working `event_add`. P4 is partial parallel work.

---

## 3. Risks & Open Unknowns

| Risk | Mitigation |
|------|-----------|
| Classifier accuracy too low at confidence ≥ 0.85 — pack gets noisy | Build correction UX first (P3), measure base rate on dogfood data, tune threshold per event type if needed |
| Hook latency interferes with main agent | NON-BLOCKING subprocess + benchmark in P3 (target: <50ms hook return time) |
| `task_pack` exceeds token budget for long tasks | Hard cap per mode + truncation strategy (drop oldest evidence, keep all decisions) |
| Cross-machine sync (v2) requires schema-stable events | Locked v1 schema with `schema_version` field; migrations are read-time projections |
| Project_hash collisions on path normalization differences | Document constraint: same canonical path → same hash; rebuild-state can re-key |

---

## 4. Out of Scope (v1 → v2 candidates)

- Cloud sync / cross-machine
- Beads bidirectional integration (read-only might fit v1.5)
- Web UI for browsing tasks
- Multi-user / team mode
- Auto-capture of all agent tool calls (currently selective via hooks)
- Discussion compaction through main Claude (explicitly rejected per ТЗ)

---

## 5. Self-Review

- ✅ **Placeholder scan**: no TBD or TODO inside resolved sections.
- ✅ **Internal consistency**: 5 tools (Q2) match the diagram (§1) and the storage layout (Q7) supports JSONL + SQLite as described.
- ✅ **Scope check**: phases 1-4 are estimable, total 12-16 days fits ТЗ "2-3 weeks". No subsystem feels like a hidden second project.
- ✅ **Ambiguity check**: each tool has a single primary caller; each event type has clear write trigger; classifier confidence threshold is a single number with explicit semantics.

---

## 6. Approval Gate

This document is the contract for v1. Once approved:
1. The implementation plan (`writing-plans` skill) decomposes Phase 1 into ≤5-min tasks tracked in beads under epic `claude-memory-d36`.
2. No code is written without an approved plan AND a failing test (TDD per project rules).
3. The TZ v2 (`.docs/plans/2026-04-29-tz-task-journal-v2.md`) is updated to reference this design doc as its canonical architecture answer.

**Awaiting**: user review. Reply with "approved" to move to writing-plans, or list changes needed.
