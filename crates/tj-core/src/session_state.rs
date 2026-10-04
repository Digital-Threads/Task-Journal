//! What an integration shows for one session: the session's active task,
//! what that task holds so far, and how many tasks are open. Read by the
//! Claude Code mod through `task-journal state`; any client can call it.
//!
//! The active task is the open task this session last wrote to (by
//! `meta.session_id`), never "the newest open task", so two sessions in one
//! project don't share a task.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// Bumped when a field changes meaning; readers refuse other schemas.
pub const SCHEMA: &str = "tj-state/1";

/// How many of the task's latest entries ride along: enough for a model to
/// tell what is already recorded, small enough to stay cheap.
const RECENT: i64 = 20;

#[derive(Debug, Serialize)]
pub struct SessionState {
    pub schema: &'static str,
    pub session_id: Option<String>,
    pub open_tasks: i64,
    pub active: Option<ActiveTask>,
}

#[derive(Debug, Serialize)]
pub struct ActiveTask {
    pub task_id: String,
    pub title: String,
    pub goal: Option<String>,
    /// Entries per event type, e.g. `{"decision": 4, "evidence": 1}`.
    pub counts: BTreeMap<String, i64>,
    /// The latest entries, oldest first. No `open` / `amend` bookkeeping.
    pub recent: Vec<RecentEntry>,
}

#[derive(Debug, Serialize)]
pub struct RecentEntry {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

/// Assemble the state of `session_id` in one project. The caller has
/// ingested the journal into `conn` already. `prefer` names the task the
/// caller already shows: it stays the active one while it is open, so a
/// session logging to a parent and a subtask in turn doesn't flip between
/// them.
pub fn session_state(
    conn: &Connection,
    project_hash: &str,
    session_id: Option<&str>,
    prefer: Option<&str>,
) -> anyhow::Result<SessionState> {
    let open_tasks = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE project_hash = ?1 AND status = 'open'",
        [project_hash],
        |r| r.get::<_, i64>(0),
    )?;

    let preferred = match prefer {
        Some(id) => conn
            .query_row(
                "SELECT task_id FROM tasks WHERE task_id = ?1 AND project_hash = ?2 AND status = 'open'",
                [id, project_hash],
                |r| r.get::<_, String>(0),
            )
            .optional()?,
        None => None,
    };
    let active_id = match (preferred, session_id) {
        (Some(id), _) => Some(id),
        (None, Some(sid)) => crate::db::active_task_for_session(conn, project_hash, sid)?,
        (None, None) => None,
    };
    let active = active_id.map(|id| active_task(conn, &id)).transpose()?;

    Ok(SessionState {
        schema: SCHEMA,
        session_id: session_id.map(str::to_string),
        open_tasks,
        active,
    })
}

fn active_task(conn: &Connection, task_id: &str) -> anyhow::Result<ActiveTask> {
    let (title, goal) = conn.query_row(
        "SELECT title, goal FROM tasks WHERE task_id = ?1",
        [task_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    )?;

    let mut counts = BTreeMap::new();
    let mut stmt =
        conn.prepare("SELECT type, COUNT(*) FROM events_index WHERE task_id = ?1 GROUP BY type")?;
    for row in stmt.query_map([task_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (kind, n) = row?;
        counts.insert(kind, n);
    }

    let mut stmt = conn.prepare(
        "SELECT e.type, f.text FROM events_index e
         JOIN search_fts f ON f.event_id = e.event_id
         WHERE e.task_id = ?1 AND e.type NOT IN ('open', 'amend')
         ORDER BY e.timestamp DESC, e.event_id DESC LIMIT ?2",
    )?;
    let mut recent = stmt
        .query_map(rusqlite::params![task_id, RECENT], |r| {
            Ok(RecentEntry {
                kind: r.get(0)?,
                text: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    recent.reverse();

    Ok(ActiveTask {
        task_id: task_id.to_string(),
        title,
        goal: goal.filter(|g| !g.trim().is_empty()),
        counts,
        recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Author, Event, EventType, Source};

    fn write(conn: &Connection, task: &str, kind: EventType, text: &str, session: Option<&str>) {
        let mut event = Event::new(task, kind, Author::Agent, Source::Chat, text.into());
        if kind == EventType::Open {
            event.meta =
                serde_json::json!({ "title": format!("Title of {task}"), "goal": "Ship it" });
        }
        crate::session_id::stamp_session_id(&mut event.meta, session);

        crate::db::upsert_task_from_event(conn, &event, "p").unwrap();
        crate::db::index_event(conn, &event).unwrap();
    }

    // The TempDir rides along so the database outlives the test body.
    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(dir.path().join("s.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn active_task_is_the_sessions_own_not_the_newest() {
        let (_dir, conn) = conn();
        write(&conn, "tj-a", EventType::Open, "a", Some("s1"));
        write(&conn, "tj-a", EventType::Decision, "Use <=", Some("s1"));
        write(&conn, "tj-b", EventType::Open, "b", Some("s2"));

        let s1 = session_state(&conn, "p", Some("s1"), None).unwrap();
        let active = s1.active.expect("s1 has a task");
        assert_eq!(active.task_id, "tj-a");
        assert_eq!(active.title, "Title of tj-a");
        assert_eq!(active.goal.as_deref(), Some("Ship it"));
        assert_eq!(active.counts.get("decision"), Some(&1));
        assert_eq!(active.recent.len(), 1);
        assert_eq!(active.recent[0].kind, "decision");
        assert_eq!(s1.open_tasks, 2);

        let other = session_state(&conn, "p", Some("s3"), None).unwrap();
        assert!(other.active.is_none());
        assert_eq!(other.open_tasks, 2);
    }

    #[test]
    fn a_preferred_open_task_stays_active() {
        let (_dir, conn) = conn();
        write(&conn, "tj-parent", EventType::Open, "p", Some("s1"));
        write(&conn, "tj-sub", EventType::Open, "s", Some("s1"));

        // Without a preference the newest write wins; with one, the pin holds.
        let plain = session_state(&conn, "p", Some("s1"), None).unwrap();
        assert_eq!(plain.active.unwrap().task_id, "tj-sub");
        let pinned = session_state(&conn, "p", Some("s1"), Some("tj-parent")).unwrap();
        assert_eq!(pinned.active.unwrap().task_id, "tj-parent");

        // A closed or unknown preference falls back to the session's own task.
        write(&conn, "tj-parent", EventType::Close, "done", Some("s1"));
        let closed = session_state(&conn, "p", Some("s1"), Some("tj-parent")).unwrap();
        assert_eq!(closed.active.unwrap().task_id, "tj-sub");
        let unknown = session_state(&conn, "p", Some("s1"), Some("tj-nope")).unwrap();
        assert_eq!(unknown.active.unwrap().task_id, "tj-sub");
    }

    #[test]
    fn a_closed_task_is_no_longer_active() {
        let (_dir, conn) = conn();
        write(&conn, "tj-a", EventType::Open, "a", Some("s1"));
        write(&conn, "tj-a", EventType::Close, "done", Some("s1"));

        let s = session_state(&conn, "p", Some("s1"), None).unwrap();
        assert!(s.active.is_none());
        assert_eq!(s.open_tasks, 0);
    }

    #[test]
    fn json_shape_is_the_contract() {
        let (_dir, conn) = conn();
        write(&conn, "tj-a", EventType::Open, "a", Some("s1"));

        let v = serde_json::to_value(session_state(&conn, "p", Some("s1"), None).unwrap()).unwrap();
        assert_eq!(v["schema"], "tj-state/1");
        assert_eq!(v["session_id"], "s1");
        assert_eq!(v["active"]["task_id"], "tj-a");
        assert!(v["active"]["recent"].as_array().unwrap().is_empty());

        let none = serde_json::to_value(session_state(&conn, "p", None, None).unwrap()).unwrap();
        assert!(none["active"].is_null());
        assert!(none["session_id"].is_null());
    }
}
