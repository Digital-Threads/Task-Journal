# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/Digital-Threads/Task-Journal/compare/v0.2.0-rc.1...HEAD
[0.2.0-rc.1]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.4...v0.2.0-rc.1
[0.1.4]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/Digital-Threads/Task-Journal/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Digital-Threads/Task-Journal/releases/tag/v0.1.0
