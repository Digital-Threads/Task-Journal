//! Backend abstraction for dream Pass A: given a task's existing events
//! and a full transcript, return the significant events that were missed.

use crate::event::EventType;
use serde::{Deserialize, Serialize};

/// One missed event the backend proposes appending.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BackfillEvent {
    pub event_type: EventType,
    /// Which task this belongs to (one of the input's candidate task ids).
    pub task_id: String,
    pub text: String,
    /// RFC3339 timestamp of the transcript turn this was inferred from,
    /// so the event sorts into its correct place in the chain.
    pub timestamp: String,
}

/// Input for one session's backfill call.
#[derive(Debug, Clone, Serialize)]
pub struct BackfillInput {
    /// Candidate task contexts active in this session.
    pub tasks: Vec<BackfillTaskContext>,
    /// The full session transcript, flattened to role-tagged turns.
    pub transcript: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackfillTaskContext {
    pub task_id: String,
    pub title: String,
    /// Text of events already captured for this task (for dedup context).
    pub existing_events: Vec<String>,
}

pub trait DreamBackend {
    /// Return the events the realtime classifier missed for this session.
    fn backfill(&self, input: &BackfillInput) -> anyhow::Result<Vec<BackfillEvent>>;
}

/// Test backend that returns a canned list, ignoring the input.
pub struct MockDreamBackend {
    pub events: Vec<BackfillEvent>,
}

impl DreamBackend for MockDreamBackend {
    fn backfill(&self, _input: &BackfillInput) -> anyhow::Result<Vec<BackfillEvent>> {
        Ok(self.events.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_backend_returns_canned_events() {
        let be = MockDreamBackend {
            events: vec![BackfillEvent {
                event_type: EventType::Decision,
                task_id: "tj-1".into(),
                text: "Chose A over B.".into(),
                timestamp: "2026-06-08T10:00:00Z".into(),
            }],
        };
        let input = BackfillInput {
            tasks: vec![],
            transcript: "x".into(),
        };
        let out = be.backfill(&input).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].task_id, "tj-1");
        assert_eq!(out[0].event_type, EventType::Decision);
    }
}
