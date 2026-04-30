//! Pack assembler: turns events + derived state into compact resume Markdown.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackMode { Compact, Full }

#[derive(Debug, Clone, Serialize)]
pub struct TaskPack {
    pub task_id: String,
    pub mode: PackMode,
    pub schema_version: String,
    pub text: String,
    pub metadata: PackMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackMetadata {
    pub generated_at: String,
    pub source_event_count: usize,
    pub cache_hit: bool,
}

use anyhow::Context;
use rusqlite::Connection;

fn render_active_decisions(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Active decisions\n");
    let mut stmt = conn.prepare(
        "SELECT text FROM decisions WHERE task_id=?1 AND status='active' ORDER BY decision_id ASC"
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| r.get::<_, String>(0))?;
    let mut count = 0;
    for row in rows {
        out.push_str(&format!("- {}\n", row?));
        count += 1;
    }
    if count == 0 { out.push_str("- (none)\n"); }
    out.push('\n');
    Ok(out)
}

fn render_lifecycle(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Lifecycle\n");
    let mut stmt = conn.prepare(
        "SELECT timestamp, type FROM events_index
         WHERE task_id=?1 AND type IN ('open','close','reopen','supersede','redirect')
         ORDER BY timestamp ASC"
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| {
        let ts: String = r.get(0)?;
        let ty: String = r.get(1)?;
        Ok((ts, ty))
    })?;
    let mut count = 0;
    for row in rows {
        let (ts, ty) = row?;
        let verb = match ty.as_str() {
            "open" => "opened",
            "close" => "closed",
            "reopen" => "reopened",
            "supersede" => "superseded",
            "redirect" => "redirected",
            _ => &ty,
        };
        out.push_str(&format!("- {ts} {verb}\n"));
        count += 1;
    }
    if count == 0 { out.push_str("- (none)\n"); }
    out.push('\n');
    Ok(out)
}

pub fn assemble(conn: &Connection, task_id: &str, mode: PackMode) -> anyhow::Result<TaskPack> {
    let (title, status): (String, String) = conn.query_row(
        "SELECT title, status FROM tasks WHERE task_id=?1",
        rusqlite::params![task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).with_context(|| format!("task not found: {task_id}"))?;

    let event_count: usize = conn.query_row(
        "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
        rusqlite::params![task_id],
        |r| r.get::<_, i64>(0).map(|n| n as usize),
    )?;

    let mut text = format!("# {title}  [status: {status}]\n\n");
    text.push_str(&render_lifecycle(conn, task_id)?);
    text.push_str(&render_active_decisions(conn, task_id)?);

    Ok(TaskPack {
        task_id: task_id.to_string(),
        mode,
        schema_version: "1.0".into(),
        text,
        metadata: PackMetadata {
            generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            source_event_count: event_count,
            cache_hit: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_mode_round_trips_via_serde() {
        let s = serde_json::to_string(&PackMode::Compact).unwrap();
        assert_eq!(s, "\"Compact\"");
    }

    #[test]
    fn pack_renders_active_decisions() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new("tj-ad", EventType::Open, Author::User, Source::Cli, "x".into());
        open_e.meta = serde_json::json!({"title": "Decisions test"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let dec = Event::new("tj-ad", EventType::Decision, Author::Agent, Source::Chat, "Adopt Rust".into());
        db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        db::index_event(&conn, &dec).unwrap();

        let pack = assemble(&conn, "tj-ad", PackMode::Full).unwrap();
        assert!(pack.text.contains("## Active decisions"), "missing section: {}", pack.text);
        assert!(pack.text.contains("Adopt Rust"), "decision text missing: {}", pack.text);
    }

    #[test]
    fn assemble_includes_lifecycle_history() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();

        let mut open_e = Event::new("tj-l", EventType::Open, Author::User, Source::Cli, "x".into());
        open_e.meta = serde_json::json!({"title": "Lifecycle"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let close_e = Event::new("tj-l", EventType::Close, Author::User, Source::Cli, "done".into());
        db::upsert_task_from_event(&conn, &close_e, "feedface").unwrap();
        db::index_event(&conn, &close_e).unwrap();

        let pack = assemble(&conn, "tj-l", PackMode::Full).unwrap();
        assert!(pack.text.contains("## Lifecycle"));
        assert!(pack.text.contains("opened"));
        assert!(pack.text.contains("closed"));
    }

    #[test]
    fn assemble_header_only_compact() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();

        let mut open_e = Event::new("tj-h", EventType::Open, Author::User, Source::Cli, "x".into());
        open_e.meta = serde_json::json!({"title": "Header test"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let pack = assemble(&conn, "tj-h", PackMode::Compact).unwrap();
        assert!(pack.text.contains("# Header test"), "header missing: {}", pack.text);
        assert!(pack.text.contains("status: open"), "status missing: {}", pack.text);
        assert_eq!(pack.metadata.source_event_count, 1);
        assert!(!pack.metadata.cache_hit);
    }
}
