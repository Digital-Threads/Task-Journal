//! Event classifier: takes a chat chunk + recent task context,
//! returns suggested event_type + task_id + confidence.

use crate::event::{EventType, EvidenceStrength};
use serde::{Deserialize, Serialize};

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
    /// The task's most-recent `constraint` events (≤ N). Empty when the
    /// task has no constraints — the prompt is then unchanged.
    pub constraints: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClassifyOutput {
    pub event_type: EventType,
    pub task_id_guess: Option<String>,
    pub confidence: f64,
    pub evidence_strength: Option<EvidenceStrength>,
    pub suggested_text: String,
    /// v0.6.0: optional structured artifacts the classifier extracted
    /// directly. When absent (old protocol or model didn't bother),
    /// the journal falls back to regex extraction in
    /// `db::ingest_new_events`. When present, the two sets are merged
    /// at ingest time so the model can surface artifacts the regex
    /// would miss (e.g. ticket ids in non-ASCII brackets).
    #[serde(default)]
    pub artifacts: Option<crate::artifacts::Artifacts>,
}

pub trait Classifier: Send + Sync {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput>;
}

use crate::event::EventStatus;

pub const CONFIDENCE_THRESHOLD: f64 = 0.85;

pub fn decide_status(confidence: f64) -> EventStatus {
    if confidence >= CONFIDENCE_THRESHOLD {
        EventStatus::Confirmed
    } else {
        EventStatus::Suggested
    }
}

/// Parse a model's raw text reply into a strict-JSON `ClassifyOutput`,
/// tolerating ```json code-fence wrapping. Shared by the HTTP and agent-sdk
/// backends so the two never diverge on how they read the verdict.
pub(crate) fn parse_verdict(text: &str) -> anyhow::Result<ClassifyOutput> {
    use anyhow::Context;
    let json_str = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(json_str)
        .with_context(|| format!("classifier JSON parse failed; got: {json_str}"))
}

pub mod agent_sdk;
pub mod heuristic;
pub mod http;
pub mod hybrid;
pub mod mock;
pub mod prompt;
pub mod telemetry;

#[cfg(test)]
mod tests {
    use super::*;

    /// The HTTP backend must honour `TJ_CLASSIFIER_MODEL`. Wraps the
    /// read-set-restore steps in one test to avoid env-var races with
    /// other tests in this crate.
    #[test]
    fn tj_classifier_model_env_var_overrides_http_default() {
        let prev_model = std::env::var("TJ_CLASSIFIER_MODEL").ok();
        let prev_key = std::env::var("ANTHROPIC_API_KEY").ok();

        // SAFETY: tests in this crate do not concurrently read these env vars.
        unsafe {
            std::env::remove_var("TJ_CLASSIFIER_MODEL");
            std::env::set_var("ANTHROPIC_API_KEY", "test-key-do-not-use");
        }
        let http_default = http::AnthropicClassifier::from_env().unwrap();
        assert_eq!(http_default.model, http::DEFAULT_MODEL);

        unsafe {
            std::env::set_var("TJ_CLASSIFIER_MODEL", "sonnet-override");
        }
        let http_override = http::AnthropicClassifier::from_env().unwrap();
        assert_eq!(http_override.model, "sonnet-override");

        // Restore.
        unsafe {
            match prev_model {
                Some(v) => std::env::set_var("TJ_CLASSIFIER_MODEL", v),
                None => std::env::remove_var("TJ_CLASSIFIER_MODEL"),
            }
            match prev_key {
                Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
                None => std::env::remove_var("ANTHROPIC_API_KEY"),
            }
        }
    }

    #[test]
    fn task_context_has_constraints_field() {
        let c = TaskContext {
            task_id: "tj-1".into(),
            title: "t".into(),
            last_events: vec![],
            constraints: vec!["must support PHP 7.4".into()],
        };
        assert_eq!(c.constraints, vec!["must support PHP 7.4".to_string()]);
    }

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

    #[test]
    fn decide_status_high_confidence_is_confirmed() {
        assert_eq!(decide_status(0.95), EventStatus::Confirmed);
        assert_eq!(decide_status(0.85), EventStatus::Confirmed);
    }

    #[test]
    fn decide_status_low_confidence_is_suggested() {
        assert_eq!(decide_status(0.84), EventStatus::Suggested);
        assert_eq!(decide_status(0.0), EventStatus::Suggested);
    }
}
