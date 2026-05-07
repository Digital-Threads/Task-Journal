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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

use crate::event::EventStatus;

pub const CONFIDENCE_THRESHOLD: f64 = 0.85;

pub fn decide_status(confidence: f64) -> EventStatus {
    if confidence >= CONFIDENCE_THRESHOLD {
        EventStatus::Confirmed
    } else {
        EventStatus::Suggested
    }
}

pub mod cli;
pub mod http;
pub mod mock;
pub mod prompt;
pub mod telemetry;

#[cfg(test)]
mod tests {
    use super::*;

    /// Both classifiers must honour `TJ_CLASSIFIER_MODEL`. Combined into a
    /// single test to avoid env-var races with other tests in this crate;
    /// inside the test we serialize the read-set-restore steps.
    #[test]
    fn tj_classifier_model_env_var_overrides_defaults_for_both_backends() {
        let prev_model = std::env::var("TJ_CLASSIFIER_MODEL").ok();
        let prev_key = std::env::var("ANTHROPIC_API_KEY").ok();

        // Unset → defaults.
        // SAFETY: tests in this crate do not concurrently read these env vars.
        unsafe {
            std::env::remove_var("TJ_CLASSIFIER_MODEL");
        }

        let cli_default = cli::ClaudeCliClassifier::default();
        assert_eq!(cli_default.model, cli::DEFAULT_MODEL);

        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "test-key-do-not-use");
        }
        let http_default = http::AnthropicClassifier::from_env().unwrap();
        assert_eq!(http_default.model, http::DEFAULT_MODEL);

        // Set → override applied to both.
        unsafe {
            std::env::set_var("TJ_CLASSIFIER_MODEL", "sonnet-override");
        }
        let cli_override = cli::ClaudeCliClassifier::default();
        assert_eq!(cli_override.model, "sonnet-override");

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
