# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.0]

### Added
- Close gate: closing a task now surfaces its completeness gaps (from
  `completeness::assess`) — CLI prints them to stderr, MCP `task_close` returns
  them in a new `completeness_gaps` field. Non-blocking: the close always
  succeeds.
- Capture-completeness flagging: a task's resume-pack now shows a `Completeness`
  section listing structural gaps (closed without outcome, decisions without
  evidence, unconfirmed suggested events, missing goal, unclassified pending
  entries) — shown only when gaps exist. Read-only; reusable
  `completeness::assess` API for the upcoming close-gate.
- `task-journal dream` — offline memory backfill (Pass A). Re-reads session
  transcripts and appends significant typed events the realtime classifier
  missed, stamped `source=dream`, `status=suggested` (visible, prunable).
  Manual trigger; `--dry-run`, `--since`, `--task`, `--limit`. Reuses the
  Anthropic HTTP backend via `TJ_DREAM_MODEL` / `TJ_DREAM_MAX_TOKENS`.
  Additive — the JSONL source of truth is never mutated.
- Subtask hierarchy: tasks can have a `parent_id`. Set at creation via
  `task-journal create --parent <id>` or the MCP `task_create` `parent` param
  (validated: parent must exist, no cycles). A parent's resume-pack rolls up its
  direct children (status, id, title). `list --tree` renders the hierarchy.
  Closing a parent with open children warns but proceeds. Additive — existing
  flat tasks are unaffected (`parent_id` NULL).

## [0.11.1] - 2026-06-08

**Fix: `pack` panicked on multibyte UTF-8.** Pack truncation sliced the
rendered text at a raw byte index, panicking ("byte index N is not a char
boundary") whenever the budget cutoff landed inside a multibyte character —
i.e. on Cyrillic/CJK/emoji-heavy journals that exceed the pack budget.
ASCII-only content was unaffected, so it stayed latent. Truncation now cuts
at a UTF-8 char boundary.

### Fixed
- `tj_core::pack` truncation is now char-boundary-safe (`truncate_to_budget`);
  packs with non-ASCII text exceeding the budget no longer panic.

## [0.11.0] - 2026-06-08

**Live `session_id` on emitted events (additive, opt-in).** The journal now
stamps the active Claude Code session id onto the events it emits itself —
hook-driven events (synchronous FileChanged/PreCompact and the async
classify-worker path) and the MCP tools (`task_create`, `event_add`,
`task_close`). This lets external consumers correlate journal events with
the originating session without time-window heuristics.

Fully backward-compatible: the id is read from the hook payload's
`session_id` field, falling back to the `CLAUDE_CODE_SESSION_ID` env var.
When neither is present (standalone use), nothing is added and behavior is
byte-identical to before. This is distinct from the existing transcript
`session_id` *parsing* — that passive read-only lookup is unchanged.

### Added
- `tj_core::session_id` — helpers to resolve the live session id
  (`live_session_id`, `session_id_from_payload`, `session_id_from_env`) and
  additively stamp it into an event's free-form `meta` (`stamp_session_id`).
- `meta.session_id` on live hook events and MCP events when a source is
  available. The pending-v2 chunk now carries `session_id` so async
  classify-worker events inherit it.

## [0.10.3] - 2026-06-06

**Search & pack quality fixes from real user feedback.** Five bugs hit
during a month-long session: FTS5 query crashes on hyphenated
identifiers (`OPS-306` → `no such column: 306`), event-body search
missing hits the tokenizer split differently than the query, pack
truncation cutting the **newest** decision (most important) instead
of the oldest, no way to filter search by event type, and duplicate
"Conversation compacted" markers when PreCompact fires twice within
the same second.

### Added
- `tj_core::fts::sanitize_query` — phrase-quotes FTS5 metacharacters
  (`-` `:` `*` `(` `)` `"` `/`) so identifiers like `OPS-306`, paths
  like `src/main.rs`, and tokens like `ttl:30s` stop crashing the
  `search_fts MATCH` planner. Multi-word queries pass through
  unchanged so default AND semantics are preserved.
- `tj_core::fts::like_pattern` — wraps a query as `%query%` for the
  LIKE-fallback path described below.
- `--type <event_type>` flag on `task-journal search` and matching
  `event_type` field on the MCP `task_search` tool. Restricts hits
  to a single event class (`decision`, `evidence`, `finding`, ...).
- LIKE fallback in both CLI `search` and MCP `task_search`: when the
  sanitized FTS5 phrase returns zero hits, the same query is rerun
  against `search_fts.text LIKE %query%`. Recovers cases where the
  unicode61 tokenizer split the source text differently from the
  query string.

### Changed
- `render_active_decisions`, `render_evidence`, `render_rejected`
  now `ORDER BY ... DESC` (newest-first). The summary/final decision
  the agent records just before close lives at the *top* of its
  section so end-of-pack truncation drops the **oldest** rows, not
  the newest.
- `FULL_BUDGET` bumped 10 KiB → 24 KiB. Real long-running tasks
  (50–100 events with detailed decision text) blew past 10 KiB after
  a couple of weeks and the budget was the binding constraint on
  what survived. 24 KiB still fits comfortably inside any modern
  LLM context window.

### Fixed
- B1 (CRITICAL): `task_search "OPS-306"` no longer crashes with
  `MCP error -32603: no such column: 306`. Same fix covers all
  paths/colon-prefixed tokens/glob-shaped queries.
- B2 (HIGH): event-body terms now reach the user via the LIKE
  fallback when an FTS5 token-split mismatch otherwise hides them.
- B3 (HIGH): the **newest** decision is now the first line of the
  Active decisions section and survives truncation; the user's
  "final summary" pattern no longer gets clipped.
- B5 (LOW): two `PreCompact` hook firings within 60 s no longer
  double-write the boundary marker. The dedup check inspects the
  most-recent decision event for the active task and skips the
  append if it already looks like a recent compaction marker.

### Migration
- No schema changes. Existing tasks pick up the new ordering on the
  next pack render (cache is keyed by mode, not order, so callers
  may need to clear `task_pack_cache` once for visible effect — or
  wait for the next event to invalidate it organically).

### CLI / MCP API
- CLI: `task-journal search <query> [--type TYPE]` is additive.
- MCP `task_search`: new optional `event_type: Option<String>`
  parameter. Existing callers that omit it see no behavior change
  besides the FTS5 crash fix.

## [0.10.2] - 2026-06-02

**`watchPaths` + FileChanged → auto-evidence on marker file edits.** X4
of the v0.10.x undocumented-hooks adoption. SessionStart envelope now
emits `watchPaths` — Claude Code starts monitoring the project's
marker files (CLAUDE.md, README.md, .docs/plans/), and whenever one
of them changes (write, create, delete), Claude Code fires the
`FileChanged` hook event. Our `ingest-hook` handler translates that
into an `evidence` event on the active task. Captures
"instructions / plans were edited mid-session" without anyone manually
typing it. Schema verified in Claude Code 2.1.160:
`literal("FileChanged"), file_path: y.string(), event:
y.enum(["change","add","unlink"])`.

### Added
- SessionStart envelope emits `watchPaths` containing the absolute
  paths of `CLAUDE.md`, `README.md`, and `.docs/plans` when they
  exist under the current cwd. Missing files are skipped — Claude
  Code's watcher logs an error on non-existent paths, so we don't
  ask it to watch them.
- `FileChanged` branch in the `ingest-hook` handler: appends an
  `evidence` event (`FileChanged (change|add|unlink): <relative path>`)
  to the most-recent open task. No active task → silently no-op.
- 4 new integration tests:
  - `session_start_emits_watch_paths_for_existing_marker_files`
  - `session_start_omits_watch_paths_when_disabled_via_env`
  - `file_changed_hook_appends_evidence_to_active_task`
  - `file_changed_hook_with_no_open_task_is_no_op`

### Changed
- Path display in FileChanged evidence trims the project cwd prefix
  so the journal stays project-relative and doesn't waste tokens on
  the absolute home path.

### Configuration
- `TJ_WATCH_PATHS=0` suppresses watchPaths emission for users who
  don't want their marker-file edits auto-logged.

### Migration
- None — additive on SessionStart envelope + new hook branch.
  Claude Code 2.1.x+ required for FileChanged event firing; older
  versions ignore unknown envelope keys and never emit FileChanged,
  so the handler simply never fires for them.

## [0.10.1] - 2026-06-02

**SessionStart envelope: `sessionTitle` + `initialUserMessage`.**
X2 of the v0.10.x undocumented-hooks adoption. Extends the existing
`hookSpecificOutput` envelope emitted by `task-journal ingest-hook`
on SessionStart with the two extra fields verified in Claude Code
2.1.160's K45 Zod schema. `additionalContext` already injected the
full task pack into the system prompt; the new fields give the model
a sharper "where were we" signal:

- `sessionTitle` — shown as the terminal tab / window label. Format:
  `TJ — <task-id> (<n> open)`. Always emitted when there is at least
  one open task.
- `initialUserMessage` — prepended to the user's first real prompt
  this session. Format: `[Task Journal resumed: <id> — <title>]`.
  Emitted only when the primary task has at least one non-`open`
  event (i.e. real activity, not just creation marker), so fresh
  tasks don't get an unsolicited "resuming" preamble. Gated by
  `TJ_INITIAL_USER_MESSAGE=0` for users who'd rather opt out.

### Added
- `sessionTitle` field on SessionStart envelope.
- `initialUserMessage` field on SessionStart envelope, with hollow-
  task and env-var-opt-out guards.
- 2 new integration tests:
  `session_start_emits_no_initial_user_message_for_hollow_task`,
  `session_start_initial_user_message_disabled_via_env`.
- Existing `ingest_hook_session_start_emits_resume_pack_json` test
  now asserts both new fields.

### Migration
- None — additive on the SessionStart envelope. Older Claude Code
  versions ignore unknown keys. The feature is invisible until the
  primary task accumulates a non-`open` event.

## [0.10.0] - 2026-06-02

**`asyncRewake` on PostToolUse — zero-latency happy path, wake on backlog.**
Adopts the undocumented `asyncRewake: true` Claude Code hook field (verified
present in 2.1.160's Zod schema as `asyncRewake` + `rewakeMessage` +
`rewakeSummary`). The PostToolUse hook now runs entirely in the background:
the model never blocks on `task-journal ingest-hook` on the success path,
even though the binary still does its full persist-to-`pending/` + spawn-
classify-worker work. When the pending queue grows past 25 entries, the
hook exits with code 2, which Claude Code interprets as "wake the model
with a system reminder." The model sees `rewakeSummary` ("Task Journal
backlog forming") plus the hook's stdout — a one-liner pointing at
`task-journal pending-gc --days 0`. The classifier-can't-keep-up state
becomes visible BEFORE it grows into the hundreds (the v0.6.2 fork-bomb
era saw 515 entries before a user noticed).

PreCompact and Stop hooks stay synchronous — they do transcript catch-up
that must finish before compaction/exit, and exit code 2 from a sync
hook BLOCKS the operation in Claude Code's contract. The wake-signal is
gated on `TJ_ASYNC_REWAKE=1`, which only the PostToolUse hook command
sets; CLI invocations and sync hooks never exit 2 even on overflow.

### Added
- `PENDING_OVERFLOW_THRESHOLD = 25` const and `count_pending_entries`
  helper in `tj-cli` — best-effort directory count, IO errors return 0
  so a borked filesystem never wakes the model with noise.
- 3 new integration tests: `asyncrewake_below_threshold_exits_zero`,
  `asyncrewake_overflow_exits_two_with_drain_hint`,
  `asyncrewake_overflow_without_env_does_not_exit_two` — the last one
  is the sync-hook safety guarantee.

### Changed
- `plugin/hooks/hooks.json` PostToolUse entry now declares
  `"asyncRewake": true` + `"rewakeSummary": "Task Journal backlog forming"`
  and the command sets `TJ_ASYNC_REWAKE=1`. Dropped the trailing
  `|| true` from the PostToolUse command — asyncRewake hooks treat
  exit codes themselves; other exit codes are ignored, only code 2
  wakes. PreCompact and Stop entries are unchanged.

### Migration
- `task-journal install-hooks --uninstall && task-journal install-hooks`
  to pick up the new hook contract. Claude Code 2.1.x or later required
  for the `asyncRewake` field to be recognized (silently ignored on
  older versions — they will run the PostToolUse hook synchronously
  as a fallback). The 25-entry threshold is hard-coded for v0.10.0;
  later releases may make it configurable.

## [0.9.4] - 2026-05-17

### Fixed
- Clippy `doc_lazy_continuation` lint on v0.9.3 release commit
  failed CI (rustc 1.95 promoted the lint to a hard error under
  `-D warnings`). The docstring on
  `enqueue_transcript_chunks_since_last_event` started a line with
  `+`, which the new lint reads as a list item whose continuation
  lines must be indented. Replaced with prose ("user and assistant
  text entries"). No behavior change.

## [0.9.3] - 2026-05-17

**Stop hook learns to catch up.** Previously the `Stop` hook fired
with a hardcoded `--text="Session ended"` — a sentinel string that
carried no semantic signal and just littered the pending queue with
unclassifiable noise (the heuristic skipped it, the API would have
spent a haiku call to say "this is nothing"). v0.9.3 replaces it with
the same transcript-tail catch-up that PreCompact already does:
parse the JSONL session log, enqueue user + assistant entries newer
than the active task's last event timestamp, spawn the
classify-worker. No boundary marker — a session end isn't a
reasoning boundary, just a pause.

### Added
- Stop-hook transcript catch-up. Mirrors the PreCompact catch-up
  introduced in v0.7.1. Reads `transcript_path` from the hook stdin
  payload; chunks land as `UserPromptSubmit` (user) or `StopChunk`
  (assistant) pending v2 entries. Distinct `StopChunk` kind lets ops
  grep the pending dir by source hook.
- `enqueue_transcript_chunks_since_last_event` helper — extracted
  from the PreCompact branch so both hooks share the same code path.
  Old `precompact_enqueue_transcript_chunks` was renamed; same body,
  one new parameter (`assistant_chunk_kind`).

### Changed
- Plugin `hooks.json` Stop entry no longer pins
  `--kind=Stop --text="Session ended"`. Reads hook stdin payload
  like PostToolUse and PreCompact already do.

### Compatibility
- Mock test path (`--mock-event-type` + `--mock-task-id`) bypasses
  the new Stop branch so existing test fixtures invoking
  `--kind=Stop` with mock args still hit the mock-classifier
  dispatch instead of being intercepted by transcript catch-up.

## [0.9.2] - 2026-05-17

### Fixed
- Windows CI flake in `session::discovery::tests::*` — four tests
  mutated `CLAUDE_CONFIG_DIR` in parallel and observed each other's
  writes. Now serialized through a module-level `Mutex<()>`; the
  Windows runner sees the expected override path. Linux/macOS were
  asymptomatically affected by the same race.
- `claude_config_dir_respects_env_var` no longer hardcodes
  `/tmp/custom-claude-config` (invalid on Windows). Uses
  `std::env::temp_dir()` for a portable path.

## [0.9.1] - 2026-05-17

### Fixed
- `cargo fmt --all --check` on the v0.9.0 release commit failed and
  blocked the release pipeline. v0.9.1 carries only the formatting
  fixes — no behavior change.

## [0.9.0] - 2026-05-17

**Breaking — `cli` backend removed.** v0.8.0 left it as a deprecated
alias for `hybrid`; v0.9.0 deletes the implementation. With it goes
the `--classifier-command` flag, the `TJ_CLASSIFIER_CLI` env var
(only the back-compat strip on uninstall stays), and the
`ClaudeCliClassifier` struct.

If you upgraded to v0.8.0 you saw a one-line deprecation warning on
every hook; that's the whole migration story. On v0.9.0 the value
`--backend=cli` errors with `unknown backend: cli (expected hybrid,
api, or heuristic)`.

### Removed
- `tj_core::classifier::cli::ClaudeCliClassifier` and the entire
  `tj_core::classifier::cli` module.
- `crates/tj-core/tests/classifier_eval.rs` and its fixtures — the
  eval harness depended on `ClaudeCliClassifier`.
- `task-journal install-hooks --classifier-command <CMD>` flag.
- `TJ_CLASSIFIER_CLI` env var write on install. The variable is
  still **stripped** on `--uninstall` to clean up settings.json from
  pre-0.9 installs (back-compat).
- Default value handling for `--backend=cli`. It now hits the
  generic `unknown backend` error path.

### Documentation
- README rewritten around the hybrid model — heuristic stage
  characterized, API stage as the optional fallback, no `claude -p`
  references anywhere.
- Plugin skill description (`SKILL.md`) drops the Pro/Max
  subscription claim that was no longer true.
- Configuration table trimmed to the two env vars that still matter:
  `ANTHROPIC_API_KEY` and `TJ_CLASSIFIER_MODEL`.

### Migration
- Re-run `task-journal install-hooks --scope user` to refresh
  `~/.claude/settings.json` without the legacy `--backend=cli` flag
  and without `TJ_CLASSIFIER_CLI` in `env`.
- If you want the API stage (recommended for full coverage), set
  `ANTHROPIC_API_KEY` in your shell or in the same `settings.json`
  `env` block.

## [0.8.0] - 2026-05-17

**Breaking — classifier backend reshaped.** Anthropic changed `claude -p`
to bill against a separate token budget instead of riding the Pro/Max
subscription, so the v0.7.x `--backend=cli` path silently charged users
who had explicitly opted into "free background". v0.8.0 removes it from
the default path and replaces it with a heuristic-first hybrid.

### Added
- New `--backend=hybrid` (now the default). Keyword heuristic runs
  first — handles obvious decisions, rejections, evidence, findings,
  constraints, hypotheses, corrections in EN+RU at zero cost. If the
  heuristic is uncertain (or no rule fires), falls back to the
  Anthropic API backend when `ANTHROPIC_API_KEY` is set; otherwise
  the chunk stays in `pending/` for later retry. No `claude -p`
  subprocess.
- New `--backend=heuristic` — heuristic only, no LLM. For users who
  want strict zero-cost / offline operation and accept lower coverage.
- `tj_core::classifier::heuristic::try_heuristic` — public helper for
  pattern-matched classification, returning `Option<ClassifyOutput>`.
- `tj_core::classifier::hybrid::HybridClassifier` — production
  default. Exposes `from_env()` (picks up `ANTHROPIC_API_KEY`) and
  `has_llm_fallback()` for callers that want to surface state to the
  user.

### Changed
- Default backend in `task-journal ingest-hook` and
  `task-journal classify-worker` changed from `cli` to `hybrid`.
- `install-hooks` settings.json template no longer pins
  `--backend=cli`; the binary default wins.
- Plugin `hooks.json` (PostToolUse / PreCompact / Stop) no longer
  passes `--backend=cli`. Same default-wins reasoning.

### Deprecated
- `--backend=cli` now routes to `hybrid` and prints a one-line
  deprecation warning on stderr. The `ClaudeCliClassifier` struct
  stays in `tj_core::classifier::cli` for the v0.7.x eval harness
  but is no longer reachable from production code. Will be removed
  in v0.9.0.

### Migration
- **Users with Pro/Max only and no API key** — keep working: the
  heuristic catches the most common cases; chunks it can't classify
  land in `pending/` and you can drain them later when you set an
  API key (or just leave them — they don't block anything).
- **Users with `ANTHROPIC_API_KEY` set** — best experience. Heuristic
  saves API calls on obvious cases; the API handles the rest.
- **No action required.** Old `~/.claude/settings.json` with
  `--backend=cli` still works (alias). Re-run `install-hooks` to
  remove the deprecation warning.

## [0.7.1] - 2026-05-17

PreCompact closes the gap before compaction — the synchronous hook only
fires on `PostToolUse`, so any reasoning between the final tool call and
the compact event used to vanish. v0.7.1 reads the transcript JSONL on
`PreCompact`, enqueues entries newer than the active task's last event
timestamp as pending v2 chunks, and spawns the classify-worker. The
boundary marker still lands as before; the catch-up is additive.

### Added
- PreCompact transcript catch-up — `ingest-hook --kind=PreCompact` now
  reads `transcript_path` from the hook stdin payload, walks the
  session JSONL, and enqueues user/assistant entries newer than the
  task's last event timestamp as pending v2 chunks (`UserPromptSubmit`
  / `PreCompactChunk`). The classify-worker picks them up after the
  hook returns. Best-effort: missing or unreadable transcript falls
  through to the marker-only path.
- Plugin `hooks.json` now wires `PreCompact` (was previously only
  installed via `install-hooks`). Plugin users get the catch-up
  without re-running the installer.
- `TJ_DISABLE_CLASSIFY_SPAWN=1` env var — skips the classify-worker
  spawn after enqueueing. Test-only; not documented as public.

### Fixed
- Plugin `hooks.json` PostToolUse template — was passing
  `--text="$TOOL_OUTPUT"` (an env var Claude Code never sets), feeding
  the classifier empty text and dropping every PostToolUse event in
  the plugin install path. Now reads the hook payload from stdin like
  `install-hooks` already does. Plugin users may see a sudden
  uplift in captured events — by design.

## [0.7.0] - 2026-05-10

Reasoning-chain ergonomics: surface the journal in the Claude Code
statusline, capture compaction + rewind boundaries automatically, and
make rejection lookup + PR-description rendering first-class CLI
commands. All additive — no schema breaking changes from 0.6.x.

### Added
- `task-journal statusline` — sub-100ms one-liner for the Claude Code
  statusline. Renders `[tj-x9rz · open: N · pending: N · stale: N]`
  using only the small `tasks` table, no FTS5 hits, no classifier
  calls. Hidden from `--help` (it's tooling, not a human command).
  Wire it via `~/.claude/settings.json` `"statusLine"`.
- `task-journal rejected <topic>` — cross-task rejection lookup.
  FTS5 by default, LIKE fallback for FTS-unfriendly topics (e.g.
  `oauth-pkce`). Supports `--all-projects`, `--limit`, `--since`.
  Surfaces approaches that were already turned down so the agent
  doesn't repeat them.
- `task-journal export-pr <id>` — render a task as PR-description
  Markdown: Summary, Changes (decisions), Why-this-approach
  (rejections), Verification (evidence), Affected (artifacts).
  Reuses existing event log + artifacts; no new tables.
- PreCompact hook handler — Claude Code emits `PreCompact` before
  compaction; ingest-hook now drops a marker `decision` event on the
  most-recent open task so the post-compact agent sees a clear
  boundary in the journal.
- `/rewind` UserPromptSubmit marker — when the user prepends `/rewind`
  to a prompt, ingest-hook appends a single `correction` event
  instead of running the classifier. Conservative — does NOT mass-mark
  prior events as rejected; just leaves a sentinel for pack readers.
- Plugin skill `rejected.md` wrapping the new CLI command.

### Changed
- `install-hooks` now wires a `PreCompact` event entry alongside the
  existing `UserPromptSubmit` / `PostToolUse` / `Stop` / `SessionStart`
  hooks. Re-run `task-journal install-hooks` to pick this up.

## [0.6.3] - 2026-05-09

Drop empty-text events at the hook boundary. PostToolUse for tools
with no `tool_response` (SlashCommand, background ops, etc.) used to
queue text="" entries that always failed classification and littered
`pending/` with v1 dead entries.

### Fixed
- `ingest-hook` now early-returns when `text.trim().is_empty()` for
  the real-classifier path. Mock path (test-only) keeps the event.
  Saves a haiku call per empty event and prevents pending-queue
  pollution from silent tools.

## [0.6.2] - 2026-05-09

Fork-bomb fix. Synchronous classifier in `ingest-hook` was blocking
each Claude Code hook for 5-30s while spawning a nested
`claude -p` that loaded all installed plugins (including
task-journal-mcp itself), so within minutes ~19 stale
`task-journal ingest-hook` and `task-journal-mcp` processes piled up
and WSL2 died on `EAGAIN: pthread_create`.

### Fixed
- `ingest-hook` no longer blocks on the classifier. Real-classifier
  events are queued to `pending/<id>.json` (schema "v2") and a
  detached `task-journal classify-worker` child drains them in the
  background. Hook returns in <100ms instead of 5-30s. Mock-classifier
  path stays synchronous (tests rely on it); set `TJ_INGEST_SYNC=1`
  to force sync mode for the real path too.
- `tj_core::classifier::cli::ClaudeCliClassifier` injects
  `--strict-mcp-config --mcp-config '{"mcpServers":{}}'` automatically
  when the configured command is bare `claude` (no wrapper). Wrappers
  like `aimux run dt claude` are detected by non-empty base args and
  left alone — wrappers may not pass through unknown flags. Stops the
  inner haiku-claude from spawning task-journal-mcp (and ~24 other MCP
  servers) per classification. `--bare` not used because it breaks
  subscription auth (claude-memory-0kk); `--no-plugins` does not exist
  in claude 2.1.x CLI.
- New project-scoped worker lockfile at
  `state_dir/classifier-<project_hash>.lock` caps in-flight
  classifier workers at 1 per project. PID is written to the lockfile
  on acquire; stale lockfiles (dead PID) are reclaimed automatically.

### Added
- Hidden `task-journal classify-worker --backend <cli|api>`
  subcommand. Internal — spawned by `ingest-hook`. Not stable API.

### Changed (internal)
- `pending/<id>.json` gained a `"schema"` field. v2 entries carry
  `kind`, `text`, `project_hash`, `events_path`, `backend` and route
  through `classify-worker`. v1 entries (legacy `text`+`error` shape)
  still parse and route through the existing `pending retry` path.

## [0.6.1] - 2026-05-08

Branch-name regex was too greedy and captured the next word after any
prose mention of "branch". After running `reclassify` against a
real-session task we saw `branches: names` appear in pack output
because the meta-text discussed regex categories ("commits, PRs,
issues, files, branches"). Fix: anchor the pattern to an explicit
`git ` prefix.

### Fixed
- `tj_core::artifacts::extract` — the branch capture now requires
  `\bgit\s+(?:checkout\s+-b|switch\s+-c|branch)\s+...` so bare-prose
  `branch X` no longer matches.

### Added
- New unit test `does_not_capture_branch_from_prose` to lock the
  regression.

## [0.6.0] - 2026-05-08

Backlog cleanup: MCP brought in line with CLI, score-based linking,
TUI/pack split out a Linked block, hygiene commands for stale tasks
and pending GC, and the classifier protocol got an artifacts field
ready for richer model output.

### Added — MCP parity
- `task_create` MCP tool now accepts `goal: Option<String>` and
  persists it via `set_task_goal` after writing the open event.
- `task_close` MCP tool now accepts `outcome_tag: Option<String>`
  validated against `done|abandoned|superseded`. Outcome + tag
  both go into the tasks table and the close event meta.

### Added — Hygiene CLI commands
- `task-journal stale [--days 7]` lists open tasks whose last event
  crossed the inactivity threshold. Sorted by idle time descending.
  Hint at the bottom suggests close-with-abandoned for the obvious
  cases.
- `task-journal pending-gc [--days 7]` deletes pending classifier
  payloads older than the threshold. Useful after a long classifier
  outage when the queue stops being recoverable.

### Added — Smarter linking
- `db::find_related_tasks` scores tasks by overlap on
  `linked_issue` (1.0), `commit_hash` (0.8), and `file path` (0.3).
  Replaces the linked-issue-only scan inside auto-open.
- Pack render splits `linked:tj-xxx` entries into a dedicated
  `**Linked**:` block with the live status of each pointer (`open`
  / `closed` / `unknown`). Other external references stay in
  `**External**`.
- Artifact extractor now captures dot-prefixed directories
  (`.docs/specs/auth.md`, `.github/workflows/ci.yml`).

### Added — Classifier protocol
- `ClassifyOutput.artifacts: Option<Artifacts>` (with `#[serde(default)]`
  for backwards compat). Field is ready for the next prompt
  iteration that will instruct the model to return structured
  artifacts; current behaviour unchanged (regex extraction still
  the source of truth).

### Tests
- 1 new unit test for dot-prefixed directory extraction.
- All previous tests updated for the new External/Linked split.

## [0.5.0] - 2026-05-08

Auto-everything release. Phase B + C of the v0.5.0 plan land
together: artifacts get scraped out of every event automatically,
and prompts that mention a known ticket id auto-link back to the
prior task that handled it.

### Added — Phase B (artifacts auto-extract)
- New `tj_core::artifacts` module with `Artifacts` struct +
  regex-based `extract(text)`. Pulls commit hashes (7-40 hex), GitHub
  / GitLab PR URLs, ticket IDs (FIN-868 etc), file paths, and branch
  names from any free-form text.
- `events_index.artifacts` (added in v0.4.0 schema v003) is now
  populated on every `ingest_new_events` call. Per-event JSON keeps
  reclassify cheap.
- `db::task_artifacts(conn, task_id)` aggregates and dedupes across
  every event of a task.
- Pack output gets a new `**Artifacts**:` block listing commits, PRs,
  issues, files, branches when any are present.
- New CLI `task-journal reclassify <task_id>` walks existing events
  and backfills `artifacts` for journals upgraded from v0.4.x.

### Added — Phase C (linked_issue / reopen)
- `db::find_tasks_by_linked_issues(conn, issues)` searches every
  task whose events reference a given ticket id.
- `auto_open_task_from_prompt` now extracts artifacts from the
  prompt; if any ticket id matches a prior task, the new task gets
  `linked:tj-old-id` appended to its External column. When the prior
  task is closed, a hint goes to stderr suggesting
  `task-journal reopen <id>` instead of fresh scope.
- New CLI `task-journal reopen <task_id> [--reason "..."]` flips a
  closed task back to open (writes a `[reopen]` event whose lifecycle
  hook handles the status flip).

### Schema
- Migration v004 wipes the pack cache so existing tasks pick up the
  new Artifacts block on next render. Events still need `reclassify`
  to backfill the `artifacts` column for old data.

### Tests
- 9 new unit tests for `tj_core::artifacts` (commit / PR / issue /
  file / branch extraction + dedup + JSON round-trip).
- 4 new integration tests in tj-cli covering pack rendering with
  artifacts, reclassify backfill, reopen lifecycle, and Phase C
  auto-link to closed task.

## [0.4.1] - 2026-05-08

v0.5.0 Phase A — auto-create tasks. Removes the manual
`task-journal create --goal "..."` step. The journal now opens a
task on demand the first time a UserPromptSubmit fires into an
empty project, taking the prompt itself as the goal. No prompt is
ever lost again.

### Added
- `auto_open_task_from_prompt()` helper in `tj-cli`. Synthesizes a
  task with `title = first line trimmed to 80 chars`,
  `goal = prompt trimmed to 200 chars`, then continues the normal
  classifier pipeline so the same prompt becomes the first event on
  the task it just opened.
- `meta.auto_opened: true` flag on synthesized open events so
  reclassify / analytics can distinguish auto-opened tasks from
  user-created ones.

### Changed
- `ingest-hook` previously dropped UserPromptSubmit events when no
  open task existed. Now it auto-opens unless the assistant tool
  call is the trigger (PostToolUse / Stop never conjure tasks).

### Configuration
- `TJ_AUTO_OPEN_TASKS=0` (or `false`) restores the v0.4.0 silent-
  drop behaviour. Default is ON.

### Phase B/C still pending
- B (artifacts auto-extract: commit_hash, pr_url, files, linked_issue)
- C (linked_issue / reopen suggestion when prompt matches a recently
  closed task)

## [0.4.0] - 2026-05-08

Task model redesign — Phase 1. A task is now an explicit
**goal → outcome** record, not a free-form bag of events. Lets the pack
answer "what was I trying to do, did it work, and what did it
produce?" without re-reading the whole chain.

### Added
- `tasks.goal` column — the intent ("why am I touching this code").
  Set via `task-journal create --goal "<text>"` at creation, or later
  via `task-journal goal <task_id> "<text>"`.
- `tasks.outcome` + `tasks.outcome_tag` columns — what actually
  happened on close. Set via
  `task-journal close <id> --reason "..." --outcome "..." --outcome-tag done|abandoned|superseded`.
  Tag is validated against the enum.
- `tasks.external` column — comma-separated free-form references
  (commit hashes, PR URLs, file paths). Append via
  `task-journal external <task_id> --add "<ref>"`.
- `events_index.artifacts` column — reserved for Phase 2 classifier
  artifact extraction (commit_hash, files, linked_issue).
- `tj_core::db::set_task_goal`, `set_task_outcome`, `add_task_external`,
  `task_metadata` (returns `TaskMetadata` struct) helpers.

### Changed
- Pack output now renders a **Goal** line (or `(not set)`), an
  **Outcome [tag]** line for closed tasks (or `(not recorded)`), and
  an **External** line when references exist. Resume packs and `pack`
  command both updated.
- Schema migrated to v003. `task_pack_cache` is wiped on migration so
  existing tasks re-render with the new fields visible.

### Migration notes
Existing tasks keep their events but get `(not set)` / `(not
recorded)` placeholders until the new flags are used. Phase 2 will
add a `task-journal reclassify <id>` to backfill goals/outcomes from
event history.

## [0.3.1] - 2026-05-08

Three correctness fixes for the auto-capture pipeline. The journal
was technically working but producing confusing output: events
attached to the wrong tasks, sessions auto-closed tasks they had no
business closing, and TUI's compact summary hid the reasoning chain
the user actually wanted to see.

### Fixed
- TUI task detail now renders the **Full** pack instead of Compact —
  every event, decisions, rejections, evidence (including commit
  hashes), and close lines, in chronological order. Compact's three-
  line "Active decisions / Recent events" summary made the detail
  view look empty.
- Stop hook no longer auto-closes tasks. The Stop hook fires every
  time a Claude Code session ends, and the classifier was happily
  emitting `Close` events from those endings. Sessions ending !=
  task done. Closes are now reserved for explicit
  `task-journal close <id>` calls.
- Closed and missing tasks are no longer silently appended to. When
  the classifier's `task_id_guess` points at a task that doesn't
  exist or is already closed, the event is routed to `pending/`
  instead of being attached. Old tasks ("Demo task", "Тест plugin"
  in our case) stop accumulating events from unrelated work.

### Added
- `tj_core::db::task_status(&conn, task_id)` helper for the closed-
  task safeguard above.

## [0.3.0] - 2026-05-08

### Changed
- **`task-journal ui` now opens the task-journal browser by default,
  not the chat-session browser.** Surfaces what the journal is *for*
  — tasks of the current project (open first by recency, then closed)
  with event count and last-activity timestamps. Enter on a task
  renders its compact resume-pack inline. The old chat-session
  browser is still available behind `task-journal ui --chats`. This
  is a breaking change to UX — bumping minor version (0.3.x) to
  flag it.

### Added
- New `tj_core::db::list_tasks_by_project` query and `TaskRow` type
  feeding the new TUI list view. The query is denormalised (joins
  `events_index` for `event_count` in a single round-trip) so the
  TUI doesn't pay per-row overhead on large journals.
- New TUI screens: `task_list` (the new default) and `task_detail`
  (renders `pack::assemble(.., Compact)` text scrollably). Both have
  the same key bindings as the legacy session browser (j/k arrow
  navigation, Esc back, q quit).
- `--chats` flag on `task-journal ui` to open the legacy chat-session
  browser. Same behavior as v0.2.11's default.

## [0.2.11] - 2026-05-08

### Fixed
- TUI session list (`task-journal ui`) now hides classifier sessions.
  Each `claude -p` invocation we make for classification creates its
  own JSONL in `~/.claude/projects/`; without filtering, the TUI was
  buried under hundreds of one-message ghost sessions all starting
  with "You classify chat chunks for an AI-coding-agent task journal."
  We now skip any session whose first user message begins with that
  marker so only real conversations show up.

## [0.2.10] - 2026-05-07

### Fixed
- Classifier now strips wrapper prelude lines from claude's stdout
  before parsing the JSON envelope. `aimux run` (and similar
  orchestrators) prepend "Auto-sync: 0 created, 0 repaired, …"-style
  status lines, which made `serde_json::from_str` choke on the first
  character. We now anchor the parse at the first `{`. One unit test
  (`classifier_strips_wrapper_prelude_before_envelope`) covers the
  shape end-to-end with a fake script that emits a prelude before
  the envelope.

## [0.2.9] - 2026-05-07

Critical fix: classifier path now works for users on Claude Pro/Max
subscription (the majority of Claude Code users). v0.2.8 still
shipped `--bare` to the inner `claude -p` invocation; that flag
silently bypasses `~/.claude/.credentials.json` and demands
`ANTHROPIC_API_KEY`. With only a subscription, every classification
returned "Not logged in".

### Fixed
- `ClaudeCliClassifier` no longer passes `--bare`. Hook recursion
  (the original reason for `--bare`) is now broken via an explicit
  env-var sentinel: the classifier sets `TJ_IN_CLASSIFIER=1` on the
  child process, and `ingest-hook` returns immediately when it sees
  that env. One regression test (`ingest_hook_short_circuits_when_in_
  classifier_env_set`) covers the guard. Closes claude-memory-0kk.

### Notes
- Without `--bare`, the inner `claude -p` loads the user's
  `CLAUDE.md`, skills, and hooks. That increases the prompt-cache
  cost the first time per 5-minute window. The classifier prompt is
  explicit about the JSON-only output contract, so model compliance
  is preserved; subsequent calls within the cache TTL hit the
  prompt cache and stay cheap.

## [0.2.8] - 2026-05-07

Critical fix: hooks now actually carry content end-to-end. Without
this release, every captured event reached the classifier with empty
text, queued in `pending/`, and never got classified.

### Fixed
- `ingest-hook` now reads the Claude Code hook payload as JSON from
  stdin (the documented wiring) instead of relying on `$CLAUDE_HOOK_NAME`
  / `$CLAUDE_HOOK_TEXT` env vars that Claude Code never set. Per
  hook kind:
  - `UserPromptSubmit` → `prompt`
  - `PreToolUse` / `PostToolUse` → synthesized from `tool_name`,
    `tool_input`, and (when present) `tool_response`
  - `Stop` / `SessionStart` → empty (SessionStart already short-
    circuits to its resume-pack path).
  `--kind` / `--text` remain accepted as CLI overrides for tests and
  ad-hoc use; they take precedence when both are passed.
- `install-hooks` now writes `task-journal ingest-hook --backend=cli
  || true` — the bogus env-var interpolation is gone. Closes
  claude-memory-rsw.

## [0.2.7] - 2026-05-07

### Fixed
- `install-hooks --uninstall` previously called `hooks_obj.remove("hooks")`,
  which erased every plugin's hook entries — token-pilot, custom user
  hooks, anything else co-located in `~/.claude/settings.json`. Now
  the uninstall walks each event kind, filters out only commands
  containing `task-journal ingest-hook`, and drops empty matchers /
  empty kinds / the empty hooks block in that order. Third-party
  hooks survive even when they share a `UserPromptSubmit` matcher
  with task-journal. Closes claude-memory-bxl.

## [0.2.6] - 2026-05-07

Three additive features that close the "auto-memory" loop end-to-end:
the journal can now (1) surface itself at session start, (2) seed
itself from existing Claude Code history at install time, and (3)
recognize a project regardless of which subdir you launch from.

### Added
- **SessionStart resume-pack injection**. `task-journal ingest-hook
  --kind=SessionStart` now opens the project's journal, renders the
  three most-recent open tasks in compact mode, and writes a
  `hookSpecificOutput.additionalContext` envelope to stdout. Claude
  Code merges that into the system prompt so a new session starts
  with the journal's state already in context — no manual
  `task_pack` call needed. Empty stdout when there are no open
  tasks, so fresh projects don't get noise.
  `install-hooks` automatically wires the `SessionStart` event
  alongside the existing three.
- **`install-hooks --backfill`**. After writing the hook entries,
  re-execs `task-journal backfill` against the current directory so
  first-time users get an auto-populated journal from their existing
  Claude Code history. Onboarding becomes one command.
- **Project-root normalization in `project_hash`**. `repo/`,
  `repo/src/`, and `repo/src/foo/bar/` now hash to the same project
  by walking up to the first `.git` (file or directory, so worktrees
  work) or `.task-journal/` marker. Without this, opening Claude
  Code in a subdir gave an empty journal and silently broke
  continuity. `.task-journal/` is the explicit override for
  intentional sub-projects.

DX improvement: ship classifier-wrapper config through `install-hooks` —
no more manual `bashrc` / `settings.json` edits to use `aimux`, `direnv`,
`nix run`, etc.

### Added
- `task-journal install-hooks --classifier-command "<CMD>"` flag.
  Writes `env.TJ_CLASSIFIER_CLI=<CMD>` into the same `settings.json`
  that already gets the hook entries. Claude Code reads the `env` block
  at startup and propagates the variable to hook subprocesses.
  Example:
  ```bash
  task-journal install-hooks --classifier-command "aimux run dt"
  ```
  When the flag is omitted, no `env` block is touched — default
  classifier remains the bare `claude` binary.
- `--uninstall` now also strips `TJ_CLASSIFIER_CLI` from `env`,
  preserving any unrelated env keys and dropping the `env` block
  if it becomes empty.

### Fixed
- 0.2.4's instructions told users to set `TJ_CLASSIFIER_CLI` via
  `~/.bashrc`, but Claude Code starts hook subprocesses outside an
  interactive bash, so the env var was invisible to the classifier
  and 401s kept piling up in `pending/`. The `--classifier-command`
  flag closes that loop end-to-end.
- CI: bumped MSRV from Rust 1.83 to **1.88** (workspace
  `rust-version` + GitHub Actions toolchain). The ecosystem post-
  2025-02 widely depends on edition2024 (`rmcp`, `clap_lex`) and
  `darling 0.23` which itself requires 1.88. Pinning each transitive
  dep was a losing race; one MSRV bump unblocks them all.
- CI: marked the three `fake_claude`-driven classifier unit tests
  with `#[cfg_attr(windows, ignore)]`. The shim is a `.cmd` script,
  and Rust 1.77.2+ refuses to forward argv with quote characters to
  `.cmd`/`.bat` files because of the BatBadBut CVE
  (CVE-2024-24576). Real `claude` is a native binary, so the
  classifier path is exercised in production; this is purely a
  test-fake limitation on Windows.
- CI: opened the JSONL append handle with `read(true)` in addition
  to `append(true)`. `fd_lock`'s `LockFileEx` on Windows requires
  GENERIC_READ access on the handle; without it tests panicked with
  `acquire exclusive file lock — Access is denied (os error 5)`.
  Linux's `flock` was unaffected, so the issue was Windows-only.

## [0.2.4] - 2026-05-07

Hotfix: support workspace-orchestrator wrappers (aimux, nix-shell, etc).

### Added
- `TJ_CLASSIFIER_CLI` env var. The CLI classifier (`--backend=cli`)
  now splits this on whitespace and uses it as the program + base
  arguments before appending the classifier-specific flags. Lets users
  with `aimux`, `direnv`, `nix run` and similar wrappers pass through
  to their actual `claude` binary without symlinks or PATH gymnastics:
  ```bash
  export TJ_CLASSIFIER_CLI="aimux run dt claude"
  ```
  When unset, defaults to the bare `claude` (previous behavior).

### Fixed
- Auto-capture hooks were silently failing for users on managed Claude
  Code installations where the `claude` binary is not directly on PATH
  but accessed via a wrapper. The `TJ_CLASSIFIER_CLI` override resolves
  this without requiring binary changes to install-hooks.

## [0.2.3] - 2026-05-07

Hotfix release. Re-release of the 0.2.2 fixes plus CI repair —
0.2.2 publish was incomplete (only `task-journal-core` reached
crates.io before MSRV/test failures; `cli`/`mcp` never published).
0.2.3 is the canonical replacement; `task-journal-core@0.2.2` will be
yanked.

### Fixed
- TUI session browser (`task-journal ui`) panicked with `byte index is
  not a char boundary` when a session's first user message was longer
  than 80 bytes and contained non-ASCII characters (Cyrillic, CJK,
  emoji, etc.). Title truncation now slices by Unicode scalars instead
  of bytes. Same fix applied to the fallback `Session <id>` path for
  consistency.
- `task-journal doctor` exited with code 1 when the `claude` CLI was
  not on PATH. Missing `claude` is normal for users on the API
  backend (`ANTHROPIC_API_KEY`) — it should be informational, not an
  error. Doctor now distinguishes hard `issues` (non-zero exit) from
  soft `notes` (zero exit), and `claude` absence is a note.
- MSRV CI job failed because `assert_cmd@2.2.1` requires Rust edition
  2024 (stable in Rust 1.85+) while our MSRV is 1.83. Pinned the dev
  dependency to `>=2, <2.2.1` to keep MSRV builds green.
- Lingering `clippy::doc_lazy_continuation` warning in
  `classifier_eval.rs` test header.

### Added
- Regression tests for `truncate_with_ellipsis`: ASCII under/over
  limit, Cyrillic boundary, emoji char-counting, exact-length no-op.

## [0.2.1] - 2026-05-07

Operational maturity release. No breaking changes — additive features
plus internal perf and observability work.

### Added
- `task-journal export --format sqlite` — VACUUM-based clean snapshot
  of the derived state, streamed to stdout for redirection to a backup
  file.
- `task-journal pending list` and `task-journal pending retry` —
  inspect the auto-capture-hook failure queue and re-feed entries
  through the classifier (mock path wired; real classifier path
  reuses the existing hook drain). `attempts` counter persisted in
  each pending JSON; entries rename to `<id>.dead.json` after 3
  failures.
- MCP server: structured tracing with `correlation_id` per tool call.
  Two INFO log lines wrap each invocation (start + ok / err) so a
  single client request can be greppped across logs.
- MCP server: graceful Ctrl-C and SIGTERM (Unix only) shutdown via
  `tokio::select!` between the rmcp serve loop and a new
  `wait_for_shutdown_signal()` future. Logs which signal arrived.
- New regression tests:
  `cached_open_returns_same_arc_for_same_path`,
  `cached_open_returns_distinct_arcs_for_distinct_paths`,
  `export_sqlite_round_trips_through_pack`,
  `pending_list_shows_queued_entries`,
  `pending_retry_drains_with_mock_classifier`,
  `pending_retry_marks_dead_after_max_attempts`,
  `dummy_client_handler_compiles_and_provides_default_info`,
  `rmcp_call_tool_request_param_round_trips_via_serde`,
  `new_correlation_id_is_unique_across_thousand_calls`,
  `traced_tool_transparently_returns_inner_result`,
  `shutdown_signal_does_not_fire_spuriously`.

### Changed
- MCP server caches one `Arc<Mutex<rusqlite::Connection>>` per state
  path for the process lifetime. Eliminates per-call PRAGMA +
  migration registry replays; small-N tool calls become noticeably
  cheaper.

### Performance
- Tool-call overhead at small event counts dropped (Connection cache,
  D1). Run `cargo bench --workspace` to see the local before/after.

### Internal
- Added `criterion` benches compile in CI (no behaviour change).
- Added rmcp `client` feature in dev-deps to enable the future
  end-to-end MCP roundtrip test once `TaskJournalServer` is
  extracted to a lib target (tracked in claude-memory-yj1.8).
- tokio `signal` feature added to workspace deps.

## [0.2.0-rc.1] - 2026-05-06

> **Release candidate.** Major version bump because the MCP error
> contract changed shape (see _BREAKING_ below). After dogfooding
> for a week the matching `0.2.0` will be cut without further code
> changes.

### BREAKING

- **MCP error contract.** Tool handlers (`task_pack`, `task_search`,
  `task_create`, `event_add`, `task_close`) no longer mask failures
  as success-typed JSON with `task_id = "[error] msg"`. They now
  return JSON-RPC error frames (rmcp `ErrorData`) carrying the full
  `anyhow` chain in the `message` field. Any client that was parsing
  `"[error]"` out of the result must switch to detecting the rpc
  error envelope first.

### Added
- `tj_core::db::ingest_new_events` — incremental indexing that reads
  only the JSONL tail since the last marker. Two safe fallbacks to
  full `rebuild_state`: no marker yet, or marker missing in file.
- `tj_core::db::task_exists` — O(1) lookup against `tasks` PK.
- Migration v002: `index_state(project_hash, last_indexed_event_id,
  updated_at)` table, plus a forward-only migrations registry tracked
  in `schema_migrations(version, applied_at)`.
- MCP `--project-dir <PATH>` argument — overrides the cwd-derived
  project hash. Path is canonicalized at startup.
- `criterion` benchmarks for `rebuild_state`, `pack_assemble_cold`,
  and FTS `search` at 1k and 10k events. CI `benches-compile` job
  guards the harness.
- New regression tests:
  `fresh_db_runs_all_migrations`, `apply_migrations_is_idempotent_
  across_reopens`, `task_exists_returns_true_for_known_id_false_
  otherwise`, `ingest_new_events_picks_up_only_new_lines`,
  `ingest_new_events_falls_back_to_full_rebuild_when_marker_vanishes`,
  `rebuild_state_and_ingest_new_events_produce_same_state`,
  `pack_cache_hits_after_incremental_ingest_with_no_new_events`,
  `into_mcp_error_carries_full_anyhow_chain`,
  `resolve_project_paths_uses_provided_dir_for_hash`,
  `cli_parses_project_dir_argument`,
  `run_blocking_executes_two_tasks_concurrently`,
  `close_unknown_task_id_returns_error` (CLI integration).

### Changed
- Every MCP tool handler now offloads its synchronous I/O to the
  tokio blocking pool via `tokio::task::spawn_blocking`. Concurrent
  client requests no longer serialise behind one slow operation.
- `rebuild_state` writes the `last_indexed_event_id` marker on
  completion so subsequent `ingest_new_events` calls can pick up
  from the tail.
- CLI `Close` and MCP `task_close` validate that `task_id` exists
  in the `tasks` table before appending a close event. Closing an
  unknown id used to silently succeed; now it returns an error
  (CLI: non-zero exit + stderr; MCP: rpc error frame).
- Workspace version `0.1.3` → `0.2.0-rc.1`.

### Performance
- `task_pack`, `task_search`, and the auto-capture hook used to
  re-read the entire JSONL log on every invocation through
  `rebuild_state`. They now use `ingest_new_events` and only
  process events newer than the last marker. The pack-cache, which
  was wiped on every `index_event` call during full rebuild, is now
  reused naturally — a no-op ingest yields `cache_hit: true` on the
  next `assemble`.

## [0.1.4] - 2026-05-06

Backwards-compatible hardening release. No breaking changes to the CLI flags
or MCP tool schema; the only on-wire shape change is the removal of an
internal `stub: false` field that was never read by any client.

### Added
- `tj_core::SCHEMA_VERSION` const — single source of truth, replacing four
  inlined `"1.0"` literals across `event.rs`, `pack.rs`, and the MCP server.
- `tj_core::new_task_id()` helper — generates `tj-` plus 10 lowercase
  base32 characters (~50 bits of entropy, ≈33M-task collision threshold).
  Replaces three slightly-different inline copies.
- `TJ_CLASSIFIER_MODEL` env var — overrides the hardcoded model alias for
  both the subscription (`claude -p`) and Anthropic API classifiers.
  Defaults unchanged: `haiku` for CLI, `claude-haiku-4-5-20251001` for API.
- `AnthropicClassifier::DEFAULT_TIMEOUT` — public const for the 15-second
  HTTP request timeout (read by `from_env()`; overridable via the struct's
  `timeout` field).
- `.editorconfig` at the repo root — LF, UTF-8, 4-space Rust, 2-space YAML
  / TOML / JSON / Markdown, tab Makefile.
- CI: `msrv` job pinning Rust 1.83 to catch accidental new-feature usage.
- CI: `cargo-audit` job (`rustsec/audit-check@v2`) for security advisories.
  Marked `continue-on-error` initially; will be flipped to blocking once
  the baseline is clean.
- New regression tests: `rebuild_state_skips_malformed_jsonl_lines`,
  `classifier_times_out_on_unresponsive_server`, `new_task_id_*` (×2),
  `pack_assembler_does_not_inline_schema_version_literal`,
  `schema_version_matches_event_default`,
  `tj_classifier_model_env_var_overrides_defaults_for_both_backends`,
  `no_response_serializes_a_stub_field`,
  `concurrent_appends_do_not_interleave_bytes`.

### Changed
- `JsonlWriter` now wraps the file in `fd_lock::RwLock` and acquires an
  exclusive advisory lock around every append + `flush_durable`. Cross-
  platform: `flock` on Linux/macOS, `LockFileEx` on Windows. The internal
  `BufWriter` was removed — for the journal's traffic profile (a handful
  of events per minute) buffering offered no measurable benefit.
- `rebuild_state` now logs malformed JSONL lines via `tracing::warn!`
  with line number and parse error, then skips and continues. SQL errors
  still propagate. The returned count reflects only successfully-indexed
  events.
- `AnthropicClassifier::from_env` now reads `TJ_CLASSIFIER_MODEL` and
  applies a 15-second request timeout (`Duration::from_secs(15)`).
- `ClaudeCliClassifier::default()` now reads `TJ_CLASSIFIER_MODEL`.
- New task IDs are 10 characters of base32 instead of 6. Existing
  6-character IDs continue to work — storage is keyed by opaque string.

### Removed
- `stub: bool` field from `TaskPackResult`, `TaskPackMetadata`,
  `TaskSearchResult`, `TaskCreateResult`, `EventAddResult`, and
  `TaskCloseResult`. The field was a Phase-1 stub indicator that has
  always been `false` in production and was never documented as part of
  the public schema. A regression test (`no_response_serializes_a_stub
  _field`) guards against re-introduction.

### Fixed
- HTTP classifier no longer hangs indefinitely on a stalled connection
  (default 15-second timeout).
- `rebuild_state` no longer aborts the entire transaction on a single
  malformed JSONL line, preventing a permanently-empty SQLite mirror.
- Concurrent producers (auto-capture hook + manual `task-journal event`
  + MCP server) can no longer interleave bytes mid-line on Windows;
  POSIX append-atomicity is not enforced by NTFS.
- Six-character task IDs had a birthday-collision threshold of only
  ~4096 tasks per project; extended to 10 characters (~33M).

### Internal
- `chore(lint)`: cleared `clippy::useless_vec` and `clippy::unnecessary_
  sort_by` flags introduced in rustc 1.95, plus a small batch of
  rustfmt style adjustments — no semantic changes.
- `docs(plan)`: implementation plan landed in
  `.docs/plans/2026-05-06-v0.1.4-hardening.md`.

## [0.1.3] - 2026-05-06

### Added
- `export` subcommand: dump tasks to stdout as Markdown or JSON.
- `task-journal ui` / `tui`: interactive terminal UI for browsing
  Claude Code sessions and the conversation history of the current
  project.
- 71 new tests covering session parsing, extraction, and TUI logic.

### Changed
- README expanded with TUI walkthrough and clearer install/configuration
  guidance.

## [0.1.2] - 2026-05-05

### Added
- `task-journal backfill`: import historical tasks from existing
  Claude Code session JSONL files.
- Self-contained Claude Code plugin with built-in MCP instructions and
  npm-wrapped distribution (`claude plugin install ...`).
- Subscription-based classifier (`ClaudeCliClassifier`) — uses
  `claude -p --output-format json` with the user's Pro/Max subscription
  instead of an API key.
- Auto-capture hook integration via `install-hooks`.

### Fixed
- `data_dir()` now respects `XDG_DATA_HOME` on all platforms; CI green
  on Linux, macOS, and Windows runners.

## [0.1.1] - 2026-04-30

### Changed
- Tightened publish workflow (no `continue-on-error`).
- Dependabot configured to ignore major-version bumps for manual review.

## [0.1.0] - 2026-04-29

Initial release on crates.io.

### Added
- `task-journal-core`: append-only JSONL event log + SQLite derived
  state, with FTS5 full-text search and pack assembler.
- `task-journal-cli`: `create`, `event`, `close`, `pack`, `search`,
  `stats`, `rebuild-state`, `events list` commands.
- `task-journal-mcp`: MCP server exposing `task_create`, `event_add`,
  `task_pack`, `task_search`, `task_close`.

[Unreleased]: https://github.com/Digital-Threads/Task-Journal/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/Digital-Threads/Task-Journal/compare/v0.2.0-rc.1...v0.2.1
[0.2.0-rc.1]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.4...v0.2.0-rc.1
[0.1.4]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Digital-Threads/Task-Journal/releases/tag/v0.1.0
