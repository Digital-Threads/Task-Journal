//! Dream backfill over the unified pluggable [`crate::llm`] backend, so dream
//! gets the same provider choice as everything else (claude-p default,
//! Anthropic, OpenAI/Codex, free local Ollama) instead of its own bespoke
//! clients.

use anyhow::Context;

use crate::dream::backend::{BackfillEvent, BackfillInput, DreamBackend};
use crate::llm::LlmBackend;

/// Adapts any [`LlmBackend`] into a [`DreamBackend`]: build the dream prompt,
/// run one completion, parse the JSON array of missed events.
pub struct LlmDreamBackend {
    llm: Box<dyn LlmBackend>,
}

impl LlmDreamBackend {
    pub fn new(llm: Box<dyn LlmBackend>) -> Self {
        Self { llm }
    }

    pub fn backend_name(&self) -> &'static str {
        self.llm.name()
    }
}

impl DreamBackend for LlmDreamBackend {
    fn backfill(&self, input: &BackfillInput) -> anyhow::Result<Vec<BackfillEvent>> {
        let prompt = crate::dream::prompt::build_prompt(input);
        let text = self.llm.complete(&prompt, 1024)?;
        parse_backfill_json(&text)
    }
}

/// Parse the model's reply (a JSON array of `BackfillEvent`, possibly wrapped in
/// a ```json fence) into events.
pub fn parse_backfill_json(text: &str) -> anyhow::Result<Vec<BackfillEvent>> {
    let json_str = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(json_str)
        .with_context(|| format!("dream JSON parse failed; got: {json_str}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    #[test]
    fn parse_strips_fence_and_decodes() {
        let reply = "```json\n[{\"event_type\":\"decision\",\"task_id\":\"tj-1\",\
\"text\":\"chose X\",\"timestamp\":\"2026-06-13T00:00:00Z\"}]\n```";
        let evs = parse_backfill_json(reply).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, EventType::Decision);
        assert_eq!(evs[0].task_id, "tj-1");
        assert!(evs[0].text.contains("chose X"));
    }

    #[test]
    fn parse_empty_array() {
        assert!(parse_backfill_json("[]").unwrap().is_empty());
    }

    #[test]
    fn llm_dream_backend_runs_and_parses() {
        struct FakeLlm;
        impl LlmBackend for FakeLlm {
            fn complete(&self, _prompt: &str, _max: u32) -> anyhow::Result<String> {
                Ok(
                    "[{\"event_type\":\"finding\",\"task_id\":\"tj-x\",\"text\":\"found it\",\
\"timestamp\":\"2026-06-13T00:00:00Z\"}]"
                        .to_string(),
                )
            }
            fn name(&self) -> &'static str {
                "fake"
            }
        }
        let b = LlmDreamBackend::new(Box::new(FakeLlm));
        let input = BackfillInput {
            tasks: vec![],
            transcript: "x".into(),
        };
        let evs = b.backfill(&input).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].text, "found it");
        assert_eq!(b.backend_name(), "fake");
    }
}
