use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;

/// One forward-only schema migration. Migrations are applied in `version`
/// order; each is recorded in `schema_migrations` so re-running `open()`
/// is idempotent.
struct Migration {
    version: i64,
    sql: &'static str,
}

const MIGRATION_001: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
  task_id        TEXT PRIMARY KEY,
  title          TEXT NOT NULL,
  status         TEXT NOT NULL,
  project_hash   TEXT NOT NULL,
  opened_at      TEXT NOT NULL,
  closed_at      TEXT,
  last_event_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_hash, last_event_at DESC);

CREATE TABLE IF NOT EXISTS events_index (
  event_id    TEXT PRIMARY KEY,
  task_id     TEXT NOT NULL,
  type        TEXT NOT NULL,
  timestamp   TEXT NOT NULL,
  confidence  REAL,
  status      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_task_time ON events_index(task_id, timestamp DESC);

CREATE TABLE IF NOT EXISTS decisions (
  decision_id    TEXT PRIMARY KEY,
  task_id        TEXT NOT NULL,
  text           TEXT NOT NULL,
  status         TEXT NOT NULL,
  superseded_by  TEXT
);

CREATE TABLE IF NOT EXISTS evidence (
  evidence_id           TEXT PRIMARY KEY,
  task_id               TEXT NOT NULL,
  text                  TEXT NOT NULL,
  strength              TEXT NOT NULL,
  refers_to_decision_id TEXT
);

CREATE TABLE IF NOT EXISTS task_pack_cache (
  task_id             TEXT NOT NULL,
  mode                TEXT NOT NULL,
  text                TEXT NOT NULL,
  generated_at        TEXT NOT NULL,
  source_event_count  INTEGER NOT NULL,
  PRIMARY KEY (task_id, mode)
);

CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
  task_id UNINDEXED,
  event_id UNINDEXED,
  text,
  type
);
"#;

/// Tracks how far we've ingested the JSONL log per project so subsequent
/// `ingest_new_events` calls can read only the tail rather than rescanning
/// the entire file. `last_indexed_event_id` is the `event_id` of the most
/// recent event written to `events_index`.
const MIGRATION_002: &str = r#"
CREATE TABLE IF NOT EXISTS index_state (
  project_hash          TEXT PRIMARY KEY,
  last_indexed_event_id TEXT NOT NULL,
  updated_at            TEXT NOT NULL
);
"#;

/// v0.4.0 task-as-goal redesign: explicit goal/outcome on tasks +
/// typed artifacts on events. NULLable so existing rows survive
/// without backfill. Wipes the pack cache so old packs (rendered
/// without Goal/Outcome blocks) regenerate on next view.
const MIGRATION_003: &str = r#"
ALTER TABLE tasks ADD COLUMN goal        TEXT;
ALTER TABLE tasks ADD COLUMN outcome     TEXT;
ALTER TABLE tasks ADD COLUMN outcome_tag TEXT;
ALTER TABLE tasks ADD COLUMN external    TEXT;
ALTER TABLE events_index ADD COLUMN artifacts TEXT;
DELETE FROM task_pack_cache;
"#;

// v0.5.0 Phase B — artifacts auto-extract on ingest. The column was
// added in v003 but stayed NULL for everyone; v004 just wipes the
// pack cache so newly-extracted artifacts surface in the next pack
// render. Existing events stay NULL until `reclassify` (Phase B+) or
// `rebuild-state` is run.
const MIGRATION_004: &str = r#"
DELETE FROM task_pack_cache;
"#;

/// v0.12.0 dream Pass A — per-project watermark of the last successful
/// dream run. Sessions modified after this are in scope for the next run.
const MIGRATION_005: &str = r#"
CREATE TABLE IF NOT EXISTS dream_state (
  project_hash    TEXT PRIMARY KEY,
  last_dream_at   TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);
"#;

/// v0.12.0 subtask hierarchy — nullable `parent_id` carries the parent
/// task on the `open` event's `meta.parent_id`. Existing flat tasks stay
/// NULL. Index supports `children_of` lookups.
const MIGRATION_006: &str = r#"
ALTER TABLE tasks ADD COLUMN parent_id TEXT;
CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_id);
"#;

/// v0.12.0 structured decision alternatives — nullable `alternatives`
/// carries the JSON array from a decision event's `meta.alternatives`
/// (objects like `{option, chosen, rationale}`). Existing decisions stay
/// NULL; the append-only log is untouched. Wipes the pack cache so packs
/// re-render with the alternatives block once events carry it.
const MIGRATION_007: &str = r#"
ALTER TABLE decisions ADD COLUMN alternatives TEXT;
DELETE FROM task_pack_cache;
"#;

/// v0.15.0 semantic-memory substrate (Pillar A). `embeddings` stores one
/// vector per event as a little-endian f32 BLOB, tagged with the model id +
/// dim so we never compare across models and can re-embed on a model change.
/// `memory_tier` is denormalised onto `events_index` for cheap tier filtering
/// (episodic by default; semantic/procedural/preference added in Phase 3).
/// Purely additive — existing rows default to `episodic`, the append-only log
/// is untouched, and an absent embedder simply leaves `embeddings` empty.
const MIGRATION_008: &str = r#"
CREATE TABLE IF NOT EXISTS embeddings (
  event_id     TEXT PRIMARY KEY,
  task_id      TEXT NOT NULL,
  project_hash TEXT NOT NULL,
  tier         TEXT NOT NULL DEFAULT 'episodic',
  model        TEXT NOT NULL,
  dim          INTEGER NOT NULL,
  vec          BLOB NOT NULL,
  created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_emb_project_tier ON embeddings(project_hash, tier);
ALTER TABLE events_index ADD COLUMN memory_tier TEXT NOT NULL DEFAULT 'episodic';
"#;

/// v0.30.0 per-session queries — `session_id` projects an event's
/// `meta.session_id`. Clearing `index_state` makes the next
/// `ingest_new_events` replay the whole log (every projection is an
/// idempotent upsert), which fills the column for events indexed before.
const MIGRATION_009: &str = r#"
ALTER TABLE events_index ADD COLUMN session_id TEXT;
CREATE INDEX IF NOT EXISTS idx_events_session_time ON events_index(session_id, timestamp);
DELETE FROM index_state;
"#;

/// v0.30.0 corrections — `corrected_by` holds the `event_id` of the
/// `correction` event whose `corrects` points at this event, so packs leave
/// the corrected event out without re-reading the log. Clearing
/// `index_state` replays the log once to fill it for existing events.
const MIGRATION_010: &str = r#"
ALTER TABLE events_index ADD COLUMN corrected_by TEXT;
DELETE FROM index_state;
"#;

/// v0.30.0 bookkeeping — `bookkeeping` flags machine-written events that are
/// not reasoning (see [`is_bookkeeping`]) so active decisions, export-pr,
/// recall and the global memory can skip them. Clearing `index_state`
/// replays the log once to flag existing events.
const MIGRATION_011: &str = r#"
ALTER TABLE events_index ADD COLUMN bookkeeping INTEGER NOT NULL DEFAULT 0;
DELETE FROM index_state;
"#;

/// v0.30.0 pack cache holds only the stable body; the header and the gaps
/// are rendered on every call. Rows cached as the whole pack text go.
const MIGRATION_012: &str = r#"
DELETE FROM task_pack_cache;
"#;

/// v0.30.0 projection marker — `projection_state` holds the `index_state` row
/// as this version last wrote it. An older binary still running (or a
/// downgrade) ingests or rebuilds without the 0.30 columns and moves
/// `index_state`; the mismatch makes the next ingest replay the log to
/// re-derive them. It starts empty, so the first ingest after the upgrade
/// replays the log once — which also fills `author`, the event's writer, so
/// only deliberate writes bind a session to a task.
const MIGRATION_013: &str = r#"
CREATE TABLE IF NOT EXISTS projection_state (
  project_hash          TEXT PRIMARY KEY,
  last_indexed_event_id TEXT NOT NULL,
  updated_at            TEXT NOT NULL
);
ALTER TABLE events_index ADD COLUMN author TEXT;
"#;

/// v0.31.0 project chronicle: modules, task ↔ module links, module history
/// lines, and a projection mark only 0.31+ writes. 0.30 writes the older
/// marks while skipping `module` lines it cannot parse; a mismatch with this
/// one makes 0.31 replay the log, so no module is lost.
const MIGRATION_014: &str = r#"
CREATE TABLE IF NOT EXISTS modules (
  project_hash TEXT NOT NULL,
  module_id    TEXT NOT NULL,
  name         TEXT NOT NULL,
  description  TEXT,
  hints        TEXT NOT NULL DEFAULT '{}',
  state        TEXT,
  state_at     TEXT,
  status       TEXT NOT NULL DEFAULT 'active',
  merged_into  TEXT,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  PRIMARY KEY (project_hash, module_id)
);
CREATE TABLE IF NOT EXISTS task_modules (
  project_hash TEXT NOT NULL,
  task_id      TEXT NOT NULL,
  module_id    TEXT NOT NULL,
  PRIMARY KEY (task_id, module_id)
);
CREATE INDEX IF NOT EXISTS idx_task_modules_module ON task_modules(project_hash, module_id);
CREATE TABLE IF NOT EXISTS module_notes (
  project_hash TEXT NOT NULL,
  event_id     TEXT NOT NULL,
  module_id    TEXT NOT NULL,
  task_id      TEXT NOT NULL,
  text         TEXT NOT NULL,
  at           TEXT NOT NULL,
  PRIMARY KEY (event_id, module_id)
);
CREATE INDEX IF NOT EXISTS idx_module_notes_module ON module_notes(project_hash, module_id, at);
CREATE TABLE IF NOT EXISTS projection_state_014 (
  project_hash          TEXT PRIMARY KEY,
  last_indexed_event_id TEXT NOT NULL,
  updated_at            TEXT NOT NULL
);
"#;

/// All schema migrations in version order. Append new entries here; never
/// edit a published migration's `sql` — write a new one instead.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: MIGRATION_001,
    },
    Migration {
        version: 2,
        sql: MIGRATION_002,
    },
    Migration {
        version: 3,
        sql: MIGRATION_003,
    },
    Migration {
        version: 4,
        sql: MIGRATION_004,
    },
    Migration {
        version: 5,
        sql: MIGRATION_005,
    },
    Migration {
        version: 6,
        sql: MIGRATION_006,
    },
    Migration {
        version: 7,
        sql: MIGRATION_007,
    },
    Migration {
        version: 8,
        sql: MIGRATION_008,
    },
    Migration {
        version: 9,
        sql: MIGRATION_009,
    },
    Migration {
        version: 10,
        sql: MIGRATION_010,
    },
    Migration {
        version: 11,
        sql: MIGRATION_011,
    },
    Migration {
        version: 12,
        sql: MIGRATION_012,
    },
    Migration {
        version: 13,
        sql: MIGRATION_013,
    },
    Migration {
        version: 14,
        sql: MIGRATION_014,
    },
];

fn apply_migrations(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
    )
    .context("create schema_migrations table")?;

    let applied: HashSet<i64> = {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations")
            .context("select applied versions")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .context("iterate schema_migrations")?;
        rows.collect::<rusqlite::Result<HashSet<_>>>()
            .context("collect applied versions")?
    };

    for migration in MIGRATIONS {
        if applied.contains(&migration.version) {
            continue;
        }

        // One IMMEDIATE transaction per migration: it takes the write lock up
        // front, so a second process opening the same fresh DB waits here and
        // then sees the version as applied instead of re-running its ALTERs.
        // The migration and its row commit together — a failure halfway rolls
        // the whole migration back instead of leaving a partial schema.
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .with_context(|| format!("begin schema migration v{:03}", migration.version))?;
        let already_applied: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            rusqlite::params![migration.version],
            |r| r.get(0),
        )?;
        if already_applied {
            continue;
        }

        tx.execute_batch(migration.sql)
            .with_context(|| format!("apply schema migration v{:03}", migration.version))?;
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![
                migration.version,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            ],
        )
        .with_context(|| {
            format!(
                "record schema migration v{:03} as applied",
                migration.version
            )
        })?;
        tx.commit()
            .with_context(|| format!("commit schema migration v{:03}", migration.version))?;
    }
    Ok(())
}

use crate::event::{Event, EventType};

pub fn upsert_task_from_event(
    conn: &Connection,
    event: &Event,
    project_hash: &str,
) -> anyhow::Result<()> {
    crate::modules::project(conn, event, project_hash)?;
    if event.event_type == EventType::Module {
        return Ok(());
    }

    match event.event_type {
        EventType::Open => {
            let title = event
                .meta
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.text)
                .to_string();
            let parent_id = event
                .meta
                .get("parent_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // ON CONFLICT intentionally does not overwrite parent_id — parent
            // is set once at creation; re-parenting is a separate future path.
            conn.execute(
                "INSERT INTO tasks(task_id, title, status, project_hash, opened_at, last_event_at, parent_id)
                 VALUES (?1, ?2, 'open', ?3, ?4, ?4, ?5)
                 ON CONFLICT(task_id) DO UPDATE SET last_event_at = ?4",
                rusqlite::params![event.task_id, title, project_hash, event.timestamp, parent_id],
            )?;

            // Goal and external refs given at creation ride in the open
            // event's meta so a rebuild from the JSONL restores them. A replay
            // over an existing row keeps a goal changed since.
            if let Some(goal) = event.meta.get("goal").and_then(|v| v.as_str()) {
                conn.execute(
                    "UPDATE tasks SET goal = COALESCE(goal, ?2) WHERE task_id = ?1",
                    rusqlite::params![event.task_id, goal],
                )?;
            }
            for reference in meta_strings(&event.meta, "external") {
                add_task_external(conn, &event.task_id, reference)?;
            }
        }
        EventType::Amend => {
            if let Some(goal) = event.meta.get("goal").and_then(|v| v.as_str()) {
                set_task_goal(conn, &event.task_id, goal)?;
            }
            for reference in meta_strings(&event.meta, "external_add") {
                add_task_external(conn, &event.task_id, reference)?;
            }
        }
        EventType::Close => {
            conn.execute(
                "UPDATE tasks SET status='closed', closed_at=?2, last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
            )?;
            // Restore closure metadata from the event so the recorded outcome
            // survives a full rebuild_state replay — the tasks row is rebuilt
            // from events, and set_task_outcome's direct DB write would be lost.
            if let Some(outcome) = event.meta.get("outcome").and_then(|v| v.as_str()) {
                let tag = event.meta.get("outcome_tag").and_then(|v| v.as_str());
                conn.execute(
                    "UPDATE tasks SET outcome=?2, outcome_tag=?3 WHERE task_id=?1",
                    rusqlite::params![event.task_id, outcome, tag],
                )?;
            }
        }
        EventType::Reopen => {
            conn.execute(
                "UPDATE tasks SET status='open', closed_at=NULL, last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
            )?;
        }
        EventType::Rename => {
            // The new human-readable title is the event text. Replaying the
            // JSONL in order means the last Rename wins — exactly what we want.
            let title = event
                .meta
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.text);
            conn.execute(
                "UPDATE tasks SET title=?2, last_event_at=?3 WHERE task_id=?1",
                rusqlite::params![event.task_id, title, event.timestamp],
            )?;
        }
        _ => {
            conn.execute(
                "UPDATE tasks SET last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
            )?;
        }
    }
    Ok(())
}

/// The string items of the JSON array at `meta[key]`; empty when absent.
fn meta_strings<'a>(meta: &'a serde_json::Value, key: &str) -> impl Iterator<Item = &'a str> {
    meta.get(key)
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
}

use std::io::BufRead;

pub fn list_all_projects(state_dir: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    let dir = state_dir.as_ref();
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("sqlite") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        }
    }
    Ok(out)
}

/// Replay the whole log from scratch: `search_fts` is cleared once, then every
/// event is indexed again. Commits in chunks like [`ingest_new_events`].
pub fn rebuild_state(
    conn: &Connection,
    jsonl_path: impl AsRef<Path>,
    project_hash: &str,
) -> anyhow::Result<usize> {
    replay(conn, jsonl_path.as_ref(), project_hash, true)
}

/// The events of a JSONL log in order. Malformed lines are skipped with a
/// warning so that one bad event cannot abort an otherwise-recoverable replay;
/// read errors still propagate.
fn log_events(f: std::fs::File) -> impl Iterator<Item = anyhow::Result<Event>> {
    std::io::BufReader::new(f)
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let line = match line.with_context(|| format!("read line {i}")) {
                Ok(line) => line,
                Err(e) => return Some(Err(e)),
            };
            if line.trim().is_empty() {
                return None;
            }

            match serde_json::from_str(&line) {
                Ok(event) => Some(Ok(event)),
                Err(err) => {
                    tracing::warn!(
                        line_number = i + 1,
                        error = %err,
                        "skipping malformed JSONL line"
                    );
                    None
                }
            }
        })
}

/// Returns whether a task with this id has been recorded in the derived
/// state. Cheap O(1) lookup against the `tasks` primary key. Callers
/// should run [`ingest_new_events`] first if they want to see the latest
/// JSONL state.
pub fn task_exists(conn: &Connection, task_id: &str) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE task_id = ?1",
        rusqlite::params![task_id],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Status string for an existing task (e.g. "open", "closed"). Returns
/// `None` when the task is unknown — caller decides whether that's a
/// hard error or a route-to-pending case.
pub fn task_status(conn: &Connection, task_id: &str) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT status FROM tasks WHERE task_id = ?1")?;
    let mut rows = stmt.query(rusqlite::params![task_id])?;
    Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
}

/// Set or replace `tasks.goal` for an existing task. Caller is
/// expected to have validated the task exists (via `task_exists`); we
/// don't error on no-op rows so the upsert pattern is uniform.
pub fn set_task_goal(conn: &Connection, task_id: &str, goal: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE tasks SET goal = ?1 WHERE task_id = ?2",
        rusqlite::params![goal, task_id],
    )
    .with_context(|| format!("set goal for {task_id}"))?;
    // Pack cache is now stale for this task — drop the entry so the
    // next render picks up the new goal.
    conn.execute(
        "DELETE FROM task_pack_cache WHERE task_id = ?1",
        rusqlite::params![task_id],
    )?;
    Ok(())
}

/// Set or replace the closure metadata. Pass `None` for `outcome_tag`
/// to leave it unset; pass `Some("done"|"abandoned"|"superseded")`
/// for a structured tag. Free-text `outcome` is the primary field.
pub fn set_task_outcome(
    conn: &Connection,
    task_id: &str,
    outcome: &str,
    outcome_tag: Option<&str>,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE tasks SET outcome = ?1, outcome_tag = ?2 WHERE task_id = ?3",
        rusqlite::params![outcome, outcome_tag, task_id],
    )
    .with_context(|| format!("set outcome for {task_id}"))?;
    conn.execute(
        "DELETE FROM task_pack_cache WHERE task_id = ?1",
        rusqlite::params![task_id],
    )?;
    Ok(())
}

/// Append an external reference to `tasks.external`. The column is
/// stored as a comma-separated list — small, append-mostly. A reference
/// already in the list is not added again, so replaying the events that
/// carry it is idempotent; an unknown task is a no-op. Acceptable shapes
/// (loose, not enforced): `beads:claude-memory-rsw`, `github:#42`,
/// `jira:PROJ-1234`.
pub fn add_task_external(conn: &Connection, task_id: &str, reference: &str) -> anyhow::Result<()> {
    let current: Option<Option<String>> = conn
        .query_row(
            "SELECT external FROM tasks WHERE task_id = ?1",
            rusqlite::params![task_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .with_context(|| format!("read external for {task_id}"))?;
    let Some(current) = current else {
        return Ok(());
    };
    if current
        .as_deref()
        .is_some_and(|s| s.split(',').any(|r| r == reference))
    {
        return Ok(());
    }

    let next = match current {
        Some(s) if !s.is_empty() => format!("{s},{reference}"),
        _ => reference.to_string(),
    };
    conn.execute(
        "UPDATE tasks SET external = ?1 WHERE task_id = ?2",
        rusqlite::params![next, task_id],
    )?;
    conn.execute(
        "DELETE FROM task_pack_cache WHERE task_id = ?1",
        rusqlite::params![task_id],
    )?;
    Ok(())
}

/// Find the task whose `external` list contains exactly `reference` (one of the
/// comma-separated tokens). Used to make a journal idempotent by external id —
/// e.g. resolve `loom:t-abc` back to its task. Returns the most recently
/// touched match, or None.
pub fn task_id_by_external(conn: &Connection, reference: &str) -> anyhow::Result<Option<String>> {
    let pattern = format!("%,{reference},%");
    let id: Option<String> = conn
        .query_row(
            "SELECT task_id FROM tasks WHERE ',' || external || ',' LIKE ?1 ORDER BY rowid DESC LIMIT 1",
            rusqlite::params![pattern],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(id)
}

/// The open task of `project_hash` whose most recent event carries
/// `meta.session_id == session_id` — the task a live agent session is
/// working on. `None` when the session has no event on an open task.
///
/// Only deliberate writes count: hooks and the classifier fall back to the
/// newest open task and stamp the current session, so their events (and
/// bookkeeping) would bind a fresh session to another session's task.
pub fn active_task_for_session(
    conn: &Connection,
    project_hash: &str,
    session_id: &str,
) -> anyhow::Result<Option<String>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT ei.task_id FROM events_index ei
             JOIN tasks t ON t.task_id = ei.task_id
             WHERE ei.session_id = ?2 AND t.project_hash = ?1 AND t.status = 'open'
               AND ei.author IN ('user', 'agent') AND ei.bookkeeping = 0
             ORDER BY ei.timestamp DESC LIMIT 1",
            rusqlite::params![project_hash, session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(id)
}

/// Read-only metadata bundle used by pack rendering (and TUI list
/// teasers in v0.4.0+). Returns `None` for unknown tasks.
#[derive(Debug, Clone, Default)]
pub struct TaskMetadata {
    pub goal: Option<String>,
    pub outcome: Option<String>,
    pub outcome_tag: Option<String>,
    pub external: Option<String>,
}

pub fn task_metadata(conn: &Connection, task_id: &str) -> anyhow::Result<Option<TaskMetadata>> {
    let mut stmt =
        conn.prepare("SELECT goal, outcome, outcome_tag, external FROM tasks WHERE task_id = ?1")?;
    let mut rows = stmt.query(rusqlite::params![task_id])?;
    Ok(match rows.next()? {
        Some(r) => Some(TaskMetadata {
            goal: r.get::<_, Option<String>>(0)?,
            outcome: r.get::<_, Option<String>>(1)?,
            outcome_tag: r.get::<_, Option<String>>(2)?,
            external: r.get::<_, Option<String>>(3)?,
        }),
        None => None,
    })
}

/// One row of the stale-task report: an open task whose last event
/// crossed the inactivity threshold.
#[derive(Debug, Clone)]
pub struct StaleTask {
    pub task_id: String,
    pub title: String,
    pub last_event_at: String,
    pub days_idle: i64,
}

/// Find open tasks with no event in the last `days` days. Sorted by
/// idle time descending so the user sees the most ancient first.
pub fn stale_tasks(conn: &Connection, days: i64) -> anyhow::Result<Vec<StaleTask>> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    let cutoff_str = cutoff.to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT task_id, title, last_event_at FROM tasks
         WHERE status = 'open' AND last_event_at < ?1
         ORDER BY last_event_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![cutoff_str], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let now = chrono::Utc::now();
    let mut out = Vec::new();
    for row in rows {
        let (task_id, title, last_at) = row?;
        let dt = chrono::DateTime::parse_from_rfc3339(&last_at)
            .map(|d| d.with_timezone(&chrono::Utc))
            .unwrap_or(now);
        let days_idle = (now - dt).num_days();
        out.push(StaleTask {
            task_id,
            title,
            last_event_at: last_at,
            days_idle,
        });
    }
    Ok(out)
}

/// Score-weighted relationship between a fresh prompt's artifacts and
/// every prior task's artifacts. Higher score = stronger continuation
/// signal. Threshold tuning is the caller's job; v0.6.0 auto-link
/// keeps anything with score > 0.0.
#[derive(Debug, Clone)]
pub struct RelatedTask {
    pub task_id: String,
    pub status: String,
    pub score: f64,
}

/// Find tasks whose events overlap the given artifacts on any
/// dimension we have a signal for. Weights:
///   shared linked_issue → +1.0   (strongest, ticket id is unique)
///   shared commit_hash  → +0.8   (commits are nearly unique)
///   shared file path    → +0.3   (files churn across tasks)
///
/// The scan reads `events_index.artifacts` (JSON) directly with LIKE
/// substring matches — JSON1 would be cleaner but keeps the codepath
/// dependency-free. Returns top hits sorted by score desc; ties keep
/// the most-recent task first.
pub fn find_related_tasks(
    conn: &Connection,
    arts: &crate::artifacts::Artifacts,
) -> anyhow::Result<Vec<RelatedTask>> {
    use std::collections::HashMap;
    if arts.is_empty() {
        return Ok(Vec::new());
    }
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut last_seen: HashMap<String, String> = HashMap::new();

    let needles: Vec<(String, f64)> = arts
        .linked_issues
        .iter()
        .map(|s| (s.clone(), 1.0))
        .chain(arts.commit_hashes.iter().map(|s| (s.clone(), 0.8)))
        .chain(arts.files.iter().map(|s| (s.clone(), 0.3)))
        .collect();

    for (needle, weight) in needles {
        let pattern = format!("%\"{}\"%", needle.replace('%', "\\%"));
        let mut stmt = conn.prepare(
            "SELECT DISTINCT task_id, MAX(timestamp) as ts FROM events_index
             WHERE artifacts LIKE ?1
             GROUP BY task_id
             ORDER BY ts DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, ts) = row?;
            *scores.entry(id.clone()).or_insert(0.0) += weight;
            last_seen.insert(id, ts);
        }
    }

    let mut out: Vec<RelatedTask> = Vec::with_capacity(scores.len());
    for (id, score) in scores {
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM tasks WHERE task_id = ?1",
                rusqlite::params![&id],
                |r| r.get(0),
            )
            .ok();
        if let Some(status) = status {
            out.push(RelatedTask {
                task_id: id,
                status,
                score,
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ts_a = last_seen.get(&a.task_id).cloned().unwrap_or_default();
                let ts_b = last_seen.get(&b.task_id).cloned().unwrap_or_default();
                ts_b.cmp(&ts_a)
            })
    });
    Ok(out)
}

/// Find tasks (open or closed) whose events reference any of the given
/// issue identifiers (FIN-868, JIRA-123, INC-7…). Looks at the
/// per-event `artifacts.linked_issues` column populated on ingest.
/// Returns `(task_id, status)` deduplicated, most-recent first. Used
/// by the v0.5.0 Phase C auto-link flow to recognise that a fresh
/// prompt is a continuation of a prior task.
pub fn find_tasks_by_linked_issues(
    conn: &Connection,
    issues: &[String],
) -> anyhow::Result<Vec<(String, String)>> {
    if issues.is_empty() {
        return Ok(Vec::new());
    }
    // Stage A: collect candidate task_ids whose events_index.artifacts
    // contains any of the requested issue strings. JSON1 is overkill
    // here — a substring LIKE on the raw JSON is correct given the
    // ticket id format ("FIN-868") never appears outside its own
    // linked_issues array.
    let mut candidate_ids: Vec<String> = Vec::new();
    for issue in issues {
        let pattern = format!("%\"{}\"%", issue.replace('%', "\\%"));
        let mut stmt = conn.prepare(
            "SELECT DISTINCT task_id FROM events_index
             WHERE artifacts LIKE ?1
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], |r| r.get::<_, String>(0))?;
        for r in rows {
            let id = r?;
            if !candidate_ids.contains(&id) {
                candidate_ids.push(id);
            }
        }
    }
    // Stage B: hydrate status for each candidate.
    let mut out = Vec::with_capacity(candidate_ids.len());
    for id in candidate_ids {
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM tasks WHERE task_id = ?1",
                rusqlite::params![&id],
                |r| r.get(0),
            )
            .ok();
        if let Some(s) = status {
            out.push((id, s));
        }
    }
    Ok(out)
}

/// Re-run artifact extraction over every event of a task and write the
/// result back to `events_index.artifacts`. Used to backfill events
/// that were ingested before Phase B landed. Returns the number of
/// events touched. Wipes the pack cache for the task so the next
/// render reflects the freshly extracted artifacts.
pub fn reclassify_task_artifacts(conn: &Connection, task_id: &str) -> anyhow::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT ei.event_id, COALESCE(sf.text, '') FROM events_index ei
         LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.task_id = ?1",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![task_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<_, _>>()?;
    let count = rows.len();
    for (event_id, text) in rows {
        let arts = crate::artifacts::extract(&text);
        let json = if arts.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&arts)?)
        };
        conn.execute(
            "UPDATE events_index SET artifacts = ?1 WHERE event_id = ?2",
            rusqlite::params![json, event_id],
        )?;
    }
    invalidate_pack_cascade(conn, task_id)?;
    Ok(count)
}

/// Aggregate artifacts (commit hashes, PR URLs, ticket IDs, files,
/// branches) across every event of a task, deduplicated. Reads the
/// per-event JSON payload that `ingest_new_events` populated. Skips
/// events whose `artifacts` column is NULL or unparseable rather than
/// failing the pack render, and corrected events, so a wrong hash or path
/// stops raising a gap once a correction retires it.
pub fn task_artifacts(
    conn: &Connection,
    task_id: &str,
) -> anyhow::Result<crate::artifacts::Artifacts> {
    let mut stmt = conn.prepare(
        "SELECT artifacts FROM events_index
         WHERE task_id = ?1 AND artifacts IS NOT NULL AND corrected_by IS NULL
         ORDER BY timestamp ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| r.get::<_, String>(0))?;
    let mut acc = crate::artifacts::Artifacts::default();
    for row in rows {
        let json = row?;
        if let Ok(parsed) = serde_json::from_str::<crate::artifacts::Artifacts>(&json) {
            acc.merge(parsed);
        }
    }
    Ok(acc)
}

/// How far the log is indexed for a project: the `index_state` row as
/// `(last_indexed_event_id, updated_at)`. `None` when the project has never
/// been indexed (first call, or a migration cleared the marker).
type IndexMark = (String, String);

/// Marks every write records: `index_state` (all versions),
/// `projection_state` (0.30 — kept so a 0.30 binary goes on incrementally),
/// `projection_state_014` (0.31+, which projects modules).
const MARK_TABLES: [&str; 3] = ["index_state", "projection_state", "projection_state_014"];

/// The mark only this version writes: equal to `index_state` means nothing
/// older indexed since.
const OWN_MARK: &str = "projection_state_014";

/// The mark stored in `table`: `index_state`, which every version writes, or
/// [`OWN_MARK`], the copy only this version writes.
fn read_mark(
    conn: &Connection,
    table: &str,
    project_hash: &str,
) -> anyhow::Result<Option<IndexMark>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT last_indexed_event_id, updated_at FROM {table} WHERE project_hash = ?1"
            ),
            rusqlite::params![project_hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

fn record_last_indexed(
    conn: &Connection,
    project_hash: &str,
    event_id: &str,
) -> anyhow::Result<IndexMark> {
    let mark = (
        event_id.to_string(),
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    );
    for table in MARK_TABLES {
        conn.execute(
            &format!(
                "INSERT INTO {table}(project_hash, last_indexed_event_id, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_hash) DO UPDATE SET
                     last_indexed_event_id = excluded.last_indexed_event_id,
                     updated_at = excluded.updated_at"
            ),
            rusqlite::params![project_hash, mark.0, mark.1],
        )?;
    }

    Ok(mark)
}

/// Events a replay indexes per transaction. Each commit moves the marker, so
/// a replay killed partway (a hook timeout) resumes after its last chunk
/// instead of starting over, and other writers wait one chunk at most.
const REPLAY_CHUNK: usize = 500;

/// Read only the tail of the JSONL log since the last call. The cheap path
/// for hot loops (every MCP tool invocation): scan to the marker, ingest
/// the rest, update the marker.
///
/// Falls back to a full replay (see [`rebuild_state`]) in three cases:
/// - No marker yet for this project (first call after a migration cleared
///   it, or a brand-new install).
/// - The marker is not the one this version last wrote: an older binary
///   ingested or rebuilt without the columns or lines it does not know
///   (0.30 skips `module` lines).
/// - The stored marker is not present in the JSONL (corrupted / truncated
///   file). A `tracing::warn!` is emitted so the operator notices.
pub fn ingest_new_events(
    conn: &Connection,
    jsonl_path: impl AsRef<Path>,
    project_hash: &str,
) -> anyhow::Result<usize> {
    replay(conn, jsonl_path.as_ref(), project_hash, false)
}

/// Index the log after the marker — or all of it when `from_scratch` or there
/// is no usable marker, clearing `search_fts` first — in chunks of
/// [`REPLAY_CHUNK`] events. Returns how many events it indexed.
fn replay(
    conn: &Connection,
    jsonl_path: &Path,
    project_hash: &str,
    mut from_scratch: bool,
) -> anyhow::Result<usize> {
    let mut count = 0;

    'pass: loop {
        let f = match std::fs::File::open(jsonl_path) {
            Ok(f) => f,
            // A fresh project has no events log on disk yet — there is simply nothing
            // to read, which is not an error (this is what crashed task_create the
            // first time the journal was touched in a new worktree).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(count),
            Err(e) => return Err(anyhow::Error::new(e).context(format!("open {jsonl_path:?}"))),
        };
        let mut events = log_events(f);
        let mut committed: Option<IndexMark> = None;

        loop {
            // IMMEDIATE: the marker is read under the write lock, so two
            // processes never index the same chunk.
            let tx = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let mark = read_mark(&tx, "index_state", project_hash)?;

            if committed.is_none() {
                let ours = mark == read_mark(&tx, OWN_MARK, project_hash)?;
                if mark.is_some() && !ours {
                    tracing::warn!(
                        project_hash = project_hash,
                        "index_state was written by another version — replaying the log"
                    );
                }

                match mark.filter(|_| !from_scratch && ours) {
                    None => {
                        clear_search_fts(&tx, project_hash)?;
                        crate::modules::clear(&tx, project_hash)?;
                    }
                    Some((marker, _)) => {
                        let mut found = false;
                        for event in events.by_ref() {
                            if event?.event_id == marker {
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            tracing::warn!(
                                project_hash = project_hash,
                                marker = marker.as_str(),
                                "last_indexed_event_id not found in JSONL — falling back to full rebuild"
                            );
                            from_scratch = true;
                            continue 'pass;
                        }
                    }
                }
            } else if mark != committed {
                // Another process indexed past our last chunk: go on from its marker.
                from_scratch = false;
                continue 'pass;
            }

            let mut in_chunk = 0;
            let mut last_event_id = None;
            for event in events.by_ref().take(REPLAY_CHUNK) {
                let event = event?;
                upsert_task_from_event(&tx, &event, project_hash)?;
                index_event(&tx, &event)?;
                last_event_id = Some(event.event_id);
                in_chunk += 1;
            }

            if let Some(eid) = last_event_id {
                committed = Some(record_last_indexed(&tx, project_hash, &eid)?);
            }
            tx.commit()?;
            count += in_chunk;

            if in_chunk < REPLAY_CHUNK {
                return Ok(count);
            }
        }
    }
}

/// Drop this project's rows from `search_fts` before a replay re-inserts
/// them: one scan, where deleting per event scanned the table every time.
fn clear_search_fts(conn: &Connection, project_hash: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM search_fts
         WHERE task_id NOT IN (SELECT task_id FROM tasks WHERE project_hash <> ?1)",
        rusqlite::params![project_hash],
    )?;

    Ok(())
}

/// The `search_fts` rowid of an event: a stable FNV-1a hash of its id.
/// `event_id` is UNINDEXED, so finding an event's row by it scans the whole
/// table; by rowid, re-indexing an event replaces its row in O(log n).
// ponytail: two ids hashing alike (odds ~n²/2^64) would share one search row;
// a mapping table removes that if it ever matters.
fn fts_rowid(event_id: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in event_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }

    (hash >> 1) as i64
}

/// Machine-written bookkeeping, not reasoning: the PreCompact boundary marker
/// (a `decision`) and the model-switch note (a `constraint`). Known by
/// `meta.kind`; events written before the writers set it, by text prefix.
pub fn is_bookkeeping(event: &Event) -> bool {
    match event.meta.get("kind").and_then(|v| v.as_str()) {
        Some(kind) => matches!(kind, "compaction_marker" | "model_switch"),
        None => match event.event_type {
            EventType::Decision => event.text.starts_with("Conversation compacted at"),
            EventType::Constraint => event
                .text
                .starts_with(crate::reminder::MODEL_SWITCH_TEXT_PREFIX),
            _ => false,
        },
    }
}

pub fn index_event(conn: &Connection, event: &Event) -> anyhow::Result<()> {
    // An amend only changes task metadata, which upsert_task_from_event
    // applies. It is not reasoning: keeping it out of events_index and
    // search_fts keeps it out of packs, search, recall and memory sync.
    if event.event_type == EventType::Amend {
        return invalidate_pack_cascade(conn, &event.task_id);
    }
    // A module event describes the project's map, not a task.
    if event.event_type == EventType::Module {
        return Ok(());
    }

    let type_str = serde_json::to_value(event.event_type)?
        .as_str()
        .unwrap()
        .to_string();
    let status_str = serde_json::to_value(event.status)?
        .as_str()
        .unwrap()
        .to_string();
    // v0.5.0 Phase B: scrape artifacts (commit hashes, PR URLs, ticket
    // IDs, file paths, branch names) out of the event text. Storing
    // per-event so reclassify can recompute without touching foreign
    // events; pack aggregates and dedupes across events at render time.
    let mut artifacts = crate::artifacts::extract(&event.text);
    // v0.26.5: structured artifacts harvested deterministically at close
    // (git/gh: PR url, commit, branch) ride in `event.meta["artifacts"]`.
    // Merge them so reliable refs land without depending on the lossy text
    // regex — this is what turns a closed task into a clickable Loom card.
    if let Some(meta_arts) = event
        .meta
        .get("artifacts")
        .cloned()
        .and_then(|v| serde_json::from_value::<crate::artifacts::Artifacts>(v).ok())
    {
        artifacts.merge(meta_arts);
    }
    let artifacts_json = if artifacts.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&artifacts)?)
    };
    let session_id = event.meta.get("session_id").and_then(|v| v.as_str());
    let author_str = serde_json::to_value(event.author)?
        .as_str()
        .unwrap()
        .to_string();
    conn.execute(
        "INSERT OR REPLACE INTO events_index(event_id, task_id, type, timestamp, confidence, status, artifacts, session_id, bookkeeping, author)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            event.event_id, event.task_id, type_str,
            event.timestamp, event.confidence, status_str, artifacts_json, session_id,
            is_bookkeeping(event), author_str
        ],
    )?;
    // search_fts has no PK; replacing by the event's own rowid keeps it
    // idempotent across replays.
    conn.execute(
        "INSERT OR REPLACE INTO search_fts(rowid, task_id, event_id, text, type)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            fts_rowid(&event.event_id),
            event.task_id,
            event.event_id,
            event.text,
            type_str
        ],
    )?;

    if event.event_type == EventType::Decision {
        // v0.12.0: project structured alternatives (meta.alternatives) into
        // a dedicated column so pack can render "considered A/B/C, chose X".
        // Stored as the verbatim JSON of the meta value; NULL when absent.
        let alternatives_json = match event.meta.get("alternatives") {
            Some(v) if !v.is_null() => Some(serde_json::to_string(v)?),
            _ => None,
        };
        conn.execute(
            "INSERT OR REPLACE INTO decisions(decision_id, task_id, text, status, alternatives)
             VALUES (?1, ?2, ?3, 'active', ?4)",
            rusqlite::params![event.event_id, event.task_id, event.text, alternatives_json],
        )?;
    }

    if event.event_type == EventType::Supersede {
        if let Some(target) = &event.supersedes {
            conn.execute(
                "UPDATE decisions SET status='superseded', superseded_by=?1 WHERE decision_id=?2",
                rusqlite::params![event.event_id, target],
            )?;
        }
    }

    // A correction retires the event it corrects from packs and export-pr.
    // The target may sit in another task, whose cached pack is now stale too.
    if event.event_type == EventType::Correction {
        if let Some(target) = &event.corrects {
            conn.execute(
                "UPDATE events_index SET corrected_by=?1 WHERE event_id=?2",
                rusqlite::params![event.event_id, target],
            )?;
            let target_task: Option<String> = conn
                .query_row(
                    "SELECT task_id FROM events_index WHERE event_id=?1",
                    rusqlite::params![target],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(t) = target_task.filter(|t| *t != event.task_id) {
                invalidate_pack_cascade(conn, &t)?;
            }
        }
    }

    if event.event_type == EventType::Evidence {
        let strength_str = event
            .evidence_strength
            .map(|s| {
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .unwrap_or_else(|| "medium".into());
        conn.execute(
            "INSERT OR REPLACE INTO evidence(evidence_id, task_id, text, strength)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![event.event_id, event.task_id, event.text, strength_str],
        )?;
    }

    // Invalidate any cached pack for this task — and its parent, whose
    // Subtasks roll-up depends on this child.
    invalidate_pack_cascade(conn, &event.task_id)?;

    Ok(())
}

/// Switch the DB to WAL. On a fresh file that needs an exclusive lock, and
/// SQLite answers SQLITE_BUSY at once — no busy handler, to avoid a deadlock —
/// when another connection is switching the same file: two processes opening
/// a fresh DB together. Retry briefly; once the file is in WAL it is a no-op.
fn enable_wal(conn: &Connection) -> anyhow::Result<()> {
    let mut attempts = 0;
    loop {
        match conn.execute_batch("PRAGMA journal_mode=WAL;") {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy && attempts < 100 =>
            {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            res => return res.context("set WAL journal mode"),
        }
    }
}

pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
    }
    let conn =
        Connection::open(&path).with_context(|| format!("open SQLite at {:?}", path.as_ref()))?;
    enable_wal(&conn)?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    apply_migrations(&conn).context("apply schema migrations")?;
    Ok(conn)
}

/// One row of the task list rendered by the TUI: enough to render the
/// list view without round-tripping for each task. `event_count` joins
/// `events_index` so we don't need a second query per row.
#[derive(Debug, Clone)]
pub struct TaskRow {
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub last_event_at: String,
    pub event_count: usize,
}

/// All tasks for a project, ordered with open ones first (by recency)
/// then closed ones. The TUI list view binds directly to this — there
/// is no other consumer, so the shape is tuned for that callsite.
pub fn list_tasks_by_project(
    conn: &Connection,
    project_hash: &str,
) -> anyhow::Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.task_id, t.title, t.status, t.last_event_at,
                COALESCE(c.cnt, 0) AS event_count
         FROM tasks t
         LEFT JOIN (
             SELECT task_id, COUNT(*) AS cnt FROM events_index GROUP BY task_id
         ) c ON c.task_id = t.task_id
         WHERE t.project_hash = ?1
         ORDER BY (t.status = 'open') DESC, t.last_event_at DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![project_hash], |r| {
            Ok(TaskRow {
                task_id: r.get::<_, String>(0)?,
                title: r.get::<_, String>(1)?,
                status: r.get::<_, String>(2)?,
                last_event_at: r.get::<_, String>(3)?,
                event_count: r.get::<_, i64>(4)? as usize,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Top-level tasks for a project (those with no parent), ordered like
/// `list_tasks_by_project` — open first, then by recency. The roots of
/// the `list --tree` view.
pub fn top_level_tasks(conn: &Connection, project_hash: &str) -> anyhow::Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.task_id, t.title, t.status, t.last_event_at,
                COALESCE(c.cnt, 0) AS event_count
         FROM tasks t
         LEFT JOIN (
             SELECT task_id, COUNT(*) AS cnt FROM events_index GROUP BY task_id
         ) c ON c.task_id = t.task_id
         WHERE t.project_hash = ?1 AND t.parent_id IS NULL
         ORDER BY (t.status = 'open') DESC, t.last_event_at DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![project_hash], |r| {
            Ok(TaskRow {
                task_id: r.get::<_, String>(0)?,
                title: r.get::<_, String>(1)?,
                status: r.get::<_, String>(2)?,
                last_event_at: r.get::<_, String>(3)?,
                event_count: r.get::<_, i64>(4)? as usize,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Direct children of a task (one level), newest activity first.
pub fn children_of(conn: &Connection, task_id: &str) -> anyhow::Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.task_id, t.title, t.status, t.last_event_at,
                COALESCE(c.cnt, 0) AS event_count
         FROM tasks t
         LEFT JOIN (
             SELECT task_id, COUNT(*) AS cnt FROM events_index GROUP BY task_id
         ) c ON c.task_id = t.task_id
         WHERE t.parent_id = ?1
         ORDER BY (t.status = 'open') DESC, t.last_event_at DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![task_id], |r| {
            Ok(TaskRow {
                task_id: r.get::<_, String>(0)?,
                title: r.get::<_, String>(1)?,
                status: r.get::<_, String>(2)?,
                last_event_at: r.get::<_, String>(3)?,
                event_count: r.get::<_, i64>(4)? as usize,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The stored parent of a task, if any.
pub fn parent_of(conn: &Connection, task_id: &str) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT parent_id FROM tasks WHERE task_id = ?1")?;
    let mut rows = stmt.query(rusqlite::params![task_id])?;
    Ok(match rows.next()? {
        Some(r) => r.get::<_, Option<String>>(0)?,
        None => None,
    })
}

/// True if setting `new_parent` as the parent of `task_id` would create a
/// cycle (i.e. `new_parent` is `task_id` itself or a descendant of it).
/// Walks ancestors of `new_parent`; a depth cap guards against pre-existing
/// corrupt cycles.
pub fn would_create_cycle(
    conn: &Connection,
    task_id: &str,
    new_parent: &str,
) -> anyhow::Result<bool> {
    if task_id == new_parent {
        return Ok(true);
    }
    let mut cursor = Some(new_parent.to_string());
    for _ in 0..64 {
        let Some(cur) = cursor else {
            return Ok(false);
        };
        if cur == task_id {
            return Ok(true);
        }
        cursor = parent_of(conn, &cur)?;
    }
    // Depth cap exceeded — treat as a cycle to be safe.
    Ok(true)
}

/// Number of direct children of `task_id` whose status is still open.
pub fn count_open_children(conn: &Connection, task_id: &str) -> anyhow::Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE parent_id = ?1 AND status = 'open'",
        rusqlite::params![task_id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// Clear the pack cache for a task and its parent (roll-up depends on both).
pub fn invalidate_pack_cascade(conn: &Connection, task_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM task_pack_cache WHERE task_id = ?1",
        rusqlite::params![task_id],
    )?;
    if let Some(parent) = parent_of(conn, task_id)? {
        conn.execute(
            "DELETE FROM task_pack_cache WHERE task_id = ?1",
            rusqlite::params![parent],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Semantic-memory substrate (Pillar A / schema v008).
// ---------------------------------------------------------------------------

/// One event awaiting an embedding: its id, task, and the text to embed.
pub struct PendingEmbed {
    pub event_id: String,
    pub task_id: String,
    pub text: String,
}

/// Events that have no up-to-date embedding for `model` — either never embedded
/// or embedded by a different model. Pulls the text straight from `search_fts`.
/// `limit` bounds the batch; pass a large value to drain.
pub fn events_needing_embedding(
    conn: &Connection,
    model: &str,
    limit: usize,
) -> anyhow::Result<Vec<PendingEmbed>> {
    let mut stmt = conn.prepare(
        "SELECT f.event_id, f.task_id, f.text
           FROM search_fts f
           LEFT JOIN embeddings e ON e.event_id = f.event_id AND e.model = ?1
          WHERE e.event_id IS NULL
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![model, limit as i64], |r| {
        Ok(PendingEmbed {
            event_id: r.get(0)?,
            task_id: r.get(1)?,
            text: r.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Upsert one vector. Keyed on `event_id`, so re-embedding (e.g. after a model
/// change) replaces the prior row idempotently across `rebuild_state` replays.
#[allow(clippy::too_many_arguments)]
pub fn upsert_embedding(
    conn: &Connection,
    event_id: &str,
    task_id: &str,
    project_hash: &str,
    tier: &str,
    model: &str,
    dim: usize,
    vec: &[f32],
    created_at: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO embeddings(event_id, task_id, project_hash, tier, model, dim, vec, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            event_id,
            task_id,
            project_hash,
            tier,
            model,
            dim as i64,
            crate::embed::to_blob(vec),
            created_at
        ],
    )?;
    Ok(())
}

/// High-signal events (decisions, constraints, rejections) for consolidation —
/// `(event_id, text)`, newest first, capped at `limit`.
pub fn high_signal_events(
    conn: &Connection,
    limit: usize,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT f.event_id, f.text
           FROM search_fts f
           JOIN events_index ei ON ei.event_id = f.event_id
          WHERE f.type IN ('decision', 'constraint', 'rejection')
          ORDER BY ei.timestamp DESC
          LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// First task whose title exactly matches `title`, if any — used to find the
/// reusable per-project consolidation task.
pub fn find_task_by_title(conn: &Connection, title: &str) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT task_id FROM tasks WHERE title = ?1 LIMIT 1")?;
    let mut rows = stmt.query(rusqlite::params![title])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Texts of all events under a task (for de-duplicating consolidated facts).
pub fn task_event_texts(conn: &Connection, task_id: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT text FROM search_fts WHERE task_id = ?1")?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Number of stored embeddings for a project (test/stats helper).
pub fn count_embeddings(conn: &Connection, project_hash: &str) -> anyhow::Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE project_hash = ?1",
        rusqlite::params![project_hash],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// Embed up to `limit` events that still need a vector for the embedder's model,
/// and store them. Returns how many were embedded this call. Shared by
/// embed-on-ingest (small batch after `ingest_new_events`) and
/// `embed --backfill` (looped until it returns 0). Every pending text gets a
/// vector — including short boilerplate — so nothing is re-scanned next pass;
/// retrieval-side filtering ([`crate::embed::is_embeddable`]) decides what's
/// worth surfacing.
pub fn embed_pending(
    conn: &Connection,
    project_hash: &str,
    embedder: &dyn crate::embed::Embedder,
    created_at: &str,
    limit: usize,
) -> anyhow::Result<usize> {
    let pending = events_needing_embedding(conn, embedder.model_id(), limit)?;
    if pending.is_empty() {
        return Ok(0);
    }
    let texts: Vec<&str> = pending.iter().map(|p| p.text.as_str()).collect();
    let vecs = embedder.embed(&texts)?;
    let mut done = 0usize;
    for (p, v) in pending.iter().zip(vecs.iter()) {
        upsert_embedding(
            conn,
            &p.event_id,
            &p.task_id,
            project_hash,
            "episodic",
            embedder.model_id(),
            embedder.dim(),
            v,
            created_at,
        )?;
        done += 1;
    }
    Ok(done)
}

/// A retrieval hit: the event, its task, and the relevance score.
pub struct ScoredHit {
    pub event_id: String,
    pub task_id: String,
    pub task_title: String,
    pub event_type: String,
    pub tier: String,
    pub text: String,
    pub score: f32,
}

/// Semantic search over a project's embeddings. Scores every stored vector for
/// `model` against `query_vec` by cosine, returns the top `k` by score. The
/// caller embeds the query with the same embedder so the model ids match.
/// Pure vector ranking for now; recency / tier / contradiction weighting layer
/// on top in later phases.
pub fn semantic_search(
    conn: &Connection,
    project_hash: &str,
    query_vec: &[f32],
    model: &str,
    k: usize,
) -> anyhow::Result<Vec<ScoredHit>> {
    let mut stmt = conn.prepare(
        "SELECT e.event_id, e.task_id, e.tier, e.vec, f.text, f.type,
                COALESCE(t.title, '')
           FROM embeddings e
           JOIN search_fts f ON f.event_id = e.event_id
           LEFT JOIN tasks t ON t.task_id = e.task_id
          WHERE e.project_hash = ?1 AND e.model = ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_hash, model], |r| {
        let blob: Vec<u8> = r.get(3)?;
        Ok((
            r.get::<_, String>(0)?, // event_id
            r.get::<_, String>(1)?, // task_id
            r.get::<_, String>(2)?, // tier
            blob,
            r.get::<_, String>(4)?, // text
            r.get::<_, String>(5)?, // type
            r.get::<_, String>(6)?, // title
        ))
    })?;

    let mut hits: Vec<ScoredHit> = Vec::new();
    for row in rows {
        let (event_id, task_id, tier, blob, text, event_type, task_title) = row?;
        let score = crate::embed::cosine(query_vec, &crate::embed::from_blob(&blob));
        hits.push(ScoredHit {
            event_id,
            task_id,
            task_title,
            event_type,
            tier,
            text,
            score,
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(k);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::Embedder;
    use tempfile::TempDir;

    #[test]
    fn module_event_creates_no_task_and_no_index_row() {
        use crate::event::{Author, Source};

        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let e = Event::new(
            "mod:stars",
            EventType::Module,
            Author::Agent,
            Source::Chat,
            "Stars".into(),
        );

        upsert_task_from_event(&conn, &e, "p").unwrap();
        index_event(&conn, &e).unwrap();

        assert!(!task_exists(&conn, "mod:stars").unwrap());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events_index", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn task_exists_returns_true_for_known_id_false_otherwise() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        assert!(!task_exists(&conn, "tj-nope").unwrap());

        let e = make_open_event("tj-yes", "Hello");
        upsert_task_from_event(&conn, &e, "feedfacefeedface").unwrap();
        index_event(&conn, &e).unwrap();

        assert!(task_exists(&conn, "tj-yes").unwrap());
        assert!(!task_exists(&conn, "tj-nope").unwrap());
    }

    #[test]
    fn rename_event_updates_task_title() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let ph = "feedfacefeedface";

        let open_ev = make_open_event("tj-rn", "#: 5");
        upsert_task_from_event(&conn, &open_ev, ph).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM tasks WHERE task_id='tj-rn'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "#: 5");

        let mut rename = crate::event::Event::new(
            "tj-rn",
            crate::event::EventType::Rename,
            crate::event::Author::Agent,
            crate::event::Source::Cli,
            "Support BID 29683996 — voucher refund 50% vs promised 100%".into(),
        );
        rename.timestamp = "2099-01-01T00:00:00.000Z".into();
        upsert_task_from_event(&conn, &rename, ph).unwrap();

        let title: String = conn
            .query_row("SELECT title FROM tasks WHERE task_id='tj-rn'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            title,
            "Support BID 29683996 — voucher refund 50% vs promised 100%"
        );
    }

    #[test]
    fn close_event_restores_outcome_from_meta() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let ph = "feedfacefeedface";

        let open_ev = make_open_event("tj-cl", "T");
        upsert_task_from_event(&conn, &open_ev, ph).unwrap();

        let mut close = crate::event::Event::new(
            "tj-cl",
            crate::event::EventType::Close,
            crate::event::Author::Agent,
            crate::event::Source::Cli,
            "done".into(),
        );
        close.meta = serde_json::json!({"outcome": "Shipped the fix.", "outcome_tag": "done"});
        upsert_task_from_event(&conn, &close, ph).unwrap();

        let (status, outcome, tag): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, outcome, outcome_tag FROM tasks WHERE task_id='tj-cl'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "closed");
        assert_eq!(outcome.as_deref(), Some("Shipped the fix."));
        assert_eq!(tag.as_deref(), Some("done"));
    }

    #[test]
    fn fresh_db_runs_all_migrations() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("state.sqlite");
        let conn = open(&p).unwrap();

        let applied: Vec<i64> = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            applied,
            (1..=MIGRATIONS.len() as i64).collect::<Vec<_>>(),
            "every declared migration must be recorded"
        );
    }

    #[test]
    fn apply_migrations_is_idempotent_across_reopens() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("state.sqlite");
        let _ = open(&p).unwrap();
        let _ = open(&p).unwrap();

        let count: i64 = open(&p)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count,
            MIGRATIONS.len() as i64,
            "schema_migrations must contain exactly one row per declared migration after repeated opens"
        );
    }

    #[test]
    fn a_migration_failing_halfway_leaves_no_partial_schema() {
        let d = TempDir::new().unwrap();
        let conn = Connection::open(d.path().join("state.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
             INSERT INTO schema_migrations VALUES (1, 'x'), (2, 'x');",
        )
        .unwrap();
        conn.execute_batch(MIGRATION_001).unwrap();
        conn.execute_batch(MIGRATION_002).unwrap();
        // v003 adds goal, then outcome: make its second ALTER fail.
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN outcome TEXT;")
            .unwrap();

        assert!(apply_migrations(&conn).is_err());

        let goal_cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'goal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(goal_cols, 0, "v003's first ALTER must roll back");
        let v3: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 3",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v3, 0);
    }

    #[test]
    fn concurrent_opens_of_a_fresh_db_both_succeed() {
        let d = TempDir::new().unwrap();

        for round in 0..20 {
            let path = d.path().join(format!("state-{round}.sqlite"));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let (path, barrier) = (path.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        open(&path).map(|_| ())
                    })
                })
                .collect();

            for h in handles {
                let res = h.join().unwrap();
                assert!(res.is_ok(), "round {round}: {:#}", res.unwrap_err());
            }
        }
    }

    fn make_text_event(text: &str) -> crate::event::Event {
        crate::event::Event::new(
            "tj-x",
            crate::event::EventType::Finding,
            crate::event::Author::User,
            crate::event::Source::Cli,
            text.into(),
        )
    }

    #[test]
    fn index_event_merges_structured_meta_artifacts() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        // Close-time harvest writes deterministic refs into meta.artifacts;
        // the event text itself has no scrapeable tokens.
        let mut ev = make_text_event("closed: shipped the Loom spine");
        ev.meta = serde_json::json!({
            "artifacts": {
                "pr_urls": ["https://github.com/o/r/pull/51"],
                "commit_hashes": ["75f65e2"],
                "branch_names": ["feat/clean-pack"],
            }
        });
        index_event(&conn, &ev).unwrap();

        let arts = task_artifacts(&conn, "tj-x").unwrap();
        assert!(
            arts.pr_urls.iter().any(|p| p.contains("/pull/51")),
            "pr merged"
        );
        assert!(
            arts.commit_hashes.iter().any(|c| c == "75f65e2"),
            "commit merged"
        );
        assert!(
            arts.branch_names.iter().any(|b| b == "feat/clean-pack"),
            "branch merged"
        );
    }

    #[test]
    fn task_artifacts_leave_out_corrected_events() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let wrong = make_text_event("fixed in commit dead00beef, see src/gone.rs");
        let kept = make_text_event("also touched src/kept.rs");
        let mut corr = make_text_event("the hash and path above were wrong");
        corr.event_type = crate::event::EventType::Correction;
        corr.corrects = Some(wrong.event_id.clone());
        for e in [&wrong, &kept, &corr] {
            index_event(&conn, e).unwrap();
        }

        let arts = task_artifacts(&conn, "tj-x").unwrap();
        assert!(arts.commit_hashes.is_empty(), "{:?}", arts.commit_hashes);
        assert_eq!(arts.files, vec!["src/kept.rs".to_string()]);
    }

    #[test]
    fn embed_pending_embeds_all_then_is_idempotent() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let ph = "feedfacefeedface";

        for text in [
            "implement payment refund deduplication",
            "add validation for negative order amounts",
        ] {
            index_event(&conn, &make_text_event(text)).unwrap();
        }

        let emb = crate::embed::HashEmbedder::new(64);
        let at = "2026-06-12T00:00:00Z";

        let n = embed_pending(&conn, ph, &emb, at, 100).unwrap();
        assert_eq!(n, 2, "both events embedded on first pass");
        assert_eq!(count_embeddings(&conn, ph).unwrap(), 2);

        // Idempotent: nothing left for this model on a second pass.
        assert_eq!(embed_pending(&conn, ph, &emb, at, 100).unwrap(), 0);

        // Model-scoped: a different model id sees them as un-embedded
        // (so a model change triggers a re-embed).
        assert_eq!(
            events_needing_embedding(&conn, "other-model", 100)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn semantic_search_ranks_relevant_event_first() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let ph = "feedfacefeedface";

        for text in [
            "fix duplicate payment refund write on partial refund",
            "update the frontend button hover color",
            "add a database index for faster user lookup",
        ] {
            index_event(&conn, &make_text_event(text)).unwrap();
        }
        let emb = crate::embed::HashEmbedder::new(256);
        embed_pending(&conn, ph, &emb, "t", 100).unwrap();

        let q = emb.embed_one("payment refund duplicated").unwrap();
        let hits = semantic_search(&conn, ph, &q, emb.model_id(), 3).unwrap();

        assert_eq!(hits.len(), 3);
        assert!(
            hits[0].text.contains("refund"),
            "the refund event must rank first, got: {}",
            hits[0].text
        );
        assert!(
            hits[0].score >= hits[1].score,
            "hits must be sorted by score desc"
        );
    }

    #[test]
    fn open_creates_all_tables() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("state.sqlite");
        let conn = open(&p).unwrap();

        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' OR type='virtual table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        for required in [
            "decisions",
            "events_index",
            "evidence",
            "task_pack_cache",
            "tasks",
            "search_fts",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "missing table {required}, have {names:?}"
            );
        }
    }

    #[test]
    fn open_is_idempotent() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("state.sqlite");
        let _ = open(&p).unwrap();
        let _ = open(&p).unwrap();
    }

    #[test]
    fn index_event_projects_evidence() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = crate::event::Event::new(
            "tj-e",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "T"});
        upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        index_event(&conn, &open_e).unwrap();

        let mut ev = crate::event::Event::new(
            "tj-e",
            crate::event::EventType::Evidence,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Hook startup measured at 12ms".into(),
        );
        ev.evidence_strength = Some(crate::event::EvidenceStrength::Strong);
        upsert_task_from_event(&conn, &ev, "feedface").unwrap();
        index_event(&conn, &ev).unwrap();

        let (text, strength): (String, String) = conn
            .query_row(
                "SELECT text, strength FROM evidence WHERE task_id=?1",
                rusqlite::params!["tj-e"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(text.contains("12ms"));
        assert_eq!(strength, "strong");
    }

    #[test]
    fn supersede_event_marks_decision_superseded() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = crate::event::Event::new(
            "tj-s",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "T"});
        upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        index_event(&conn, &open_e).unwrap();

        let dec = crate::event::Event::new(
            "tj-s",
            crate::event::EventType::Decision,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Use TS".into(),
        );
        upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        index_event(&conn, &dec).unwrap();

        let mut sup = crate::event::Event::new(
            "tj-s",
            crate::event::EventType::Supersede,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Replaced by Rust decision".into(),
        );
        sup.supersedes = Some(dec.event_id.clone());
        upsert_task_from_event(&conn, &sup, "feedface").unwrap();
        index_event(&conn, &sup).unwrap();

        let (status, by): (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by FROM decisions WHERE decision_id=?1",
                rusqlite::params![dec.event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "superseded");
        assert_eq!(by.as_deref(), Some(sup.event_id.as_str()));
    }

    #[test]
    fn index_event_projects_decision_to_decisions_table() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        let mut open_e = crate::event::Event::new(
            "tj-d",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "T"});
        upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        index_event(&conn, &open_e).unwrap();

        let dec = crate::event::Event::new(
            "tj-d",
            crate::event::EventType::Decision,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Adopt Rust".into(),
        );
        upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        index_event(&conn, &dec).unwrap();

        let (id, text, status): (String, String, String) = conn
            .query_row(
                "SELECT decision_id, text, status FROM decisions WHERE task_id=?1",
                rusqlite::params!["tj-d"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(id, dec.event_id);
        assert_eq!(text, "Adopt Rust");
        assert_eq!(status, "active");
    }

    #[test]
    fn index_event_projects_decision_alternatives_into_column() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        let mut dec = crate::event::Event::new(
            "tj-alt",
            crate::event::EventType::Decision,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Use SQLite".into(),
        );
        dec.meta = serde_json::json!({
            "alternatives": [
                {"option": "SQLite", "chosen": true, "rationale": "embedded, zero-ops"},
                {"option": "Postgres", "chosen": false, "rationale": "too heavy for local tool"}
            ]
        });
        upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        index_event(&conn, &dec).unwrap();

        let alts: Option<String> = conn
            .query_row(
                "SELECT alternatives FROM decisions WHERE decision_id=?1",
                rusqlite::params![dec.event_id],
                |r| r.get(0),
            )
            .unwrap();
        let alts = alts.expect("alternatives column should be populated");
        let parsed: serde_json::Value = serde_json::from_str(&alts).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[0]["option"], "SQLite");
        assert_eq!(parsed[0]["chosen"], true);
    }

    #[test]
    fn index_event_decision_without_alternatives_leaves_column_null() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        let dec = crate::event::Event::new(
            "tj-noalt",
            crate::event::EventType::Decision,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Plain decision".into(),
        );
        upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        index_event(&conn, &dec).unwrap();

        let alts: Option<String> = conn
            .query_row(
                "SELECT alternatives FROM decisions WHERE decision_id=?1",
                rusqlite::params![dec.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(alts.is_none());
    }

    #[test]
    fn index_event_is_idempotent_no_search_fts_duplicates() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = crate::event::Event::new(
            "tj-id",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Idempotent"});
        upsert_task_from_event(&conn, &open_e, "feedface").unwrap();

        // Index three times — simulates rebuild_state replays.
        index_event(&conn, &open_e).unwrap();
        index_event(&conn, &open_e).unwrap();
        index_event(&conn, &open_e).unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM search_fts WHERE event_id=?1",
                rusqlite::params![open_e.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "search_fts must hold exactly one row per event_id");
    }

    #[test]
    fn list_all_projects_returns_hashes_from_state_dir() {
        use std::fs::File;
        let d = TempDir::new().unwrap();
        let state_dir = d.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        File::create(state_dir.join("aaaa1111aaaa1111.sqlite")).unwrap();
        File::create(state_dir.join("bbbb2222bbbb2222.sqlite")).unwrap();
        File::create(state_dir.join("not-a-project.txt")).unwrap();

        let mut hashes = list_all_projects(&state_dir).unwrap();
        hashes.sort();
        assert_eq!(hashes, vec!["aaaa1111aaaa1111", "bbbb2222bbbb2222"]);
    }

    fn write_event_line(f: &mut std::fs::File, e: &crate::event::Event) {
        use std::io::Write;
        writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
    }

    fn make_open_event(task_id: &str, title: &str) -> crate::event::Event {
        let mut e = crate::event::Event::new(
            task_id,
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        e.meta = serde_json::json!({"title": title});
        e
    }

    #[test]
    fn ingest_new_events_picks_up_only_new_lines() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let db = d.path().join("s.sqlite");
        let project = "deadbeefdeadbeef";

        let e1 = make_open_event("tj-i1", "first");
        let e2 = make_open_event("tj-i2", "second");
        let e3 = make_open_event("tj-i3", "third");

        let mut f = std::fs::File::create(&jsonl).unwrap();
        write_event_line(&mut f, &e1);
        write_event_line(&mut f, &e2);
        write_event_line(&mut f, &e3);
        drop(f);

        // First pass — no marker yet, falls back to a full rebuild.
        let conn = open(&db).unwrap();
        let n_first = ingest_new_events(&conn, &jsonl, project).unwrap();
        assert_eq!(n_first, 3);

        // Append two more events.
        let e4 = make_open_event("tj-i4", "fourth");
        let e5 = make_open_event("tj-i5", "fifth");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&jsonl)
            .unwrap();
        write_event_line(&mut f, &e4);
        write_event_line(&mut f, &e5);
        drop(f);

        // Second pass — marker = e3, only e4 + e5 must be processed.
        let n_second = ingest_new_events(&conn, &jsonl, project).unwrap();
        assert_eq!(n_second, 2, "incremental ingest must read only the tail");

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM events_index", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 5);

        let marker: String = conn
            .query_row(
                "SELECT last_indexed_event_id FROM index_state WHERE project_hash=?1",
                rusqlite::params![project],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marker, e5.event_id);
    }

    #[test]
    fn ingest_new_events_falls_back_to_full_rebuild_when_marker_vanishes() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let db = d.path().join("s.sqlite");
        let project = "feedfacefeedface";

        let e1 = make_open_event("tj-r1", "first");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        write_event_line(&mut f, &e1);
        drop(f);

        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, project).unwrap();

        // Replace the file entirely so the marker (e1.event_id) no longer
        // appears anywhere — simulates corruption / hand-edit.
        let e2 = make_open_event("tj-r2", "after-corruption");
        let e3 = make_open_event("tj-r3", "after-corruption-2");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        write_event_line(&mut f, &e2);
        write_event_line(&mut f, &e3);
        drop(f);

        let n = ingest_new_events(&conn, &jsonl, project).unwrap();
        assert_eq!(n, 2, "missing marker must trigger full rebuild");
    }

    #[test]
    fn rebuild_state_and_ingest_new_events_produce_same_state() {
        let d = TempDir::new().unwrap();
        let jsonl_a = d.path().join("a.jsonl");
        let jsonl_b = d.path().join("b.jsonl");
        let db_a = d.path().join("a.sqlite");
        let db_b = d.path().join("b.sqlite");

        let events: Vec<_> = (0..5)
            .map(|i| make_open_event(&format!("tj-eq{i}"), &format!("title {i}")))
            .collect();
        for path in [&jsonl_a, &jsonl_b] {
            let mut f = std::fs::File::create(path).unwrap();
            for e in &events {
                write_event_line(&mut f, e);
            }
        }

        let conn_a = open(&db_a).unwrap();
        let n_a = rebuild_state(&conn_a, &jsonl_a, "abcd1234abcd1234").unwrap();

        let conn_b = open(&db_b).unwrap();
        let n_b = ingest_new_events(&conn_b, &jsonl_b, "abcd1234abcd1234").unwrap();

        assert_eq!(n_a, n_b);
        assert_eq!(n_a, 5);

        for table in ["tasks", "events_index"] {
            let q = format!("SELECT COUNT(*) FROM {table}");
            let cnt_a: i64 = conn_a.query_row(&q, [], |r| r.get(0)).unwrap();
            let cnt_b: i64 = conn_b.query_row(&q, [], |r| r.get(0)).unwrap();
            assert_eq!(cnt_a, cnt_b, "row count mismatch in {table}");
        }
    }

    /// A log of `n` events: an open every 30 events, findings in between.
    fn write_synthetic_log(path: &Path, n: usize) -> Vec<crate::event::Event> {
        let mut events = Vec::with_capacity(n);
        let mut task = String::new();
        for i in 0..n {
            if i % 30 == 0 {
                task = format!("tj-syn{i}");
                events.push(make_open_event(&task, &task));
            } else {
                let mut e = make_text_event(&format!(
                    "step {i}: replay chunk marker in crates/tj-core/src/db.rs, commit {i:07x}ab"
                ));
                e.task_id = task.clone();
                events.push(e);
            }
        }

        let mut f = std::fs::File::create(path).unwrap();
        for e in &events {
            write_event_line(&mut f, e);
        }

        events
    }

    fn count_rows(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    /// Seconds a replay of an already indexed `n`-event log takes once
    /// `index_state` is cleared, as the v009–v011 upgrade migrations do.
    fn upgrade_replay_seconds(n: usize) -> f64 {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let ph = "feedfacefeedface";
        write_synthetic_log(&jsonl, n);
        let conn = open(d.path().join("s.sqlite")).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();
        conn.execute_batch("DELETE FROM index_state").unwrap();

        let started = std::time::Instant::now();
        assert_eq!(ingest_new_events(&conn, &jsonl, ph).unwrap(), n);
        let secs = started.elapsed().as_secs_f64();

        assert_eq!(count_rows(&conn, "search_fts"), n as i64);
        secs
    }

    #[test]
    fn a_full_replay_grows_linearly_with_the_log() {
        // Best of two runs each, to keep scheduler noise out of the ratio.
        let small = upgrade_replay_seconds(1500).min(upgrade_replay_seconds(1500));
        let large = upgrade_replay_seconds(3000).min(upgrade_replay_seconds(3000));

        // Linear work doubles; the old per-event FTS scan quadrupled it.
        assert!(
            large < small * 3.0,
            "replay of 2x events took {:.2}x as long ({small:.3}s -> {large:.3}s)",
            large / small
        );
    }

    #[test]
    fn a_replay_killed_partway_resumes_after_its_last_committed_chunk() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let ph = "feedfacefeedface";
        let events = write_synthetic_log(&jsonl, 1200);
        let conn = open(d.path().join("s.sqlite")).unwrap();

        // The process dies while indexing an event of the second chunk.
        conn.execute_batch(&format!(
            "CREATE TRIGGER kill BEFORE INSERT ON events_index WHEN NEW.event_id = '{}'
             BEGIN SELECT RAISE(ABORT, 'killed'); END;",
            events[700].event_id
        ))
        .unwrap();
        assert!(ingest_new_events(&conn, &jsonl, ph).is_err());
        conn.execute_batch("DROP TRIGGER kill").unwrap();

        let committed = count_rows(&conn, "events_index");
        assert!(
            committed > 0 && committed <= 700,
            "the chunks before the kill stay committed, got {committed}"
        );

        let resumed = ingest_new_events(&conn, &jsonl, ph).unwrap();
        assert_eq!(
            resumed as i64,
            1200 - committed,
            "the resumed replay must not redo committed chunks"
        );
        assert_eq!(count_rows(&conn, "events_index"), 1200);
        assert_eq!(count_rows(&conn, "search_fts"), 1200);
    }

    #[test]
    fn concurrent_replays_share_the_work_without_locking_errors_or_duplicates() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let db = d.path().join("s.sqlite");
        write_synthetic_log(&jsonl, 2000);
        drop(open(&db).unwrap());

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (jsonl, db, barrier) = (jsonl.clone(), db.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let conn = open(&db).unwrap();
                    barrier.wait();
                    ingest_new_events(&conn, &jsonl, "feedfacefeedface").map(|_| ())
                })
            })
            .collect();
        for h in handles {
            let res = h.join().unwrap();
            assert!(res.is_ok(), "{:#}", res.unwrap_err());
        }

        let conn = open(&db).unwrap();
        assert_eq!(count_rows(&conn, "events_index"), 2000);
        assert_eq!(count_rows(&conn, "search_fts"), 2000);
    }

    #[test]
    fn rebuild_state_skips_malformed_jsonl_lines() {
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let events_path = d.path().join("events.jsonl");
        let db_path = d.path().join("s.sqlite");

        let mut f = std::fs::File::create(&events_path).unwrap();

        let mut e1 = crate::event::Event::new(
            "tj-skip",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        e1.meta = serde_json::json!({"title": "Skip test"});
        writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();

        // Garbage that is not even JSON.
        writeln!(f, "this is not a json event line").unwrap();

        // Valid JSON but not a valid Event (missing required fields).
        writeln!(f, "{{\"foo\": 1}}").unwrap();

        let e3 = crate::event::Event::new(
            "tj-skip",
            crate::event::EventType::Decision,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Adopt Rust".into(),
        );
        writeln!(f, "{}", serde_json::to_string(&e3).unwrap()).unwrap();
        drop(f);

        let conn = open(&db_path).unwrap();
        let n = rebuild_state(&conn, &events_path, "deadbeefdeadbeef")
            .expect("rebuild_state must succeed despite malformed lines");
        assert_eq!(
            n, 2,
            "expected 2 valid events indexed (2 malformed skipped)"
        );

        let indexed: i64 = conn
            .query_row("SELECT COUNT(*) FROM events_index", [], |r| r.get(0))
            .unwrap();
        assert_eq!(indexed, 2);
    }

    #[test]
    fn rebuild_state_reads_jsonl_and_populates_db() {
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let events_path = d.path().join("events.jsonl");
        let db_path = d.path().join("s.sqlite");

        let mut f = std::fs::File::create(&events_path).unwrap();
        let mut e1 = crate::event::Event::new(
            "tj-9",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        e1.meta = serde_json::json!({"title": "Nine"});
        let e2 = crate::event::Event::new(
            "tj-9",
            crate::event::EventType::Decision,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Adopt Rust".into(),
        );
        writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e2).unwrap()).unwrap();
        drop(f);

        let conn = open(&db_path).unwrap();
        let n = rebuild_state(&conn, &events_path, "deadbeefdeadbeef").unwrap();
        assert_eq!(n, 2);

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events_index", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn rebuild_state_treats_a_missing_jsonl_as_empty() {
        // A brand-new project has no events log on disk yet — rebuild/ingest must
        // report zero events, not error. This is the crash that made task_create
        // fail the first time the journal was touched in a fresh worktree.
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let missing = d.path().join("does-not-exist.jsonl");
        assert_eq!(
            rebuild_state(&conn, &missing, "deadbeefdeadbeef").unwrap(),
            0
        );
        assert_eq!(
            ingest_new_events(&conn, &missing, "deadbeefdeadbeef").unwrap(),
            0
        );
    }

    #[test]
    fn index_event_writes_index_and_fts() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = crate::event::Event::new(
            "tj-1",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "Title".into(),
        );
        open_e.meta = serde_json::json!({"title": "Title"});
        upsert_task_from_event(&conn, &open_e, "deadbeefdeadbeef").unwrap();
        index_event(&conn, &open_e).unwrap();

        let mut decision = crate::event::Event::new(
            "tj-1",
            crate::event::EventType::Decision,
            crate::event::Author::Agent,
            crate::event::Source::Chat,
            "Adopt Rust".into(),
        );
        decision.confidence = Some(0.92);
        upsert_task_from_event(&conn, &decision, "deadbeefdeadbeef").unwrap();
        index_event(&conn, &decision).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
                rusqlite::params!["tj-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        let mut stmt = conn
            .prepare("SELECT event_id FROM search_fts WHERE search_fts MATCH ?1")
            .unwrap();
        let hits: Vec<String> = stmt
            .query_map(rusqlite::params!["Rust"], |r| {
                let s: String = r.get(0)?;
                Ok(s)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0], decision.event_id);
    }

    #[test]
    fn upsert_task_from_open_event_inserts_row() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        let mut e = crate::event::Event::new(
            "tj-7f3a",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "Add OAuth".into(),
        );
        e.meta = serde_json::json!({ "title": "Add OAuth login" });

        upsert_task_from_event(&conn, &e, "abcd1234abcd1234").unwrap();

        let (id, title, status): (String, String, String) = conn
            .query_row(
                "SELECT task_id, title, status FROM tasks WHERE task_id = ?1",
                ["tj-7f3a"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        assert_eq!(id, "tj-7f3a");
        assert_eq!(title, "Add OAuth login");
        assert_eq!(status, "open");
    }

    #[test]
    fn migration_adds_parent_id_column_nullable() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        // Seed a task via an open event (no parent).
        let e = make_open_event("tj-a", "Top");
        upsert_task_from_event(&conn, &e, "ph").unwrap();

        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM tasks WHERE task_id = ?1",
                rusqlite::params!["tj-a"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent, None);
    }

    #[test]
    fn task_id_by_external_resolves_exact_token() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        upsert_task_from_event(&conn, &make_open_event("tj-a", "A"), "ph").unwrap();
        upsert_task_from_event(&conn, &make_open_event("tj-b", "B"), "ph").unwrap();
        // tj-b carries two external refs incl. the loom one.
        add_task_external(&conn, "tj-b", "github:#7").unwrap();
        add_task_external(&conn, "tj-b", "loom:t-xyz").unwrap();

        assert_eq!(
            task_id_by_external(&conn, "loom:t-xyz").unwrap().as_deref(),
            Some("tj-b")
        );
        // exact token match: a different id does not match
        assert_eq!(task_id_by_external(&conn, "loom:t-other").unwrap(), None);
        // no false-positive on a substring of a token
        assert_eq!(task_id_by_external(&conn, "loom:t-xy").unwrap(), None);
    }

    /// An event of `task_id` stamped with `session` at a fixed `timestamp`.
    fn session_event(task_id: &str, session: &str, timestamp: &str) -> crate::event::Event {
        let mut e = make_text_event("work");
        e.task_id = task_id.into();
        e.timestamp = timestamp.into();
        e.meta = serde_json::json!({"session_id": session});
        e
    }

    fn indexed_session_id(conn: &Connection, event_id: &str) -> Option<String> {
        conn.query_row(
            "SELECT session_id FROM events_index WHERE event_id = ?1",
            rusqlite::params![event_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn index_event_stores_the_session_id_from_meta() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        let stamped = session_event("tj-s", "sess-1", "2026-01-01T00:00:00.000Z");
        let plain = make_text_event("no session");
        index_event(&conn, &stamped).unwrap();
        index_event(&conn, &plain).unwrap();

        assert_eq!(
            indexed_session_id(&conn, &stamped.event_id).as_deref(),
            Some("sess-1")
        );
        assert_eq!(indexed_session_id(&conn, &plain.event_id), None);
    }

    #[test]
    fn active_task_for_session_picks_the_open_task_with_its_latest_event() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let ph = "feedfacefeedface";

        for id in ["tj-a", "tj-b", "tj-c"] {
            upsert_task_from_event(&conn, &make_open_event(id, id), ph).unwrap();
        }
        upsert_task_from_event(&conn, &make_open_event("tj-other", "o"), "otherproject0000")
            .unwrap();
        let events = [
            session_event("tj-a", "s1", "2026-01-01T00:00:01.000Z"),
            session_event("tj-b", "s1", "2026-01-01T00:00:02.000Z"),
            session_event("tj-a", "s2", "2026-01-01T00:00:03.000Z"),
            session_event("tj-c", "s1", "2026-01-01T00:00:04.000Z"),
            session_event("tj-other", "s1", "2026-01-01T00:00:05.000Z"),
        ];
        for e in &events {
            upsert_task_from_event(&conn, e, ph).unwrap();
            index_event(&conn, e).unwrap();
        }
        let mut close = make_text_event("done");
        close.task_id = "tj-c".into();
        close.event_type = crate::event::EventType::Close;
        upsert_task_from_event(&conn, &close, ph).unwrap();

        // tj-c has s1's latest event but is closed; tj-other is another project.
        assert_eq!(
            active_task_for_session(&conn, ph, "s1").unwrap().as_deref(),
            Some("tj-b")
        );
        assert_eq!(
            active_task_for_session(&conn, ph, "s2").unwrap().as_deref(),
            Some("tj-a")
        );
        assert_eq!(active_task_for_session(&conn, ph, "s3").unwrap(), None);
    }

    #[test]
    fn hook_and_classifier_writes_never_bind_a_session_to_a_task() {
        use crate::event::{Author, EventType};
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let ph = "feedfacefeedface";
        upsert_task_from_event(&conn, &make_open_event("tj-t1", "T1"), ph).unwrap();

        // Session S1 works on T1 through the agent.
        let mut work = session_event("tj-t1", "s1", "2026-01-01T00:00:01.000Z");
        work.author = Author::Agent;
        // A fresh session S2: its PostModelSwitch note and a Stop catch-up
        // classifier event fall back to the newest open task, T1.
        let mut switch = session_event("tj-t1", "s2", "2026-01-01T00:00:02.000Z");
        switch.author = Author::Classifier;
        switch.event_type = EventType::Constraint;
        switch.meta = serde_json::json!({"session_id": "s2", "kind": "model_switch"});
        let mut caught_up = session_event("tj-t1", "s2", "2026-01-01T00:00:03.000Z");
        caught_up.author = Author::Classifier;
        for e in [&work, &switch, &caught_up] {
            upsert_task_from_event(&conn, e, ph).unwrap();
            index_event(&conn, e).unwrap();
        }

        assert_eq!(active_task_for_session(&conn, ph, "s2").unwrap(), None);
        assert_eq!(
            active_task_for_session(&conn, ph, "s1").unwrap().as_deref(),
            Some("tj-t1")
        );
    }

    #[test]
    fn upgrading_an_existing_db_backfills_session_ids_on_the_next_ingest() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let db = d.path().join("s.sqlite");
        let ph = "feedfacefeedface";

        let open_ev = make_open_event("tj-up", "Upgrade");
        let stamped = session_event("tj-up", "sess-old", "2026-01-01T00:00:01.000Z");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        write_event_line(&mut f, &open_ev);
        write_event_line(&mut f, &stamped);
        drop(f);

        // A pre-session_id database that has already indexed the whole log.
        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();
        // Written straight to SQLite by older versions: the replay must keep them.
        set_task_goal(&conn, "tj-up", "legacy goal").unwrap();
        add_task_external(&conn, "tj-up", "loom:t-legacy").unwrap();
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_events_session_time;
             ALTER TABLE events_index DROP COLUMN session_id;
             DELETE FROM schema_migrations WHERE version = 9;",
        )
        .unwrap();
        drop(conn);

        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();

        assert_eq!(
            indexed_session_id(&conn, &stamped.event_id).as_deref(),
            Some("sess-old")
        );
        assert_eq!(
            active_task_for_session(&conn, ph, "sess-old")
                .unwrap()
                .as_deref(),
            Some("tj-up")
        );
        let meta = task_metadata(&conn, "tj-up").unwrap().unwrap();
        assert_eq!(meta.goal.as_deref(), Some("legacy goal"));
        assert_eq!(meta.external.as_deref(), Some("loom:t-legacy"));
    }

    #[test]
    fn upgrading_an_existing_db_links_corrections_on_the_next_ingest() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let db = d.path().join("s.sqlite");
        let ph = "feedfacefeedface";

        let open_ev = make_open_event("tj-x", "Upgrade");
        let wrong = make_text_event("Migration done (wrong)");
        let mut corr = make_text_event("Migration NOT done");
        corr.event_type = crate::event::EventType::Correction;
        corr.corrects = Some(wrong.event_id.clone());
        let mut f = std::fs::File::create(&jsonl).unwrap();
        for e in [&open_ev, &wrong, &corr] {
            write_event_line(&mut f, e);
        }
        drop(f);

        // A pre-corrected_by database that has already indexed the whole log.
        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();
        conn.execute_batch(
            "ALTER TABLE events_index DROP COLUMN corrected_by;
             DELETE FROM schema_migrations WHERE version = 10;",
        )
        .unwrap();
        drop(conn);

        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();

        let corrected_by: Option<String> = conn
            .query_row(
                "SELECT corrected_by FROM events_index WHERE event_id = ?1",
                rusqlite::params![wrong.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(corrected_by.as_deref(), Some(corr.event_id.as_str()));
    }

    #[test]
    fn upgrading_wipes_packs_cached_as_whole_text() {
        let d = TempDir::new().unwrap();
        let db = d.path().join("s.sqlite");
        let conn = open(&db).unwrap();
        conn.execute_batch(
            "INSERT INTO task_pack_cache(task_id, mode, text, generated_at, source_event_count)
             VALUES ('tj-x', 'compact', '# Old whole pack', '', 1);
             DELETE FROM schema_migrations WHERE version = 12;",
        )
        .unwrap();
        drop(conn);

        let conn = open(&db).unwrap();
        let cached: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_pack_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cached, 0);
    }

    #[test]
    fn upgrading_an_existing_db_flags_old_bookkeeping_on_the_next_ingest() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let db = d.path().join("s.sqlite");
        let ph = "feedfacefeedface";

        // Written before the writers set meta.kind: known by text alone.
        let open_ev = make_open_event("tj-x", "Upgrade");
        let mut marker = make_text_event("Conversation compacted at 2026-01-01T00:00:00Z; …");
        marker.event_type = crate::event::EventType::Decision;
        let mut switch = make_text_event("Model switched (user): opus → sonnet");
        switch.event_type = crate::event::EventType::Constraint;
        let real = make_text_event("Use SQLite");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        for e in [&open_ev, &marker, &switch, &real] {
            write_event_line(&mut f, e);
        }
        drop(f);

        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();
        conn.execute_batch(
            "ALTER TABLE events_index DROP COLUMN bookkeeping;
             DELETE FROM schema_migrations WHERE version = 11;",
        )
        .unwrap();
        drop(conn);

        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();

        let flagged: Vec<String> = conn
            .prepare("SELECT event_id FROM events_index WHERE bookkeeping = 1 ORDER BY event_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut expected = vec![marker.event_id.clone(), switch.event_id.clone()];
        expected.sort();
        assert_eq!(flagged, expected);
    }

    #[test]
    fn a_rebuild_by_an_older_binary_is_repaired_on_the_next_ingest() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let ph = "feedfacefeedface";

        let open_ev = make_open_event("tj-old", "Old binary");
        let mut wrong = session_event("tj-old", "sess-1", "2026-01-01T00:00:01.000Z");
        wrong.event_type = crate::event::EventType::Decision;
        let mut corr = make_text_event("Not this");
        corr.task_id = "tj-old".into();
        corr.event_type = crate::event::EventType::Correction;
        corr.corrects = Some(wrong.event_id.clone());
        let amend = amend_event("tj-old", serde_json::json!({"goal": "g"}));
        let mut f = std::fs::File::create(&jsonl).unwrap();
        for e in [&open_ev, &wrong, &corr, &amend] {
            write_event_line(&mut f, e);
        }
        drop(f);

        let conn = open(d.path().join("s.sqlite")).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();

        // A 0.29 server cannot parse the trailing amend, so it misses the
        // marker and rebuilds: its rows lack the 0.30 columns, and it records
        // the last event it could read.
        conn.execute_batch(
            "UPDATE events_index SET session_id = NULL, corrected_by = NULL, bookkeeping = 0;",
        )
        .unwrap();
        conn.execute(
            "UPDATE index_state SET last_indexed_event_id = ?1, updated_at = '2026-01-02T00:00:00.000Z'",
            rusqlite::params![corr.event_id],
        )
        .unwrap();

        ingest_new_events(&conn, &jsonl, ph).unwrap();

        assert_eq!(
            active_task_for_session(&conn, ph, "sess-1")
                .unwrap()
                .as_deref(),
            Some("tj-old")
        );
        let corrected_by: Option<String> = conn
            .query_row(
                "SELECT corrected_by FROM events_index WHERE event_id = ?1",
                rusqlite::params![wrong.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(corrected_by.as_deref(), Some(corr.event_id.as_str()));
        assert_eq!(
            ingest_new_events(&conn, &jsonl, ph).unwrap(),
            0,
            "once repaired, the projection is in sync again"
        );
    }

    #[test]
    fn upgrading_from_0_29_replays_the_log_exactly_once() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let db = d.path().join("s.sqlite");
        let ph = "feedfacefeedface";
        let events = write_synthetic_log(&jsonl, 40);

        // A 0.29 database (schema v008) that has indexed the whole log.
        let conn = open(&db).unwrap();
        ingest_new_events(&conn, &jsonl, ph).unwrap();
        conn.execute_batch(
            "DROP INDEX idx_events_session_time;
             ALTER TABLE events_index DROP COLUMN session_id;
             ALTER TABLE events_index DROP COLUMN corrected_by;
             ALTER TABLE events_index DROP COLUMN bookkeeping;
             ALTER TABLE events_index DROP COLUMN author;
             DROP TABLE projection_state;
             DELETE FROM schema_migrations WHERE version >= 9;",
        )
        .unwrap();
        drop(conn);

        let conn = open(&db).unwrap();
        assert_eq!(ingest_new_events(&conn, &jsonl, ph).unwrap(), 40);
        assert_eq!(ingest_new_events(&conn, &jsonl, ph).unwrap(), 0);

        let authored: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events_index WHERE author = 'user'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(authored, events.len() as i64);
    }

    #[test]
    fn open_meta_goal_and_external_are_restored_by_a_rebuild() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let ph = "feedfacefeedface";

        let mut open_ev = make_open_event("tj-g", "Goal task");
        open_ev.meta = serde_json::json!({
            "title": "Goal task",
            "goal": "Ship PKCE",
            "external": ["loom:t-42", "github:#7"],
        });
        let mut f = std::fs::File::create(&jsonl).unwrap();
        write_event_line(&mut f, &open_ev);
        drop(f);

        let conn = open(d.path().join("fresh.sqlite")).unwrap();
        rebuild_state(&conn, &jsonl, ph).unwrap();

        let meta = task_metadata(&conn, "tj-g").unwrap().unwrap();
        assert_eq!(meta.goal.as_deref(), Some("Ship PKCE"));
        assert_eq!(meta.external.as_deref(), Some("loom:t-42,github:#7"));
        assert_eq!(
            task_id_by_external(&conn, "loom:t-42").unwrap().as_deref(),
            Some("tj-g")
        );
    }

    fn amend_event(task_id: &str, meta: serde_json::Value) -> crate::event::Event {
        let mut e = crate::event::Event::new(
            task_id,
            crate::event::EventType::Amend,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "amend".into(),
        );
        e.meta = meta;
        e
    }

    #[test]
    fn amend_events_are_replayed_idempotently_and_stay_out_of_the_index() {
        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let ph = "feedfacefeedface";

        let mut open_ev = make_open_event("tj-am", "Amended");
        open_ev.meta = serde_json::json!({"title": "Amended", "goal": "first goal"});
        let mut f = std::fs::File::create(&jsonl).unwrap();
        write_event_line(&mut f, &open_ev);
        write_event_line(
            &mut f,
            &amend_event("tj-am", serde_json::json!({"goal": "second goal"})),
        );
        for _ in 0..2 {
            write_event_line(
                &mut f,
                &amend_event("tj-am", serde_json::json!({"external_add": ["beads:x"]})),
            );
        }
        // An amend for a task that does not exist must not break ingest.
        write_event_line(
            &mut f,
            &amend_event("tj-ghost", serde_json::json!({"external_add": ["beads:y"]})),
        );
        drop(f);

        let conn = open(d.path().join("fresh.sqlite")).unwrap();
        rebuild_state(&conn, &jsonl, ph).unwrap();
        // Replaying over an existing DB (e.g. after a migration) changes nothing.
        rebuild_state(&conn, &jsonl, ph).unwrap();

        let meta = task_metadata(&conn, "tj-am").unwrap().unwrap();
        assert_eq!(meta.goal.as_deref(), Some("second goal"));
        assert_eq!(meta.external.as_deref(), Some("beads:x"));

        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events_index WHERE type = 'amend'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 0, "amend is metadata, not a pack/search event");
        let pack = crate::pack::assemble(&conn, "tj-am", crate::pack::PackMode::Full).unwrap();
        assert!(pack.text.contains("**Goal**: second goal"), "{}", pack.text);
        assert!(!pack.text.contains("[amend]"), "{}", pack.text);
    }

    #[test]
    fn open_event_meta_parent_id_is_persisted() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        // Parent first.
        upsert_task_from_event(&conn, &make_open_event("tj-parent", "Parent"), "ph").unwrap();

        // Child carries meta.parent_id.
        let mut child = make_open_event("tj-child", "Child");
        child.meta = serde_json::json!({"title": "Child", "parent_id": "tj-parent"});
        upsert_task_from_event(&conn, &child, "ph").unwrap();

        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM tasks WHERE task_id = ?1",
                rusqlite::params!["tj-child"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("tj-parent"));
    }

    #[test]
    fn children_of_and_parent_of_work() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        upsert_task_from_event(&conn, &make_open_event("p", "Parent"), "ph").unwrap();

        let mut c1 = make_open_event("c1", "Child1");
        c1.meta = serde_json::json!({"title": "Child1", "parent_id": "p"});
        upsert_task_from_event(&conn, &c1, "ph").unwrap();
        let mut c2 = make_open_event("c2", "Child2");
        c2.meta = serde_json::json!({"title": "Child2", "parent_id": "p"});
        upsert_task_from_event(&conn, &c2, "ph").unwrap();

        let kids = children_of(&conn, "p").unwrap();
        let ids: Vec<&str> = kids.iter().map(|t| t.task_id.as_str()).collect();
        assert!(ids.contains(&"c1") && ids.contains(&"c2"));
        assert_eq!(kids.len(), 2);

        assert_eq!(parent_of(&conn, "c1").unwrap().as_deref(), Some("p"));
        assert_eq!(parent_of(&conn, "p").unwrap(), None);
    }

    #[test]
    fn cycle_guard_rejects_self_and_ancestor() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        upsert_task_from_event(&conn, &make_open_event("a", "A"), "ph").unwrap();
        let mut b = make_open_event("b", "B");
        b.meta = serde_json::json!({"title": "B", "parent_id": "a"});
        upsert_task_from_event(&conn, &b, "ph").unwrap();

        // a is b's ancestor → making a a child of b is a cycle.
        assert!(would_create_cycle(&conn, "a", "b").unwrap());
        // self-parent is a cycle.
        assert!(would_create_cycle(&conn, "a", "a").unwrap());
        // unrelated parent is fine.
        upsert_task_from_event(&conn, &make_open_event("x", "X"), "ph").unwrap();
        assert!(!would_create_cycle(&conn, "x", "a").unwrap());
    }

    #[test]
    fn invalidate_cascade_clears_parent_pack() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        upsert_task_from_event(&conn, &make_open_event("p", "P"), "ph").unwrap();
        let mut c = make_open_event("c", "C");
        c.meta = serde_json::json!({"title": "C", "parent_id": "p"});
        upsert_task_from_event(&conn, &c, "ph").unwrap();

        // Seed pack cache rows for both.
        for id in ["p", "c"] {
            conn.execute(
                "INSERT INTO task_pack_cache(task_id, mode, text, generated_at, source_event_count)
                 VALUES (?1, 'compact', 'x', '2026-01-01T00:00:00Z', 1)",
                rusqlite::params![id],
            )
            .unwrap();
        }

        invalidate_pack_cascade(&conn, "c").unwrap();

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_pack_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0, "both child and parent pack caches cleared");
    }

    #[test]
    fn count_open_children_counts_only_open() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        upsert_task_from_event(&conn, &make_open_event("p", "P"), "ph").unwrap();
        let mut c1 = make_open_event("c1", "C1");
        c1.meta = serde_json::json!({"title": "C1", "parent_id": "p"});
        upsert_task_from_event(&conn, &c1, "ph").unwrap();
        // Close c1.
        let mut close = crate::event::Event::new(
            "c1",
            crate::event::EventType::Close,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "done".into(),
        );
        close.timestamp = "2026-01-02T00:00:00Z".into();
        upsert_task_from_event(&conn, &close, "ph").unwrap();
        let mut c2 = make_open_event("c2", "C2");
        c2.meta = serde_json::json!({"title": "C2", "parent_id": "p"});
        upsert_task_from_event(&conn, &c2, "ph").unwrap();

        assert_eq!(count_open_children(&conn, "p").unwrap(), 1); // only c2
    }
}
