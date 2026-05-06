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
            conn.execute(
                "INSERT INTO tasks(task_id, title, status, project_hash, opened_at, last_event_at)
                 VALUES (?1, ?2, 'open', ?3, ?4, ?4)
                 ON CONFLICT(task_id) DO UPDATE SET last_event_at = ?4",
                rusqlite::params![event.task_id, title, project_hash, event.timestamp],
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
    conn.execute(
        "INSERT OR REPLACE INTO events_index(event_id, task_id, type, timestamp, confidence, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            event.event_id, event.task_id, type_str,
            event.timestamp, event.confidence, status_str
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
        conn.execute(
            "INSERT OR REPLACE INTO decisions(decision_id, task_id, text, status)
             VALUES (?1, ?2, ?3, 'active')",
            rusqlite::params![event.event_id, event.task_id, event.text],
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

    // Invalidate any cached pack for this task.
    conn.execute(
        "DELETE FROM task_pack_cache WHERE task_id=?1",
        rusqlite::params![event.task_id],
    )?;

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

#[cfg(test)]
mod tests {
    use super::*;
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
}
