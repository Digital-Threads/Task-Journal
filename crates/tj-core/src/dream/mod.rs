//! Dream — offline memory passes over session transcripts.
//!
//! Pass A (backfill): re-read a session transcript and append the
//! significant typed events the realtime classifier missed. Additive —
//! the JSONL source of truth is never mutated.

pub mod backend;
pub mod backfill;
pub mod llm_backend;
pub mod prompt;
pub mod scope;
pub mod state;

use crate::dream::backend::{BackfillInput, DreamBackend};

pub struct DreamOptions {
    pub project_hash: String,
    /// If true, do not call the backend or write anything; report scope only.
    pub dry_run: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct DreamReport {
    pub sessions_processed: usize,
    pub events_backfilled: usize,
}

/// Run one dream Pass A over the given sessions, using the supplied
/// backend. `sessions` is a list of (session_id, BackfillInput) the
/// caller has already assembled from transcripts + existing events.
pub fn run_dream(
    conn: &rusqlite::Connection,
    events_path: &std::path::Path,
    opts: &DreamOptions,
    backend: &dyn DreamBackend,
    sessions: Vec<(String, BackfillInput)>,
    run_id: &str,
) -> anyhow::Result<DreamReport> {
    let mut report = DreamReport::default();
    // Texts appended earlier in this run: a later session of the same task
    // must not re-add them (its `existing_events` predate the run).
    let mut appended: Vec<String> = Vec::new();
    for (session_id, input) in sessions {
        report.sessions_processed += 1;
        if opts.dry_run {
            continue;
        }
        let proposed = backend.backfill(&input)?;
        // Flatten existing texts across candidate tasks for the guard.
        let existing: Vec<String> = input
            .tasks
            .iter()
            .flat_map(|t| t.existing_events.clone())
            .chain(appended.iter().cloned())
            .collect();
        let kept = crate::dream::backfill::dedup_guard(proposed, &existing);
        let mut writer = crate::storage::JsonlWriter::open(events_path)?;
        for b in &kept {
            let e = crate::dream::backfill::to_event(b, run_id, &session_id);
            writer.append(&e)?;
            crate::db::upsert_task_from_event(conn, &e, &opts.project_hash)?;
            crate::db::index_event(conn, &e)?;
            appended.push(b.text.clone());
            report.events_backfilled += 1;
        }
        writer.flush_durable()?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dream::backend::{BackfillEvent, BackfillTaskContext, MockDreamBackend};
    use crate::event::{Author, Event, EventType, Source};
    use tempfile::TempDir;

    fn task_input() -> (String, BackfillInput) {
        (
            "sess-1".to_string(),
            BackfillInput {
                tasks: vec![BackfillTaskContext {
                    task_id: "tj-1".into(),
                    title: "Demo".into(),
                    existing_events: vec!["Already known fact.".into()],
                }],
                transcript: "user: ...\nassistant: ...".into(),
            },
        )
    }

    #[test]
    fn run_dream_appends_novel_events_and_indexes() {
        let d = TempDir::new().unwrap();
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();
        let events_path = d.path().join("events.jsonl");

        // Seed the task so upsert/index has a home (open event).
        let open = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Cli,
            "Demo".into(),
        );
        crate::db::upsert_task_from_event(&conn, &open, "ph").unwrap();

        let backend = MockDreamBackend {
            events: vec![
                BackfillEvent {
                    event_type: EventType::Finding,
                    task_id: "tj-1".into(),
                    text: "A brand new finding.".into(),
                    timestamp: "2026-06-08T10:00:00Z".into(),
                },
                BackfillEvent {
                    event_type: EventType::Finding,
                    task_id: "tj-1".into(),
                    text: "Already known fact.".into(), // dup → dropped
                    timestamp: "2026-06-08T10:01:00Z".into(),
                },
            ],
        };
        let opts = DreamOptions {
            project_hash: "ph".into(),
            dry_run: false,
        };
        let report = run_dream(
            &conn,
            &events_path,
            &opts,
            &backend,
            vec![task_input()],
            "run-1",
        )
        .unwrap();

        assert_eq!(report.sessions_processed, 1);
        assert_eq!(report.events_backfilled, 1); // dup dropped
        let body = std::fs::read_to_string(&events_path).unwrap();
        assert!(body.contains("A brand new finding."));
        assert!(body.contains("\"source\":\"dream\""));
        assert!(!body.contains("\"text\":\"Already known fact.\",\"refs\""));
    }

    #[test]
    fn run_dream_does_not_repeat_an_event_across_sessions() {
        // Two sessions of one task both surface the same decision: the
        // second must not append it again.
        let d = TempDir::new().unwrap();
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();
        let events_path = d.path().join("events.jsonl");
        let backend = MockDreamBackend {
            events: vec![BackfillEvent {
                event_type: EventType::Decision,
                task_id: "tj-1".into(),
                text: "Chose SQLite over Postgres.".into(),
                timestamp: "2026-06-08T10:00:00Z".into(),
            }],
        };
        let opts = DreamOptions {
            project_hash: "ph".into(),
            dry_run: false,
        };
        let mut second = task_input();
        second.0 = "sess-2".into();

        let report = run_dream(
            &conn,
            &events_path,
            &opts,
            &backend,
            vec![task_input(), second],
            "run-1",
        )
        .unwrap();

        assert_eq!(report.sessions_processed, 2);
        assert_eq!(report.events_backfilled, 1);
    }

    #[test]
    fn dry_run_writes_nothing_and_skips_backend() {
        let d = TempDir::new().unwrap();
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();
        let events_path = d.path().join("events.jsonl");
        let backend = MockDreamBackend { events: vec![] };
        let opts = DreamOptions {
            project_hash: "ph".into(),
            dry_run: true,
        };
        let report = run_dream(
            &conn,
            &events_path,
            &opts,
            &backend,
            vec![task_input()],
            "run-1",
        )
        .unwrap();
        assert_eq!(report.sessions_processed, 1);
        assert_eq!(report.events_backfilled, 0);
        assert!(!events_path.exists());
    }
}
