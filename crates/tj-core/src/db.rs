use anyhow::Context;
use rusqlite::Connection;
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
        conn.execute_batch(migration.sql)
            .with_context(|| format!("apply schema migration v{:03}", migration.version))?;
        conn.execute(
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
    }
    Ok(())
}

use crate::event::{Event, EventType};

pub fn upsert_task_from_event(
    conn: &Connection,
    event: &Event,
    project_hash: &str,
) -> anyhow::Result<()> {
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
        }
        EventType::Close => {
            conn.execute(
                "UPDATE tasks SET status='closed', closed_at=?2, last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
            )?;
        }
        EventType::Reopen => {
            conn.execute(
                "UPDATE tasks SET status='open', closed_at=NULL, last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
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

pub fn rebuild_state(
    conn: &Connection,
    jsonl_path: impl AsRef<Path>,
    project_hash: &str,
) -> anyhow::Result<usize> {
    let f = std::fs::File::open(&jsonl_path)
        .with_context(|| format!("open {:?}", jsonl_path.as_ref()))?;
    let reader = std::io::BufReader::new(f);

    let tx = conn.unchecked_transaction()?;
    let mut count = 0;
    let mut last_event_id: Option<String> = None;
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {i}"))?;
        if line.trim().is_empty() {
            continue;
        }
        // Malformed JSONL lines are skipped with a warning so that one bad
        // event cannot abort an otherwise-recoverable rebuild. SQL errors
        // still propagate — those indicate schema/integrity problems.
        let event: Event = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(
                    line_number = i + 1,
                    error = %err,
                    "skipping malformed JSONL line in rebuild_state"
                );
                continue;
            }
        };
        upsert_task_from_event(&tx, &event, project_hash)?;
        index_event(&tx, &event)?;
        last_event_id = Some(event.event_id.clone());
        count += 1;
    }
    if let Some(eid) = last_event_id.as_deref() {
        record_last_indexed(&tx, project_hash, eid)?;
    }
    tx.commit()?;
    Ok(count)
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
/// stored as a comma-separated list — small, append-mostly, no
/// uniqueness constraint. Acceptable shapes (loose, not enforced):
/// `beads:claude-memory-rsw`, `github:#42`, `jira:PROJ-1234`.
pub fn add_task_external(conn: &Connection, task_id: &str, reference: &str) -> anyhow::Result<()> {
    let current: Option<String> = conn
        .query_row(
            "SELECT external FROM tasks WHERE task_id = ?1",
            rusqlite::params![task_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .with_context(|| format!("read external for {task_id}"))?;
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
/// failing the pack render.
pub fn task_artifacts(
    conn: &Connection,
    task_id: &str,
) -> anyhow::Result<crate::artifacts::Artifacts> {
    let mut stmt = conn.prepare(
        "SELECT artifacts FROM events_index
         WHERE task_id = ?1 AND artifacts IS NOT NULL
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

/// Look up the most recent `event_id` we've ingested for this project.
/// Returns `None` when the project has never been indexed (first call,
/// or migration v002 just landed on an existing 0.1.x DB).
fn last_indexed_event_id(conn: &Connection, project_hash: &str) -> anyhow::Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT last_indexed_event_id FROM index_state WHERE project_hash = ?1")?;
    let mut rows = stmt.query(rusqlite::params![project_hash])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, String>(0)?))
    } else {
        Ok(None)
    }
}

fn record_last_indexed(
    conn: &Connection,
    project_hash: &str,
    event_id: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO index_state(project_hash, last_indexed_event_id, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(project_hash) DO UPDATE SET
             last_indexed_event_id = excluded.last_indexed_event_id,
             updated_at = excluded.updated_at",
        rusqlite::params![
            project_hash,
            event_id,
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        ],
    )?;
    Ok(())
}

/// Read only the tail of the JSONL log since the last call. The cheap path
/// for hot loops (every MCP tool invocation): scan to the marker, ingest
/// the rest, update the marker.
///
/// Falls back to a full [`rebuild_state`] in two cases:
/// - No marker yet for this project (first call after migration v002 or
///   on a brand-new install).
/// - The stored marker is not present in the JSONL (corrupted / truncated
///   file). A `tracing::warn!` is emitted so the operator notices.
pub fn ingest_new_events(
    conn: &Connection,
    jsonl_path: impl AsRef<Path>,
    project_hash: &str,
) -> anyhow::Result<usize> {
    let marker = match last_indexed_event_id(conn, project_hash)? {
        Some(id) => id,
        None => return rebuild_state(conn, jsonl_path, project_hash),
    };

    let f = std::fs::File::open(&jsonl_path)
        .with_context(|| format!("open {:?}", jsonl_path.as_ref()))?;
    let reader = std::io::BufReader::new(f);

    // First pass: confirm the marker still exists in the file. If it does
    // not, the JSONL has been rewritten under us — we can't trust the
    // marker, so we fall back to a full rebuild.
    let tx = conn.unchecked_transaction()?;
    let mut found_marker = false;
    let mut count = 0;
    let mut last_event_id: Option<String> = None;
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {i}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Event = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(
                    line_number = i + 1,
                    error = %err,
                    "skipping malformed JSONL line in ingest_new_events"
                );
                continue;
            }
        };
        if !found_marker {
            if event.event_id == marker {
                found_marker = true;
            }
            continue;
        }
        upsert_task_from_event(&tx, &event, project_hash)?;
        index_event(&tx, &event)?;
        last_event_id = Some(event.event_id.clone());
        count += 1;
    }

    if !found_marker {
        // Discard the (empty) tx and rebuild from scratch.
        drop(tx);
        tracing::warn!(
            project_hash = project_hash,
            marker = marker.as_str(),
            "last_indexed_event_id not found in JSONL — falling back to full rebuild"
        );
        return rebuild_state(conn, jsonl_path, project_hash);
    }

    if let Some(eid) = last_event_id.as_deref() {
        record_last_indexed(&tx, project_hash, eid)?;
    }
    tx.commit()?;
    Ok(count)
}

pub fn index_event(conn: &Connection, event: &Event) -> anyhow::Result<()> {
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
    let artifacts = crate::artifacts::extract(&event.text);
    let artifacts_json = if artifacts.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&artifacts)?)
    };
    conn.execute(
        "INSERT OR REPLACE INTO events_index(event_id, task_id, type, timestamp, confidence, status, artifacts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            event.event_id, event.task_id, type_str,
            event.timestamp, event.confidence, status_str, artifacts_json
        ],
    )?;
    // search_fts has no PK; clear then insert to keep idempotent across rebuild_state replays.
    conn.execute(
        "DELETE FROM search_fts WHERE event_id=?1",
        rusqlite::params![event.event_id],
    )?;
    conn.execute(
        "INSERT INTO search_fts(task_id, event_id, text, type) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event.task_id, event.event_id, event.text, type_str],
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

pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
    }
    let conn =
        Connection::open(&path).with_context(|| format!("open SQLite at {:?}", path.as_ref()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
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
