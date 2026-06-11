//! Capture completeness: deterministic, read-only detection of structural
//! gaps in a task's captured history. Measure + flag only — no mutation.

use rusqlite::Connection;

#[derive(Debug, Clone, PartialEq)]
pub enum GapKind {
    ClosedNoOutcome,
    DecisionNoEvidence,
    SuggestedUnconfirmed,
    NoGoal,
    PendingLeak,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gap {
    pub kind: GapKind,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompletenessReport {
    pub gaps: Vec<Gap>,
}

impl CompletenessReport {
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }
}

/// Assess a task's captured history for structural gaps. Deterministic and
/// read-only. `pending_count` (project-level unprocessed entries) is injected
/// so this fn stays filesystem-free and unit-testable.
pub fn assess(
    conn: &Connection,
    task_id: &str,
    pending_count: usize,
) -> anyhow::Result<CompletenessReport> {
    let mut gaps = Vec::new();

    // Metadata rules: read status/goal/outcome from the tasks row.
    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT status, goal, outcome FROM tasks WHERE task_id = ?1",
            rusqlite::params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    let Some((status, goal, outcome)) = row else {
        // Unknown task → empty report (no panic).
        return Ok(CompletenessReport { gaps });
    };

    if goal.as_deref().unwrap_or("").is_empty() {
        gaps.push(Gap {
            kind: GapKind::NoGoal,
            detail: "no goal recorded".to_string(),
        });
    }
    if status == "closed"
        && !goal.as_deref().unwrap_or("").is_empty()
        && outcome.as_deref().unwrap_or("").is_empty()
    {
        gaps.push(Gap {
            kind: GapKind::ClosedNoOutcome,
            detail: "closed without a recorded outcome".to_string(),
        });
    }

    // (event rules added in Task 2; pending rule in Task 3)
    let _ = pending_count;

    Ok(CompletenessReport { gaps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Author, Event, EventType, Source};
    use tempfile::TempDir;

    fn conn() -> (TempDir, Connection) {
        let d = TempDir::new().unwrap();
        let c = crate::db::open(d.path().join("s.sqlite")).unwrap();
        (d, c)
    }

    fn open_task(c: &Connection, id: &str) {
        let e = Event::new(id, EventType::Open, Author::User, Source::Cli, id.into());
        crate::db::upsert_task_from_event(c, &e, "ph").unwrap();
    }

    #[test]
    fn no_goal_fires_when_goal_absent() {
        let (_d, c) = conn();
        open_task(&c, "t1");
        let r = assess(&c, "t1", 0).unwrap();
        assert!(r.gaps.iter().any(|g| g.kind == GapKind::NoGoal));
    }

    #[test]
    fn closed_no_outcome_fires() {
        let (_d, c) = conn();
        open_task(&c, "t2");
        // Set a goal, then close without outcome.
        c.execute("UPDATE tasks SET goal='ship X' WHERE task_id='t2'", []).unwrap();
        c.execute("UPDATE tasks SET status='closed' WHERE task_id='t2'", []).unwrap();
        let r = assess(&c, "t2", 0).unwrap();
        assert!(r.gaps.iter().any(|g| g.kind == GapKind::ClosedNoOutcome));
        assert!(!r.gaps.iter().any(|g| g.kind == GapKind::NoGoal));
    }

    #[test]
    fn unknown_task_is_empty_report() {
        let (_d, c) = conn();
        let r = assess(&c, "nope", 0).unwrap();
        assert!(r.is_complete());
    }
}
