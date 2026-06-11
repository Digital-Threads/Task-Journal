//! Compact, read-only reminder of the active task, re-injected after a
//! compaction so the post-compaction agent retains its task + constraints.

use rusqlite::Connection;

pub const MAX_CONSTRAINTS: usize = 3;

/// Most-recent OPEN task → "title + goal + up to MAX_CONSTRAINTS newest
/// constraint texts". `None` when there is no open task. Read-only.
pub fn active_task_reminder(conn: &Connection) -> anyhow::Result<Option<String>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT task_id, title FROM tasks \
             WHERE status='open' ORDER BY last_event_at DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();
    let Some((task_id, title)) = row else {
        return Ok(None);
    };

    let goal = crate::db::task_metadata(conn, &task_id)?
        .and_then(|m| m.goal)
        .filter(|g| !g.trim().is_empty());

    // Same `events_index ei LEFT JOIN search_fts sf` shape the PreCompact
    // marker query and the export-pr walk use; newest constraints first.
    let mut stmt = conn.prepare(
        "SELECT sf.text FROM events_index ei \
         LEFT JOIN search_fts sf ON sf.event_id = ei.event_id \
         WHERE ei.task_id = ?1 AND ei.type = 'constraint' \
         ORDER BY ei.timestamp DESC LIMIT ?2",
    )?;
    let constraints: Vec<String> = stmt
        .query_map(
            rusqlite::params![task_id, MAX_CONSTRAINTS as i64],
            |r| r.get::<_, Option<String>>(0),
        )?
        .filter_map(|r| r.ok().flatten())
        .filter(|t| !t.trim().is_empty())
        .collect();

    let mut out = format!("[Active task after compaction] {task_id} — {title}");
    if let Some(g) = goal {
        out.push_str(&format!("\nGoal: {g}"));
    }
    if !constraints.is_empty() {
        out.push_str("\nConstraints still in force:");
        for c in &constraints {
            out.push_str(&format!("\n  - {c}"));
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::event::{Author, Event, EventStatus, EventType, Source};

    const PH: &str = "ph-test";

    fn open_event(task: &str, title: &str) -> Event {
        let mut e = Event::new(task, EventType::Open, Author::User, Source::Cli, title.into());
        e.meta = serde_json::json!({ "title": title });
        e
    }

    fn constraint_event(task: &str, text: &str, ts: &str) -> Event {
        let mut e = Event::new(task, EventType::Constraint, Author::Agent, Source::Chat, text.into());
        e.status = EventStatus::Confirmed;
        e.timestamp = ts.into();
        e
    }

    fn seed(events: &[Event]) -> (tempfile::TempDir, rusqlite::Connection) {
        let d = tempfile::TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        for e in events {
            db::upsert_task_from_event(&conn, e, PH).unwrap();
            db::index_event(&conn, e).unwrap();
        }
        (d, conn)
    }

    #[test]
    fn reminder_includes_title_goal_and_up_to_3_constraints() {
        // Four constraints with ascending timestamps; only the 3 newest
        // should appear, the oldest must be absent.
        let events = vec![
            open_event("tj-1", "Build the widget"),
            constraint_event("tj-1", "OLDEST: rate limit is 100/min", "2026-06-01T00:00:00Z"),
            constraint_event("tj-1", "API key rotates daily", "2026-06-02T00:00:00Z"),
            constraint_event("tj-1", "Must support offline mode", "2026-06-03T00:00:00Z"),
            constraint_event("tj-1", "NEWEST: ship before Friday", "2026-06-04T00:00:00Z"),
        ];
        let (_d, conn) = seed(&events);
        db::set_task_goal(&conn, "tj-1", "Ship the dashboard widget").unwrap();

        let r = active_task_reminder(&conn).unwrap().unwrap();
        assert!(r.starts_with("[Active task after compaction]"), "got: {r}");
        assert!(r.contains("Build the widget"), "got: {r}");
        assert!(r.contains("Goal: Ship the dashboard widget"), "got: {r}");
        assert!(r.contains("NEWEST: ship before Friday"), "got: {r}");
        assert!(r.contains("Must support offline mode"), "got: {r}");
        assert!(r.contains("API key rotates daily"), "got: {r}");
        assert!(!r.contains("OLDEST"), "oldest constraint leaked: {r}");
    }

    #[test]
    fn reminder_none_when_no_open_task() {
        let (_d, conn) = seed(&[]);
        assert!(active_task_reminder(&conn).unwrap().is_none());
    }

    #[test]
    fn reminder_none_when_task_closed() {
        let mut close = Event::new("tj-1", EventType::Close, Author::User, Source::Cli, "done".into());
        close.timestamp = "2026-06-05T00:00:00Z".into();
        let events = vec![open_event("tj-1", "Build the widget"), close];
        let (_d, conn) = seed(&events);
        assert!(active_task_reminder(&conn).unwrap().is_none());
    }
}
