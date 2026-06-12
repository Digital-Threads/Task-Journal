//! Claude CLI ("agent SDK") implementation of [`DreamBackend`].
//!
//! Runs the local, already-authenticated `claude` binary in print mode,
//! **pinned to Haiku**, so offline dream backfill works on the Claude
//! subscription with no `ANTHROPIC_API_KEY`. Mirrors
//! [`crate::dream::http::AnthropicDreamBackend`] but reuses the classifier's
//! [`crate::classifier::agent_sdk`] command plumbing.
//!
//! Unlike the realtime classifier, dream feeds a whole session transcript, so
//! the prompt can be large. It therefore uses [`ClaudeBinaryStdinRunner`]
//! (prompt on stdin) to avoid the per-argument size limit (`E2BIG`).
//!
//! Cost note: as with the classifier agent-sdk backend, since 2026-06-15 a
//! headless `claude -p` run draws from the separate Agent SDK monthly credit
//! pool. Haiku keeps each backfill call cheap.

use crate::classifier::agent_sdk::{claude_on_path, run_claude_json, ClaudeBinaryStdinRunner, CommandRunner};
use crate::dream::backend::{BackfillEvent, BackfillInput, DreamBackend};
use crate::dream::prompt::build_prompt;
use anyhow::Context;

/// Default dream model: Haiku — cheap and subscription-friendly. Override with
/// `TJ_DREAM_MODEL` (shared with the API backend's override).
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

pub struct ClaudeCliDreamBackend {
    model: String,
    runner: Box<dyn CommandRunner>,
}

impl ClaudeCliDreamBackend {
    /// Build from environment. Returns `None` unless a `claude` binary is on
    /// PATH, so the caller can fall back to the API backend. Model comes from
    /// `TJ_DREAM_MODEL`, else Haiku.
    pub fn from_env() -> Option<Self> {
        if !claude_on_path() {
            return None;
        }
        let model = std::env::var("TJ_DREAM_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        Some(Self {
            model,
            runner: Box::new(ClaudeBinaryStdinRunner),
        })
    }

    /// Test/dev constructor: inject a fake runner and explicit model so the
    /// parse path is exercised without a live `claude` login.
    pub fn with_runner(model: impl Into<String>, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            model: model.into(),
            runner,
        }
    }
}

impl DreamBackend for ClaudeCliDreamBackend {
    fn backfill(&self, input: &BackfillInput) -> anyhow::Result<Vec<BackfillEvent>> {
        let prompt = build_prompt(input);
        let verdict = run_claude_json(self.runner.as_ref(), &self.model, &prompt)?;
        // The model returns a JSON array of events, sometimes fenced.
        let json_str = verdict
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        serde_json::from_str(json_str)
            .with_context(|| format!("dream JSON parse failed; got: {json_str}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dream::backend::BackfillInput;
    use crate::event::EventType;

    /// Fake runner returning a canned `--output-format json` envelope whose
    /// `result` is the model's reply text.
    struct FakeRunner {
        result_text: String,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _model: &str, _prompt: &str) -> anyhow::Result<String> {
            Ok(serde_json::json!({
                "type": "result",
                "is_error": false,
                "result": self.result_text,
            })
            .to_string())
        }
    }

    fn input() -> BackfillInput {
        BackfillInput {
            tasks: vec![],
            transcript: "user: решили взять Postgres вместо Mongo\nassistant: ок".into(),
        }
    }

    #[test]
    fn parses_event_array_from_envelope() {
        let runner = FakeRunner {
            result_text: r#"[{"event_type":"decision","task_id":"tj-2","text":"Взяли Postgres вместо Mongo.","timestamp":"2026-06-08T10:00:00Z"}]"#
                .into(),
        };
        let be = ClaudeCliDreamBackend::with_runner("claude-haiku-4-5", Box::new(runner));
        let out = be.backfill(&input()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event_type, EventType::Decision);
        assert_eq!(out[0].task_id, "tj-2");
        assert_eq!(out[0].text, "Взяли Postgres вместо Mongo.");
    }

    #[test]
    fn tolerates_code_fence_wrapped_array() {
        let runner = FakeRunner {
            result_text: "```json\n[{\"event_type\":\"finding\",\"task_id\":\"tj-1\",\"text\":\"x\",\"timestamp\":\"2026-06-08T10:00:00Z\"}]\n```"
                .into(),
        };
        let be = ClaudeCliDreamBackend::with_runner("claude-haiku-4-5", Box::new(runner));
        let out = be.backfill(&input()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event_type, EventType::Finding);
    }

    #[test]
    fn empty_array_yields_no_events() {
        let runner = FakeRunner {
            result_text: "[]".into(),
        };
        let be = ClaudeCliDreamBackend::with_runner("claude-haiku-4-5", Box::new(runner));
        let out = be.backfill(&input()).unwrap();
        assert!(out.is_empty());
    }
}
