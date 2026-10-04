//! What the project chronicle is missing, most important first, with the
//! next step for the AI. Computed from the derived state on every read, so
//! there is nothing to keep in sync.

use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Gap {
    /// Tasks exist, but no module does.
    NoMap { tasks: i64 },
    /// Tasks that belong to no module.
    UnlinkedTasks { count: i64 },
    /// Tasks closed in a module since its state was last written.
    StaleModule {
        module_id: String,
        closed_since: i64,
    },
    /// The session's active task belongs to no module.
    TaskWithoutModule { task_id: String },
}

impl Gap {
    /// One line: what is missing and what to do about it.
    pub fn action(&self) -> String {
        match self {
            Gap::NoMap { tasks } => format!(
                "no module map yet ({tasks} tasks) — map the project into modules and confirm them \
                 with the user (Claude Code: /task-journal:map; otherwise the task-journal skill, \
                 section Chronicle)"
            ),
            Gap::UnlinkedTasks { count } => format!(
                "{count} task(s) belong to no module — sort them with module_backfill_candidates, \
                 confirm with the user, then module_link (leftovers go to a catch-all module)"
            ),
            Gap::StaleModule {
                module_id,
                closed_since,
            } => format!(
                "module {module_id}: {closed_since} closed task(s) not reflected in its state — \
                 read module_page({module_id}) and rewrite it with module_save(state=...)"
            ),
            Gap::TaskWithoutModule { task_id } => format!(
                "the active task {task_id} belongs to no module — link it with module_link \
                 (module_list shows the map)"
            ),
        }
    }
}

/// The line a session start or a tool reply shows: the most important gap.
pub fn headline(gaps: &[Gap]) -> Option<String> {
    let first = gaps.first()?;
    let more = match gaps.len() {
        1 => String::new(),
        n => format!(" (+{} more)", n - 1),
    };

    Some(format!("📚 Chronicle: {}{more}", first.action()))
}

/// What the chronicle is missing, most important first, as seen from the
/// journal `conn` (`project_hash`): the map comes from the chronicle's home,
/// tasks from this journal, and a module's lag from every journal.
/// `active_task` is the session's task, checked for a module of its own.
pub fn gaps(
    chr: &crate::chronicle::Chronicle,
    conn: &Connection,
    project_hash: &str,
    active_task: Option<&str>,
) -> anyhow::Result<Vec<Gap>> {
    let count = |sql: &str| -> anyhow::Result<i64> {
        Ok(conn.query_row(sql, [project_hash], |r| r.get(0))?)
    };

    let tasks = count("SELECT COUNT(*) FROM tasks WHERE project_hash = ?1")?;
    if tasks == 0 {
        return Ok(Vec::new());
    }
    let modules: Vec<crate::modules::Module> = crate::modules::list(&chr.home, &chr.home_hash)?;
    if modules.is_empty() {
        return Ok(vec![Gap::NoMap { tasks }]);
    }

    // What the agent can act on right now comes first: its own task, then a
    // module whose state lags. Sorting old tasks needs the user, so it waits.
    let mut out = Vec::new();

    if let Some(task_id) = active_task {
        if crate::modules::modules_of_task(conn, task_id)?.is_empty() {
            out.push(Gap::TaskWithoutModule {
                task_id: task_id.to_string(),
            });
        }
    }

    let mut stale = Vec::new();
    for m in modules.iter().filter(|m| m.status == "active") {
        let mut closed_since = 0;
        for journal in chr.journals() {
            closed_since += journal.query_row(
                "SELECT COUNT(*) FROM task_modules tm
                   JOIN tasks t ON t.task_id = tm.task_id AND t.status = 'closed'
                  WHERE tm.module_id = ?1
                    AND (?2 IS NULL OR COALESCE(t.closed_at, t.last_event_at) > ?2)",
                rusqlite::params![m.module_id, m.state_at],
                |r| r.get::<_, i64>(0),
            )?;
        }
        if closed_since > 0 {
            stale.push(Gap::StaleModule {
                module_id: m.module_id.clone(),
                closed_since,
            });
        }
    }
    stale.sort_by(|a, b| match (a, b) {
        (
            Gap::StaleModule {
                module_id: x,
                closed_since: n,
            },
            Gap::StaleModule {
                module_id: y,
                closed_since: k,
            },
        ) => k.cmp(n).then_with(|| x.cmp(y)),
        _ => std::cmp::Ordering::Equal,
    });
    out.extend(stale);

    let unlinked = count(
        "SELECT COUNT(*) FROM tasks t WHERE t.project_hash = ?1
           AND NOT EXISTS (SELECT 1 FROM task_modules tm WHERE tm.task_id = t.task_id)",
    )?;
    if unlinked > 0 {
        out.push(Gap::UnlinkedTasks { count: unlinked });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tests_support::{chronicle_of, close_task, journal, open_task};
    use crate::modules::{module_event, ModuleFields};

    fn stars(state: Option<&str>) -> crate::event::Event {
        module_event(
            "stars",
            &ModuleFields {
                name: Some("Stars".into()),
                state: state.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn an_empty_journal_has_no_gaps() {
        let (_d, conn) = journal(&[]);

        assert!(gaps(&chronicle_of(&_d), &conn, "p", None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn tasks_without_any_module_is_no_map() {
        let (_d, conn) = journal(&[open_task("tj-a", &[]), open_task("tj-b", &[])]);

        assert_eq!(
            gaps(&chronicle_of(&_d), &conn, "p", None).unwrap(),
            vec![Gap::NoMap { tasks: 2 }]
        );
    }

    #[test]
    fn a_mapped_project_reports_unlinked_stale_and_the_active_task() {
        let mut close = close_task("tj-a", serde_json::json!({}));
        // Closed after the module's state was written.
        close.timestamp = "2999-01-01T00:00:00.000Z".into();

        let (_d, conn) = journal(&[
            stars(Some("v1")),
            open_task("tj-a", &["stars"]),
            close,
            open_task("tj-b", &[]),
        ]);
        let g = gaps(&chronicle_of(&_d), &conn, "p", Some("tj-b")).unwrap();

        assert_eq!(
            g,
            // What the agent can act on now comes first; sorting old
            // tasks waits behind it.
            vec![
                Gap::TaskWithoutModule {
                    task_id: "tj-b".into()
                },
                Gap::StaleModule {
                    module_id: "stars".into(),
                    closed_since: 1
                },
                Gap::UnlinkedTasks { count: 1 },
            ]
        );
    }

    #[test]
    fn a_module_with_a_fresh_state_is_not_stale() {
        let (_d, conn) = journal(&[
            open_task("tj-a", &[]),
            close_task("tj-a", serde_json::json!({})),
            stars(Some("written after the task closed")),
            crate::modules::link_event("tj-a", &["stars".into()], &[]),
        ]);

        assert!(gaps(&chronicle_of(&_d), &conn, "p", None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn closed_tasks_in_a_module_without_state_are_stale() {
        let (_d, conn) = journal(&[
            stars(None),
            open_task("tj-a", &["stars"]),
            close_task("tj-a", serde_json::json!({})),
        ]);

        assert_eq!(
            gaps(&chronicle_of(&_d), &conn, "p", None).unwrap(),
            vec![Gap::StaleModule {
                module_id: "stars".into(),
                closed_since: 1
            }]
        );
    }

    #[test]
    fn the_headline_names_the_first_gap_and_counts_the_rest() {
        let g = vec![Gap::NoMap { tasks: 3 }, Gap::UnlinkedTasks { count: 2 }];

        let line = headline(&g).unwrap();

        assert!(
            line.starts_with("📚 Chronicle: no module map yet (3 tasks)"),
            "{line}"
        );
        assert!(line.ends_with("(+1 more)"), "{line}");
        assert!(headline(&[]).is_none());
    }

    #[test]
    fn gaps_serialize_with_a_kind_tag() {
        let json = serde_json::to_string(&Gap::UnlinkedTasks { count: 2 }).unwrap();

        assert_eq!(json, r#"{"kind":"unlinked_tasks","count":2}"#);
    }
}
