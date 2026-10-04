//! The project chronicle: modules (parts of the system by meaning), which
//! tasks belong to them, and each module's history. Modules live in the
//! journal as `module` events (`task_id` = `mod:<id>`, partial updates, the
//! last value of a field wins); links ride in `open` / `amend` / `close`
//! meta, which 0.30 already reads and ignores.

use rusqlite::{Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{Author, Event, EventType, Source};

/// `task_id` prefix of a module's events; never collides with `tj-…`.
pub const ID_PREFIX: &str = "mod:";

const MAX_ID: usize = 48;

/// What marks a module in a task: code path prefixes and the module's terms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Hints {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub terms: Vec<String>,
}

/// A partial update of a module: `None` leaves the field as it is.
#[derive(Debug, Clone, Default)]
pub struct ModuleFields {
    pub name: Option<String>,
    pub description: Option<String>,
    pub hints: Option<Hints>,
    pub state: Option<String>,
    pub status: Option<String>,
    pub merged_into: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Module {
    pub module_id: String,
    pub name: String,
    pub description: Option<String>,
    pub hints: Hints,
    /// How the module works now: a short living text the AI rewrites.
    pub state: Option<String>,
    pub state_at: Option<String>,
    pub status: String,
    pub merged_into: Option<String>,
    pub task_count: i64,
    pub last_activity: Option<String>,
}

/// Module ids ready to link, with what was redirected on the way.
#[derive(Debug, Default)]
pub struct Resolved {
    pub ids: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn validate_id(id: &str) -> anyhow::Result<()> {
    let mut chars = id.chars();
    let is_first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let is_rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !is_first_ok || !is_rest_ok || id.len() > MAX_ID {
        anyhow::bail!(
            "invalid module id {id:?}: use lowercase latin letters, digits and dashes, \
             starting with a letter or digit, at most {MAX_ID} chars (e.g. `stars`, `auth-refresh`)"
        );
    }

    Ok(())
}

/// The journal event that creates or updates `module_id` with the given fields.
pub fn module_event(module_id: &str, f: &ModuleFields) -> anyhow::Result<Event> {
    validate_id(module_id)?;
    if let Some(status) = f.status.as_deref() {
        if !["active", "retired", "merged"].contains(&status) {
            anyhow::bail!("module status must be active, retired or merged, not {status:?}");
        }
        if status == "merged" && f.merged_into.is_none() {
            anyhow::bail!("status merged needs merged_into: the module that took over");
        }
    }

    let text = f
        .name
        .clone()
        .or_else(|| f.description.clone())
        .unwrap_or_else(|| format!("module {module_id} updated"));
    let mut event = Event::new(
        format!("{ID_PREFIX}{module_id}"),
        EventType::Module,
        Author::Agent,
        Source::Chat,
        text,
    );

    let mut meta = serde_json::json!({ "module_id": module_id });
    let fields = [
        ("name", &f.name),
        ("description", &f.description),
        ("state", &f.state),
        ("status", &f.status),
        ("merged_into", &f.merged_into),
    ];
    for (key, value) in fields {
        if let Some(v) = value {
            meta[key] = serde_json::Value::String(v.clone());
        }
    }
    if let Some(h) = &f.hints {
        meta["hints"] = serde_json::to_value(h)?;
    }
    event.meta = meta;

    Ok(event)
}

/// The `amend` that links `task_id` to `add` and unlinks it from `remove`.
pub fn link_event(task_id: &str, add: &[String], remove: &[String]) -> Event {
    let mut event = Event::new(
        task_id,
        EventType::Amend,
        Author::Agent,
        Source::Chat,
        "modules changed".to_string(),
    );
    event.meta = serde_json::json!({ "modules_add": add, "modules_remove": remove });

    event
}

/// The value of `close.meta.module_notes`: one history line per module.
pub fn notes_meta(notes: &[(String, String)]) -> serde_json::Value {
    notes
        .iter()
        .map(|(module, text)| serde_json::json!({ "module": module, "text": text }))
        .collect()
}

/// Apply what `event` says about modules to the derived state. Called for
/// every event before its task projection.
pub fn project(conn: &Connection, event: &Event, project_hash: &str) -> anyhow::Result<()> {
    match event.event_type {
        EventType::Module => apply_module(conn, event, project_hash),
        EventType::Open => link(
            conn,
            project_hash,
            &event.task_id,
            &strings(&event.meta, "modules"),
            &[],
        ),
        EventType::Amend => link(
            conn,
            project_hash,
            &event.task_id,
            &strings(&event.meta, "modules_add"),
            &strings(&event.meta, "modules_remove"),
        ),
        EventType::Close => add_notes(conn, project_hash, event),
        _ => Ok(()),
    }
}

fn strings(meta: &serde_json::Value, key: &str) -> Vec<String> {
    meta.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn apply_module(conn: &Connection, event: &Event, project_hash: &str) -> anyhow::Result<()> {
    let Some(id) = event.task_id.strip_prefix(ID_PREFIX) else {
        return Ok(());
    };
    if validate_id(id).is_err() {
        tracing::warn!(
            task_id = event.task_id.as_str(),
            "skipping a module event with an invalid id"
        );
        return Ok(());
    }

    let field = |k: &str| {
        event
            .meta
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let hints = event.meta.get("hints").map(|v| v.to_string());
    conn.execute(
        "INSERT INTO modules(project_hash, module_id, name, description, hints, state, state_at,
                             status, merged_into, created_at, updated_at)
         VALUES (?1, ?2, COALESCE(?3, ?2), ?4, COALESCE(?5, '{}'), ?6,
                 CASE WHEN ?6 IS NULL THEN NULL ELSE ?9 END, COALESCE(?7, 'active'), ?8, ?9, ?9)
         ON CONFLICT(project_hash, module_id) DO UPDATE SET
             name        = COALESCE(?3, name),
             description = COALESCE(?4, description),
             hints       = COALESCE(?5, hints),
             state       = COALESCE(?6, state),
             state_at    = CASE WHEN ?6 IS NULL THEN state_at ELSE ?9 END,
             status      = COALESCE(?7, status),
             merged_into = COALESCE(?8, merged_into),
             updated_at  = ?9",
        rusqlite::params![
            project_hash,
            id,
            field("name"),
            field("description"),
            hints,
            field("state"),
            field("status"),
            field("merged_into"),
            event.timestamp
        ],
    )?;

    Ok(())
}

fn link(
    conn: &Connection,
    project_hash: &str,
    task_id: &str,
    add: &[String],
    remove: &[String],
) -> anyhow::Result<()> {
    for m in add {
        conn.execute(
            "INSERT OR IGNORE INTO task_modules(project_hash, task_id, module_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![project_hash, task_id, m],
        )?;
    }
    for m in remove {
        conn.execute(
            "DELETE FROM task_modules WHERE task_id = ?1 AND module_id = ?2",
            rusqlite::params![task_id, m],
        )?;
    }

    // The pack names the task's modules.
    if !add.is_empty() || !remove.is_empty() {
        crate::db::invalidate_pack_cascade(conn, task_id)?;
    }

    Ok(())
}

fn add_notes(conn: &Connection, project_hash: &str, event: &Event) -> anyhow::Result<()> {
    let Some(notes) = event.meta.get("module_notes").and_then(|v| v.as_array()) else {
        return Ok(());
    };

    for note in notes {
        let module = note.get("module").and_then(|v| v.as_str());
        let text = note.get("text").and_then(|v| v.as_str());
        let (Some(module), Some(text)) = (module, text) else {
            continue;
        };

        conn.execute(
            "INSERT OR IGNORE INTO module_notes(project_hash, event_id, module_id, task_id, text, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                project_hash,
                event.event_id,
                module,
                event.task_id,
                text,
                event.timestamp
            ],
        )?;
        // A task that writes a module's history belongs to it.
        link(
            conn,
            project_hash,
            &event.task_id,
            &[module.to_string()],
            &[],
        )?;
    }

    Ok(())
}

/// Drop this project's module rows before a replay re-creates them.
pub fn clear(conn: &Connection, project_hash: &str) -> anyhow::Result<()> {
    for table in ["modules", "task_modules", "module_notes"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE project_hash = ?1"),
            [project_hash],
        )?;
    }

    Ok(())
}

const MODULE_SELECT: &str = "SELECT m.module_id, m.name, m.description, m.hints, m.state,
        m.state_at, m.status, m.merged_into,
        (SELECT COUNT(*) FROM task_modules tm
          WHERE tm.project_hash = m.project_hash AND tm.module_id = m.module_id),
        (SELECT MAX(t.last_event_at) FROM task_modules tm JOIN tasks t ON t.task_id = tm.task_id
          WHERE tm.project_hash = m.project_hash AND tm.module_id = m.module_id)
     FROM modules m WHERE m.project_hash = ?1";

fn module_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Module> {
    let hints: String = r.get(3)?;

    Ok(Module {
        module_id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        hints: serde_json::from_str(&hints).unwrap_or_default(),
        state: r.get(4)?,
        state_at: r.get(5)?,
        status: r.get(6)?,
        merged_into: r.get(7)?,
        task_count: r.get(8)?,
        last_activity: r.get(9)?,
    })
}

pub fn get(conn: &Connection, project_hash: &str, id: &str) -> anyhow::Result<Option<Module>> {
    Ok(conn
        .query_row(
            &format!("{MODULE_SELECT} AND m.module_id = ?2"),
            rusqlite::params![project_hash, id],
            module_row,
        )
        .optional()?)
}

/// The project's modules, active ones first, then by id.
pub fn list(conn: &Connection, project_hash: &str) -> anyhow::Result<Vec<Module>> {
    let mut stmt = conn.prepare(&format!(
        "{MODULE_SELECT} ORDER BY m.status = 'active' DESC, m.module_id"
    ))?;
    let rows = stmt
        .query_map([project_hash], module_row)?
        .collect::<Result<_, _>>()?;

    Ok(rows)
}

/// `(module_id, name)` of every module `task_id` belongs to.
pub fn modules_of_task(conn: &Connection, task_id: &str) -> anyhow::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT tm.module_id, COALESCE(m.name, tm.module_id) FROM task_modules tm
         LEFT JOIN modules m ON m.project_hash = tm.project_hash AND m.module_id = tm.module_id
         WHERE tm.task_id = ?1 ORDER BY tm.module_id",
    )?;
    let rows = stmt
        .query_map([task_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;

    Ok(rows)
}

/// Check module ids before a write links a task to them: an unknown id is
/// an error, a merged module stands for the one that took it over.
pub fn resolve(conn: &Connection, project_hash: &str, ids: &[String]) -> anyhow::Result<Resolved> {
    let mut out = Resolved::default();

    for id in ids {
        validate_id(id)?;
        let Some(m) = get(conn, project_hash, id)? else {
            anyhow::bail!(
                "module {id:?} does not exist — create it first with module_save (module_list shows the map)"
            );
        };

        let target = match (m.status.as_str(), m.merged_into) {
            ("merged", Some(into)) => {
                out.warnings.push(format!(
                    "module {id} was merged into {into}; linked to {into}"
                ));
                into
            }
            _ => m.module_id,
        };
        if !out.ids.contains(&target) {
            out.ids.push(target);
        }
    }

    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests_support {
    use rusqlite::Connection;
    use tempfile::TempDir;

    use crate::event::{Author, Event, EventType, Source};

    /// A fresh state rebuilt from a journal holding `events`, in order.
    pub(crate) fn journal(events: &[Event]) -> (TempDir, Connection) {
        let d = TempDir::new().unwrap();
        let log = d.path().join("e.jsonl");
        let mut w = crate::storage::JsonlWriter::open(&log).unwrap();
        for e in events {
            w.append(e).unwrap();
        }

        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();
        crate::db::rebuild_state(&conn, &log, "p").unwrap();

        (d, conn)
    }

    pub(crate) fn open_task(id: &str, modules: &[&str]) -> Event {
        let mut e = Event::new(id, EventType::Open, Author::Agent, Source::Chat, id.into());
        e.meta = serde_json::json!({"title": id, "modules": modules});

        e
    }

    pub(crate) fn close_task(id: &str, meta: serde_json::Value) -> Event {
        let mut e = Event::new(
            id,
            EventType::Close,
            Author::Agent,
            Source::Chat,
            "done".into(),
        );
        e.meta = meta;

        e
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::tests_support::{close_task, journal, open_task};
    use super::*;
    use crate::event::{Author, Event, EventType, Source};

    fn named(id: &str, name: &str) -> Event {
        module_event(
            id,
            &ModuleFields {
                name: Some(name.into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn rebuild_restores_modules_links_and_notes() {
        let state = module_event(
            "stars",
            &ModuleFields {
                state: Some("Feed ranks by score".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let close = close_task(
            "tj-a",
            serde_json::json!({
                "module_notes": notes_meta(&[("stars".into(), "Added ranking".into())])
            }),
        );

        let (_d, conn) = journal(&[
            named("stars", "Stars"),
            state,
            open_task("tj-a", &["stars"]),
            close,
        ]);

        let m = get(&conn, "p", "stars").unwrap().unwrap();
        // The later partial event changed the state and kept the name.
        assert_eq!(m.name, "Stars");
        assert_eq!(m.state.as_deref(), Some("Feed ranks by score"));
        assert!(m.state_at.is_some());
        assert_eq!(m.status, "active");
        assert_eq!(m.task_count, 1);
        assert_eq!(
            modules_of_task(&conn, "tj-a").unwrap(),
            vec![("stars".to_string(), "Stars".to_string())]
        );
        assert_eq!(count(&conn, "module_notes"), 1);
    }

    #[test]
    fn rebuild_twice_does_not_duplicate() {
        let close = close_task(
            "tj-a",
            serde_json::json!({"module_notes": notes_meta(&[("stars".into(), "x".into())])}),
        );
        let d = TempDir::new().unwrap();
        let log = d.path().join("e.jsonl");
        let mut w = crate::storage::JsonlWriter::open(&log).unwrap();
        for e in [
            named("stars", "Stars"),
            open_task("tj-a", &["stars"]),
            close,
        ] {
            w.append(&e).unwrap();
        }
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();

        crate::db::rebuild_state(&conn, &log, "p").unwrap();
        crate::db::rebuild_state(&conn, &log, "p").unwrap();

        assert_eq!(count(&conn, "modules"), 1);
        assert_eq!(count(&conn, "module_notes"), 1);
        assert_eq!(count(&conn, "task_modules"), 1);
    }

    #[test]
    fn amend_adds_and_removes_links() {
        let (_d, conn) = journal(&[
            named("a", "A"),
            named("b", "B"),
            open_task("tj-a", &["a"]),
            link_event("tj-a", &["b".into()], &["a".into()]),
        ]);

        assert_eq!(
            modules_of_task(&conn, "tj-a").unwrap(),
            vec![("b".to_string(), "B".to_string())]
        );
    }

    #[test]
    fn a_note_links_its_task_to_the_module() {
        let close = close_task(
            "tj-a",
            serde_json::json!({"module_notes": notes_meta(&[("a".into(), "x".into())])}),
        );

        let (_d, conn) = journal(&[named("a", "A"), open_task("tj-a", &[]), close]);

        assert_eq!(modules_of_task(&conn, "tj-a").unwrap().len(), 1);
    }

    #[test]
    fn resolve_rejects_unknown_and_redirects_merged() {
        let old = module_event(
            "old",
            &ModuleFields {
                name: Some("Old".into()),
                status: Some("merged".into()),
                merged_into: Some("a".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let (_d, conn) = journal(&[named("a", "A"), old]);

        let r = resolve(&conn, "p", &["old".into(), "a".into()]).unwrap();
        assert_eq!(r.ids, vec!["a".to_string()]);
        assert!(r.warnings[0].contains("merged into a"), "{:?}", r.warnings);

        let err = resolve(&conn, "p", &["nope".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("module_save"), "{err}");
    }

    #[test]
    fn bad_slugs_are_rejected_with_an_example() {
        let long = "x".repeat(49);
        for bad in ["", "Stars", "-x", "a b", long.as_str(), "мод"] {
            let err = validate_id(bad).unwrap_err().to_string();
            assert!(err.contains("auth-refresh"), "{bad}: {err}");
        }

        validate_id("auth-refresh").unwrap();
        validate_id(&"x".repeat(48)).unwrap();
    }

    #[test]
    fn merged_without_a_target_is_refused() {
        let err = module_event(
            "old",
            &ModuleFields {
                status: Some("merged".into()),
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("merged_into"), "{err}");
    }

    #[test]
    fn a_module_line_indexed_past_by_an_older_binary_is_still_projected() {
        // 0.30 cannot parse a `module` line: it indexes past it and writes
        // its own marks. 0.31 must notice the foreign marker and replay.
        let d = TempDir::new().unwrap();
        let log = d.path().join("e.jsonl");
        let mut w = crate::storage::JsonlWriter::open(&log).unwrap();
        let after = Event::new(
            "tj-a",
            EventType::Finding,
            Author::Agent,
            Source::Chat,
            "written after the module line".into(),
        );
        for e in [
            open_task("tj-a", &[]),
            named("stars", "Stars"),
            after.clone(),
        ] {
            w.append(&e).unwrap();
        }
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();
        crate::db::ingest_new_events(&conn, &log, "p").unwrap();

        // What 0.30 leaves behind once it indexed the tail: no module row,
        // index_state and projection_state on the last line it parsed, the
        // 0.31 mark where 0.31 left it.
        conn.execute_batch("DELETE FROM modules;").unwrap();
        for table in ["index_state", "projection_state"] {
            conn.execute(
                &format!("UPDATE {table} SET last_indexed_event_id = ?1"),
                [&after.event_id],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE projection_state_014 SET last_indexed_event_id = 'before-0.30'",
            [],
        )
        .unwrap();
        crate::db::ingest_new_events(&conn, &log, "p").unwrap();

        assert!(get(&conn, "p", "stars").unwrap().is_some());
    }
}
