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

/// Max transcript characters fed to the model in one call. The hard wall is
/// the ~200k-token context limit (a real session hit ~220k tokens and `claude
/// -p` returned HTTP 400). We stay well under it and split oversized
/// transcripts across several calls, merging the events (run_dream dedups).
const TRANSCRIPT_CHAR_BUDGET: usize = 360_000;

impl DreamBackend for LlmDreamBackend {
    fn backfill(&self, input: &BackfillInput) -> anyhow::Result<Vec<BackfillEvent>> {
        let mut out = Vec::new();
        for chunk in chunk_transcript(&input.transcript, TRANSCRIPT_CHAR_BUDGET) {
            let chunk_input = BackfillInput {
                tasks: input.tasks.clone(),
                transcript: chunk,
            };
            let prompt = crate::dream::prompt::build_prompt(&chunk_input);
            let text = self.llm.complete(&prompt, 1024)?;
            // Backfill is best-effort: a model that replied with prose instead
            // of the JSON array (e.g. continued the transcript dialogue) yields
            // nothing for this chunk, but must NOT abort the whole finalize —
            // the retitle/close still need to run.
            match parse_backfill_json(&text) {
                Ok(evs) => out.extend(evs),
                Err(e) => {
                    tracing::warn!(error = %e, "dream backfill: skipping unparseable chunk reply")
                }
            }
        }
        Ok(out)
    }
}

/// Split a transcript into chunks of at most `budget` bytes, breaking on line
/// boundaries where possible (a lone oversized line is hard-split on char
/// boundaries). Always returns at least one chunk so an empty transcript still
/// yields a single call.
fn chunk_transcript(transcript: &str, budget: usize) -> Vec<String> {
    if transcript.len() <= budget {
        return vec![transcript.to_string()];
    }
    let mut chunks = Vec::new();
    let mut cur = String::new();
    for line in transcript.split_inclusive('\n') {
        if !cur.is_empty() && cur.len() + line.len() > budget {
            chunks.push(std::mem::take(&mut cur));
        }
        if line.len() > budget {
            for ch in line.chars() {
                if !cur.is_empty() && cur.len() + ch.len_utf8() > budget {
                    chunks.push(std::mem::take(&mut cur));
                }
                cur.push(ch);
            }
        } else {
            cur.push_str(line);
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
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
    // Tolerate a JSON array wrapped in prose by slicing to the outer brackets.
    let slice = match (json_str.find('['), json_str.rfind(']')) {
        (Some(a), Some(b)) if b > a => &json_str[a..=b],
        _ => json_str,
    };
    serde_json::from_str(slice).with_context(|| format!("dream JSON parse failed; got: {json_str}"))
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
    fn parse_extracts_array_wrapped_in_prose() {
        let reply = "Here are the missed events:\n[{\"event_type\":\"finding\",\
\"task_id\":\"tj-1\",\"text\":\"found\",\"timestamp\":\"2026-06-13T00:00:00Z\"}]\nHope that helps!";
        let evs = parse_backfill_json(reply).unwrap();
        assert_eq!(evs.len(), 1);
    }

    #[test]
    fn parse_errors_on_pure_prose() {
        // A conversational reply with no array at all must be an Err so the
        // backfill loop can skip the chunk instead of inventing events.
        assert!(parse_backfill_json("Контекст в норме. Что дальше?").is_err());
    }

    #[test]
    fn backfill_skips_unparseable_chunk_reply() {
        // Model replies with prose, not JSON → backfill yields nothing but does
        // NOT error, so the surrounding finalize (retitle/close) still runs.
        struct ChattyLlm;
        impl LlmBackend for ChattyLlm {
            fn complete(&self, _prompt: &str, _max: u32) -> anyhow::Result<String> {
                Ok("Контекст в норме. 566.5k/1M использовано. Что дальше?".to_string())
            }
            fn name(&self) -> &'static str {
                "chatty"
            }
        }
        let b = LlmDreamBackend::new(Box::new(ChattyLlm));
        let input = BackfillInput {
            tasks: vec![],
            transcript: "user: hi\nassistant: hello".into(),
        };
        let evs = b.backfill(&input).unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn small_transcript_is_one_chunk() {
        let c = chunk_transcript("a\nb\nc\n", 100);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0], "a\nb\nc\n");
    }

    #[test]
    fn big_transcript_splits_on_lines_and_preserves_content() {
        // 10 lines of 20 chars; budget 50 → multiple chunks, no loss.
        let transcript: String = (0..10).map(|i| format!("line{i:015}\n")).collect();
        let chunks = chunk_transcript(&transcript, 50);
        assert!(chunks.len() > 1, "must split");
        assert!(chunks.iter().all(|c| c.len() <= 50));
        assert_eq!(chunks.concat(), transcript, "no content lost");
    }

    #[test]
    fn oversized_single_line_is_hard_split() {
        let line = "x".repeat(250);
        let chunks = chunk_transcript(&line, 100);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.len() <= 100));
        assert_eq!(chunks.concat(), line);
    }

    #[test]
    fn backfill_chunks_large_transcript_into_multiple_calls() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CountingLlm(AtomicUsize);
        impl LlmBackend for CountingLlm {
            fn complete(&self, _prompt: &str, _max: u32) -> anyhow::Result<String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok("[]".to_string())
            }
            fn name(&self) -> &'static str {
                "counting"
            }
        }
        let llm = Box::new(CountingLlm(AtomicUsize::new(0)));
        // Build a transcript larger than the budget so it must split.
        let transcript = "y\n".repeat(TRANSCRIPT_CHAR_BUDGET);
        let b = LlmDreamBackend::new(llm);
        let input = BackfillInput {
            tasks: vec![],
            transcript,
        };
        let evs = b.backfill(&input).unwrap();
        assert!(evs.is_empty());
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
