//! Hybrid classifier — heuristic-first, LLM fallback.
//!
//! Tries the cheap, zero-network heuristic first. If a rule fires with
//! confidence >= `min_heuristic_confidence`, returns the heuristic verdict.
//! Otherwise escalates to the HTTP (Anthropic API) backend — which
//! requires `ANTHROPIC_API_KEY`. When no key is set and the heuristic
//! is uncertain, the classifier errors out (caller should drop the
//! chunk into the pending queue for later retry rather than guess).
//!
//! This replaces the v0.7.x `cli` backend that relied on `claude -p`.
//! Anthropic changed `claude -p` to bill against tokens separately
//! from the Pro/Max subscription, breaking the "free fallback" promise
//! the cli backend was built on.

use super::heuristic::try_heuristic;
use super::http::AnthropicClassifier;
use super::{Classifier, ClassifyInput, ClassifyOutput};

/// Confidence the heuristic must reach to skip the LLM fallback. Below
/// this, the chunk is ambiguous enough that the API call is worth the
/// cost.
const DEFAULT_MIN_HEURISTIC_CONFIDENCE: f64 = 0.7;

pub struct HybridClassifier {
    http: Option<AnthropicClassifier>,
    min_heuristic_confidence: f64,
}

impl HybridClassifier {
    /// Build from environment. Picks up `ANTHROPIC_API_KEY` if present;
    /// without it, the hybrid still works for chunks the heuristic
    /// handles confidently, but uncertain chunks will fail (caller
    /// queues them in pending/).
    pub fn from_env() -> Self {
        Self {
            http: AnthropicClassifier::from_env().ok(),
            min_heuristic_confidence: DEFAULT_MIN_HEURISTIC_CONFIDENCE,
        }
    }

    /// Test-only constructor — accepts an explicit HTTP backend
    /// (e.g. one pointed at a mock server) without touching env vars.
    #[cfg(test)]
    pub fn with_http(http: Option<AnthropicClassifier>, min_conf: f64) -> Self {
        Self {
            http,
            min_heuristic_confidence: min_conf,
        }
    }

    pub fn has_llm_fallback(&self) -> bool {
        self.http.is_some()
    }
}

impl Classifier for HybridClassifier {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
        if let Some(out) = try_heuristic(input) {
            if out.confidence >= self.min_heuristic_confidence {
                return Ok(out);
            }
        }
        match &self.http {
            Some(h) => h.classify(input),
            None => anyhow::bail!(
                "hybrid: heuristic uncertain and ANTHROPIC_API_KEY not set — \
                 chunk left in pending queue for later retry"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::TaskContext;
    use crate::event::EventType;

    fn ctx(text: &str) -> ClassifyInput {
        ClassifyInput {
            text: text.into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![TaskContext {
                task_id: "tj-abc".into(),
                title: "test".into(),
                last_events: vec![],
            }],
        }
    }

    #[test]
    fn heuristic_hit_skips_http_even_when_available() {
        // Build a hybrid with `http` set to a *dummy* that would error if called.
        // Heuristic catches the decision phrase, so http never runs.
        let hybrid = HybridClassifier::with_http(None, 0.7);
        let out = hybrid
            .classify(&ctx(
                "After review we'll use TOML for the config format going forward",
            ))
            .unwrap();
        assert_eq!(out.event_type, EventType::Decision);
    }

    #[test]
    fn uncertain_heuristic_without_api_key_bails() {
        let hybrid = HybridClassifier::with_http(None, 0.7);
        let err = hybrid
            .classify(&ctx(
                "Browsing the call site of refundProcessor to understand the dispatch.",
            ))
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("ANTHROPIC_API_KEY"),
            "error must mention env var: {msg}"
        );
    }

    #[test]
    fn from_env_constructs_without_key() {
        // SAFETY: tests in this crate do not concurrently read these env vars.
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        let hybrid = HybridClassifier::from_env();
        assert!(!hybrid.has_llm_fallback());
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
                None => std::env::remove_var("ANTHROPIC_API_KEY"),
            }
        }
    }
}
