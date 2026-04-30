use anyhow::Context;
use rusqlite::Connection;
use std::path::Path;

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
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {i}"))?;
        if line.trim().is_empty() { continue; }
        let event: Event = serde_json::from_str(&line)
            .with_context(|| format!("parse line {i}"))?;
        upsert_task_from_event(&tx, &event, project_hash)?;
        index_event(&tx, &event)?;
        count += 1;
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

    Ok(())
}

pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {parent:?}"))?;
    }
    let conn = Connection::open(&path)
        .with_context(|| format!("open SQLite at {:?}", path.as_ref()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    conn.execute_batch(MIGRATION_001).context("apply migration 001")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
            "decisions", "events_index", "evidence", "task_pack_cache", "tasks", "search_fts"
        ] {
            assert!(names.iter().any(|n| n == required), "missing table {required}, have {names:?}");
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
    fn supersede_event_marks_decision_superseded() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = crate::event::Event::new(
            "tj-s", crate::event::EventType::Open,
            crate::event::Author::User, crate::event::Source::Cli, "x".into()
        );
        open_e.meta = serde_json::json!({"title": "T"});
        upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        index_event(&conn, &open_e).unwrap();

        let dec = crate::event::Event::new(
            "tj-s", crate::event::EventType::Decision,
            crate::event::Author::Agent, crate::event::Source::Chat,
            "Use TS".into()
        );
        upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        index_event(&conn, &dec).unwrap();

        let mut sup = crate::event::Event::new(
            "tj-s", crate::event::EventType::Supersede,
            crate::event::Author::Agent, crate::event::Source::Chat,
            "Replaced by Rust decision".into()
        );
        sup.supersedes = Some(dec.event_id.clone());
        upsert_task_from_event(&conn, &sup, "feedface").unwrap();
        index_event(&conn, &sup).unwrap();

        let (status, by): (String, Option<String>) = conn.query_row(
            "SELECT status, superseded_by FROM decisions WHERE decision_id=?1",
            rusqlite::params![dec.event_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(status, "superseded");
        assert_eq!(by.as_deref(), Some(sup.event_id.as_str()));
    }

    #[test]
    fn index_event_projects_decision_to_decisions_table() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();

        let mut open_e = crate::event::Event::new(
            "tj-d", crate::event::EventType::Open,
            crate::event::Author::User, crate::event::Source::Cli, "x".into()
        );
        open_e.meta = serde_json::json!({"title": "T"});
        upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        index_event(&conn, &open_e).unwrap();

        let dec = crate::event::Event::new(
            "tj-d", crate::event::EventType::Decision,
            crate::event::Author::Agent, crate::event::Source::Chat,
            "Adopt Rust".into()
        );
        upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        index_event(&conn, &dec).unwrap();

        let (id, text, status): (String, String, String) = conn.query_row(
            "SELECT decision_id, text, status FROM decisions WHERE task_id=?1",
            rusqlite::params!["tj-d"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(id, dec.event_id);
        assert_eq!(text, "Adopt Rust");
        assert_eq!(status, "active");
    }

    #[test]
    fn rebuild_state_reads_jsonl_and_populates_db() {
        use std::io::Write;
        let d = TempDir::new().unwrap();
        let events_path = d.path().join("events.jsonl");
        let db_path = d.path().join("s.sqlite");

        let mut f = std::fs::File::create(&events_path).unwrap();
        let mut e1 = crate::event::Event::new(
            "tj-9", crate::event::EventType::Open,
            crate::event::Author::User, crate::event::Source::Cli,
            "x".into()
        );
        e1.meta = serde_json::json!({"title": "Nine"});
        let e2 = crate::event::Event::new(
            "tj-9", crate::event::EventType::Decision,
            crate::event::Author::Agent, crate::event::Source::Chat,
            "Adopt Rust".into()
        );
        writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&e2).unwrap()).unwrap();
        drop(f);

        let conn = open(&db_path).unwrap();
        let n = rebuild_state(&conn, &events_path, "deadbeefdeadbeef").unwrap();
        assert_eq!(n, 2);

        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events_index", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn index_event_writes_index_and_fts() {
        let d = TempDir::new().unwrap();
        let conn = open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = crate::event::Event::new(
            "tj-1", crate::event::EventType::Open,
            crate::event::Author::User, crate::event::Source::Cli,
            "Title".into()
        );
        open_e.meta = serde_json::json!({"title": "Title"});
        upsert_task_from_event(&conn, &open_e, "deadbeefdeadbeef").unwrap();
        index_event(&conn, &open_e).unwrap();

        let mut decision = crate::event::Event::new(
            "tj-1", crate::event::EventType::Decision,
            crate::event::Author::Agent, crate::event::Source::Chat,
            "Adopt Rust".into()
        );
        decision.confidence = Some(0.92);
        upsert_task_from_event(&conn, &decision, "deadbeefdeadbeef").unwrap();
        index_event(&conn, &decision).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
            rusqlite::params!["tj-1"], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 2);

        let mut stmt = conn.prepare(
            "SELECT event_id FROM search_fts WHERE search_fts MATCH ?1"
        ).unwrap();
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
            "tj-7f3a", crate::event::EventType::Open,
            crate::event::Author::User, crate::event::Source::Cli,
            "Add OAuth".into()
        );
        e.meta = serde_json::json!({ "title": "Add OAuth login" });

        upsert_task_from_event(&conn, &e, "abcd1234abcd1234").unwrap();

        let (id, title, status): (String, String, String) = conn.query_row(
            "SELECT task_id, title, status FROM tasks WHERE task_id = ?1",
            ["tj-7f3a"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();

        assert_eq!(id, "tj-7f3a");
        assert_eq!(title, "Add OAuth login");
        assert_eq!(status, "open");
    }
}
