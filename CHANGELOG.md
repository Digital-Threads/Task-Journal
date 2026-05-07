# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.5] - 2026-05-07

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
- CI: bumped MSRV from Rust 1.83 to **1.85** (workspace
  `rust-version` + GitHub Actions toolchain). The ecosystem (`rmcp`,
  `clap_lex`, etc.) now ships transitive deps that require the
  `edition2024` Cargo feature, which only stabilized in 1.85.
  Pinning each one was a losing race; one MSRV bump unblocks them
  all. 1.85 has been GA since 2025-02 and is widely available.
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
