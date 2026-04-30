//! Event classifier: takes a chat chunk + recent task context,
//! returns suggested event_type + task_id + confidence.

use serde::{Deserialize, Serialize};
use crate::event::{EventType, EvidenceStrength};

#[derive(Debug, Clone, Serialize)]
pub struct ClassifyInput {
    pub text: String,
    pub author_hint: String,
    pub recent_tasks: Vec<TaskContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskContext {
    pub task_id: String,
    pub title: String,
    pub last_events: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassifyOutput {
    pub event_type: EventType,
    pub task_id_guess: Option<String>,
    pub confidence: f64,
    pub evidence_strength: Option<EvidenceStrength>,
    pub suggested_text: String,
}

pub trait Classifier: Send + Sync {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput>;
}

pub mod mock;
pub mod prompt;
pub mod http;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_input_serializes() {
        let i = ClassifyInput {
            text: "Adopted Rust for the journal".into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![],
        };
        let s = serde_json::to_string(&i).unwrap();
        assert!(s.contains("Adopted Rust"));
    }
}
