//! Hybrid classifier — heuristic-first, LLM fallback chain.
//!
//! Tries the cheap, zero-network heuristic first. If a rule fires with
//! confidence >= `min_heuristic_confidence`, returns the heuristic verdict.
//! Otherwise it walks an ordered chain of LLM backends and returns the first
//! that succeeds:
//!
//!   heuristic (>= 0.7)  →  agent-sdk (local `claude` login)  →  api (key)  →  bail
//!
//! The order is configurable via `TJ_HYBRID_LLM_ORDER` (default
//! `"agent-sdk,api"`); set it to `"api,agent-sdk"` to prefer the API key when
//! both are available. Only *available* backends join the chain (agent-sdk
//! needs `claude` on PATH; api needs `ANTHROPIC_API_KEY`). When the chain is
//! empty and the heuristic is uncertain, the classifier errors out and the
//! caller drops the chunk into the pending queue for later retry.
//!
//! The `agent-sdk` backend resurrects the v0.7.x `claude -p` path that was
//! removed in v0.8.0 — see [`super::agent_sdk`] for the honest note on the
//! post-2026-06-15 Agent SDK credit pool.

use super::agent_sdk::ClaudeCliClassifier;
use super::heuristic::try_heuristic;
#[cfg(test)]
use super::http::AnthropicClassifier;
use super::{Classifier, ClassifyInput, ClassifyOutput};

/// Confidence the heuristic must reach to skip the LLM fallback. Below
/// this, the chunk is ambiguous enough that the LLM call is worth the cost.
const DEFAULT_MIN_HEURISTIC_CONFIDENCE: f64 = 0.7;

/// Default fallback order when `TJ_HYBRID_LLM_ORDER` is unset: prefer the
/// subscription-native agent-sdk backend over the paid API key.
const DEFAULT_LLM_ORDER: &str = "agent-sdk,api";

pub struct HybridClassifier {
    /// Ordered LLM fallbacks, tried after the heuristic is uncertain. The
    /// first to return `Ok` wins. Empty = heuristic-only (uncertain → bail).
    llm_chain: Vec<Box<dyn Classifier>>,
    min_heuristic_confidence: f64,
}

impl HybridClassifier {
    /// Build from environment. The LLM chain is assembled from
    /// `TJ_HYBRID_LLM_ORDER` (default `agent-sdk,api`), including only the
    /// backends that are actually available right now.
    pub fn from_env() -> Self {
        let order =
            std::env::var("TJ_HYBRID_LLM_ORDER").unwrap_or_else(|_| DEFAULT_LLM_ORDER.into());
        let mut llm_chain: Vec<Box<dyn Classifier>> = Vec::new();
        for kind in order.split(',').map(str::trim) {
            match kind {
                "agent-sdk" => {
                    if let Some(c) = ClaudeCliClassifier::from_env() {
                        llm_chain.push(Box::new(c));
                    }
                }
                "api" => {
                    if let Ok(c) = super::http::AnthropicClassifier::from_env() {
                        llm_chain.push(Box::new(c));
                    }
                }
                _ => {} // unknown token: ignore rather than fail the hook
            }
        }
        Self {
            llm_chain,
            min_heuristic_confidence: DEFAULT_MIN_HEURISTIC_CONFIDENCE,
        }
    }

    /// Test-only constructor — accepts an explicit HTTP backend
    /// (e.g. one pointed at a mock server) without touching env vars.
    #[cfg(test)]
    pub fn with_http(http: Option<AnthropicClassifier>, min_conf: f64) -> Self {
        let llm_chain: Vec<Box<dyn Classifier>> = match http {
            Some(h) => vec![Box::new(h)],
            None => vec![],
        };
        Self {
            llm_chain,
            min_heuristic_confidence: min_conf,
        }
    }

    /// Test-only constructor — supply the LLM fallback chain directly (e.g. an
    /// agent-sdk classifier backed by a fake runner, followed by a panicking
    /// double to prove it is never reached).
    #[cfg(test)]
    pub fn with_llm_chain(llm_chain: Vec<Box<dyn Classifier>>, min_conf: f64) -> Self {
        Self {
            llm_chain,
            min_heuristic_confidence: min_conf,
        }
    }

    pub fn has_llm_fallback(&self) -> bool {
        !self.llm_chain.is_empty()
    }
}

impl Classifier for HybridClassifier {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
        if let Some(out) = try_heuristic(input) {
            if out.confidence >= self.min_heuristic_confidence {
                return Ok(out);
            }
        }
        if self.llm_chain.is_empty() {
            anyhow::bail!(
                "hybrid: heuristic uncertain and no LLM backend available \
                 (no `claude` on PATH for agent-sdk, no ANTHROPIC_API_KEY for api) — \
                 chunk left in pending queue for later retry"
            );
        }
        let mut last_err = None;
        for backend in &self.llm_chain {
            match backend.classify(input) {
                Ok(out) => return Ok(out),
                Err(e) => last_err = Some(e),
            }
        }
        // The chain is non-empty, so at least one backend ran and errored.
        Err(last_err.expect("non-empty chain must produce an error on full failure"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::agent_sdk::{ClaudeCliClassifier, CommandRunner};
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
                constraints: vec![],
            }],
        }
    }

    #[test]
    fn heuristic_hit_skips_http_even_when_available() {
        // Heuristic catches the decision phrase, so the (empty) chain never runs.
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
        let prev_key = std::env::var("ANTHROPIC_API_KEY").ok();
        // Force heuristic-only by disabling both LLM backends via an order that
        // names no real one, so this stays deterministic regardless of whether
        // a `claude` binary happens to be on the test machine's PATH.
        let prev_order = std::env::var("TJ_HYBRID_LLM_ORDER").ok();
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("TJ_HYBRID_LLM_ORDER", "none");
        }
        let hybrid = HybridClassifier::from_env();
        assert!(!hybrid.has_llm_fallback());
        unsafe {
            match prev_key {
                Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
                None => std::env::remove_var("ANTHROPIC_API_KEY"),
            }
            match prev_order {
                Some(v) => std::env::set_var("TJ_HYBRID_LLM_ORDER", v),
                None => std::env::remove_var("TJ_HYBRID_LLM_ORDER"),
            }
        }
    }

    #[test]
    fn uncertain_heuristic_prefers_agent_sdk_and_never_touches_http() {
        // agent-sdk (backed by a fake runner) returns Ok first; the http double
        // panics if reached — proving the chain stops at the first success.
        struct OkRunner;
        impl CommandRunner for OkRunner {
            fn run(&self, _model: &str, _prompt: &str) -> anyhow::Result<String> {
                Ok(serde_json::json!({
                    "type": "result",
                    "is_error": false,
                    "result": r#"{"event_type":"decision","task_id_guess":null,"confidence":0.9,"evidence_strength":null,"suggested_text":"Adopt X."}"#,
                })
                .to_string())
            }
        }
        struct PanicBackend;
        impl Classifier for PanicBackend {
            fn classify(&self, _input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
                panic!("http backend must not be reached when agent-sdk succeeds");
            }
        }

        let agent = ClaudeCliClassifier::with_runner("claude-haiku-4-5", Box::new(OkRunner));
        let hybrid =
            HybridClassifier::with_llm_chain(vec![Box::new(agent), Box::new(PanicBackend)], 0.7);
        let out = hybrid
            .classify(&ctx(
                "Browsing the call site of refundProcessor to understand the dispatch.",
            ))
            .unwrap();
        assert_eq!(out.event_type, EventType::Decision);
    }
}
