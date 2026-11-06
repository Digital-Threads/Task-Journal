//! Event classifier: takes a chat chunk + recent task context,
//! returns suggested event_type + task_id + confidence.

use crate::event::{EventType, EvidenceStrength};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ClassifyInput {
    pub text: String,
    pub author_hint: String,
    pub recent_tasks: Vec<TaskContext>,
    /// The chunk is a tool call's input + output (PostToolUse). The
    /// heuristic stage then only trusts test results; the LLM prompt is
    /// unchanged.
    #[serde(skip)]
    pub tool_output: bool,
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

/// The longest a rejection can be: a sentence or two on what was ruled out
/// and why. A whole answer that happens to say "rejected" is not one.
pub const REJECTION_MAX_CHARS: usize = 600;

/// Phrases that rule an approach out on their own.
const REJECTION_CUES: &[&str] = &[
    "decided against",
    "won't work because",
    "ruled out",
    "rule it out",
    "won't use",
    "will not use",
    "not going to use",
    "not going with",
    "gave up on",
    "giving up on",
    "abandoned the",
    "abandon the",
    "rejected the",
    "rejecting the",
    "отказались от",
    "отказался от",
    "отказываемся от",
    "отказ от",
    "отклонил",
    "отклонён",
    "отклонен",
    "отклоняем",
    "отверг",
    "не будем использовать",
    "не стали использовать",
];

/// An attempt and its failure together: "tried X, it didn't work".
const TRIED: &[&str] = &["tried", "attempted", "пробовал", "попробовал"];
const FAILED: &[&str] = &[
    "instead",
    "doesn't work",
    "didn't work",
    "won't work",
    "failed",
    "вместо",
    "не работает",
    "не сработал",
    "не подходит",
];

/// True when `text` rules an approach out: a short statement with an
/// explicit cue. A bug report ("X doesn't work"), a log line ("rejected 2
/// files"), a tool call or a whole answer is not a rejection.
pub fn is_explicit_rejection(text: &str) -> bool {
    if text.chars().count() > REJECTION_MAX_CHARS || crate::noise::machine_text(text).is_some() {
        return false;
    }

    let lower = text.to_lowercase();
    let has = |cues: &[&str]| cues.iter().any(|c| lower.contains(c));

    has(REJECTION_CUES) || (has(TRIED) && has(FAILED))
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
        let _env = crate::test_env_lock();
        let prev_model = std::env::var("TJ_CLASSIFIER_MODEL").ok();
        let prev_key = std::env::var("ANTHROPIC_API_KEY").ok();

        // SAFETY: the shared env lock serializes the crate's env tests.
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
            tool_output: false,
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
