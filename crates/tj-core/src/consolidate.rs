//! Memory consolidation (Pillar C): distil a project's recurring decisions and
//! constraints into a handful of durable semantic/procedural facts with a
//! single LLM call.
//!
//! Two backends, picked by [`summarize`]: the **direct Anthropic Haiku API**
//! when `ANTHROPIC_API_KEY` is set (cheapest — only our ~7k-token prompt,
//! ~1c/run), otherwise the local **`claude -p`** binary (subscription auth, no
//! API key needed, but it boots the whole environment per call so it's
//! pricier). With neither, the caller skips cleanly — we never fall back to a
//! heuristic, which would manufacture low-trust "facts".
//!
//! Either way this is a MANUAL command: one call per run, only when the user
//! asks, never wired to a hook — so it never resembles the per-prompt
//! classifier burn.

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Cheapest capable model for the summarisation step.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

/// A distilled fact and which tier it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidatedFact {
    /// "semantic" (a durable truth about the system) or "procedural" (how the
    /// team works).
    pub tier: String,
    pub text: String,
}

/// Direct-API consolidator.
pub struct Consolidator {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub timeout: Duration,
    pub max_facts: usize,
}

impl Consolidator {
    /// Build from the environment. Errors (so the caller can skip cleanly) when
    /// `ANTHROPIC_API_KEY` is absent. Model overridable via `TJ_CONSOLIDATE_MODEL`.
    pub fn from_env(max_facts: usize) -> anyhow::Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            anyhow!("consolidation needs ANTHROPIC_API_KEY for the direct Haiku API")
        })?;
        let model = std::env::var("TJ_CONSOLIDATE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        // TJ_CONSOLIDATE_BASE_URL overrides the endpoint (used by tests to point
        // at a local mock); production always hits the real Anthropic API.
        let base_url = std::env::var("TJ_CONSOLIDATE_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".into());
        Ok(Self {
            api_key,
            model,
            base_url,
            timeout: Duration::from_secs(60),
            max_facts: max_facts.max(1),
        })
    }

    /// Summarise the given event texts into durable facts. Empty input → no
    /// call. Returns whatever facts the model produced (possibly none).
    pub fn consolidate(&self, events: &[String]) -> anyhow::Result<Vec<ConsolidatedFact>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let prompt = build_prompt(events, self.max_facts);
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: 512,
            messages: vec![MessageIn {
                role: "user",
                content: &prompt,
            }],
        };
        let url = format!("{}/v1/messages", self.base_url);
        let resp: MessagesResponse = ureq::post(&url)
            .timeout(self.timeout)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .set("content-type", "application/json")
            .send_json(serde_json::to_value(&body)?)
            .context("Anthropic API request failed")?
            .into_json()
            .context("decode Anthropic response")?;
        let text = resp
            .content
            .iter()
            .find(|b| b.kind == "text")
            .map(|b| b.text.clone())
            .ok_or_else(|| anyhow!("no text content in response"))?;
        Ok(parse_facts(&text))
    }
}

/// Run whichever summarisation backend is available and return its label plus
/// the facts it produced. Order: (1) `ANTHROPIC_API_KEY` set → direct Haiku API
/// (cheapest, ~1c/run); (2) else `claude` on PATH → local `claude -p`
/// (subscription auth, no API key, heavier per-call boot); (3) else `Ok(None)`,
/// so the caller skips with a message — never a heuristic.
/// `TJ_CONSOLIDATE_BACKEND=none` forces the no-backend path (disable / tests).
pub fn summarize(
    events: &[String],
    max_facts: usize,
) -> anyhow::Result<Option<(&'static str, Vec<ConsolidatedFact>)>> {
    if std::env::var("TJ_CONSOLIDATE_BACKEND").as_deref() == Ok("none") {
        return Ok(None);
    }
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        let c = Consolidator::from_env(max_facts)?;
        return Ok(Some(("haiku-api", c.consolidate(events)?)));
    }
    if crate::classifier::agent_sdk::claude_on_path() {
        return Ok(Some(("claude -p", consolidate_via_cli(events, max_facts)?)));
    }
    Ok(None)
}

/// Summarise via the local `claude -p` binary (subscription auth). Reuses the
/// classifier's command plumbing — including the recursion guard set by
/// `base_claude_command` — and unwraps the `--output-format json` envelope.
fn consolidate_via_cli(
    events: &[String],
    max_facts: usize,
) -> anyhow::Result<Vec<ConsolidatedFact>> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let prompt = build_prompt(events, max_facts);
    let model = std::env::var("TJ_CONSOLIDATE_MODEL")
        .unwrap_or_else(|_| crate::classifier::agent_sdk::DEFAULT_MODEL.to_string());
    let text = crate::classifier::agent_sdk::run_claude_json(
        &crate::classifier::agent_sdk::ClaudeBinaryStdinRunner,
        &model,
        &prompt,
    )?;
    Ok(parse_facts(&text))
}

/// The summarisation prompt. Deliberately strict: durable-only, fixed line
/// format, "output nothing" escape hatch so the model doesn't pad.
pub fn build_prompt(events: &[String], max_facts: usize) -> String {
    let joined = events
        .iter()
        .map(|e| format!("- {}", e.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are given decisions and constraints recorded across ONE software \
project. Distil them into at most {max_facts} DURABLE facts — stable \
conventions or architectural truths that hold across the project, not one-off \
details.\n\n\
Rules:\n\
- One fact per line.\n\
- Each line MUST start with `[semantic]` (a durable truth about the system) or \
`[procedural]` (how the team works).\n\
- Keep each fact to one short sentence.\n\
- If nothing is durable enough, output nothing at all.\n\n\
Decisions and constraints:\n{joined}"
    )
}

/// Parse the model reply into facts. Accepts lines like `[semantic] ...` or
/// `- [procedural] ...`; ignores anything else.
pub fn parse_facts(text: &str) -> Vec<ConsolidatedFact> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', ' ']).trim();
        for tier in ["semantic", "procedural"] {
            let tag = format!("[{tier}]");
            if let Some(rest) = line.strip_prefix(&tag) {
                let fact = rest.trim();
                if fact.chars().count() >= 6 {
                    out.push(ConsolidatedFact {
                        tier: tier.to_string(),
                        text: fact.to_string(),
                    });
                }
                break;
            }
        }
    }
    out
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<MessageIn<'a>>,
}
#[derive(Serialize)]
struct MessageIn<'a> {
    role: &'a str,
    content: &'a str,
}
#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}
#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_facts_extracts_tagged_lines() {
        let reply = "[semantic] Refunds route through the idempotent ledger\n\
                     - [procedural] PR into main, squash-merge\n\
                     some preamble that should be ignored\n\
                     [bogus] not a real tier";
        let facts = parse_facts(reply);
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].tier, "semantic");
        assert!(facts[0].text.contains("idempotent ledger"));
        assert_eq!(facts[1].tier, "procedural");
        assert!(facts[1].text.contains("squash-merge"));
    }

    #[test]
    fn parse_facts_empty_on_no_tagged_lines() {
        assert!(parse_facts("nothing durable here").is_empty());
        assert!(parse_facts("").is_empty());
    }

    #[test]
    fn build_prompt_includes_events_and_cap() {
        let p = build_prompt(&["chose ledger".into(), "PR into main".into()], 5);
        assert!(p.contains("at most 5"));
        assert!(p.contains("- chose ledger"));
        assert!(p.contains("- PR into main"));
        assert!(p.contains("[semantic]") && p.contains("[procedural]"));
    }

    #[test]
    fn consolidate_empty_input_makes_no_call() {
        // base_url is unreachable; empty input must short-circuit before any
        // request, so this must not error.
        let c = Consolidator {
            api_key: "x".into(),
            model: "m".into(),
            base_url: "http://127.0.0.1:1".into(),
            timeout: Duration::from_millis(50),
            max_facts: 5,
        };
        assert!(c.consolidate(&[]).unwrap().is_empty());
    }

    #[test]
    fn consolidate_calls_api_and_parses() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "msg",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "[semantic] Always use the ledger\n[procedural] TDD here"}]
                })
                .to_string(),
            )
            .create();

        let c = Consolidator {
            api_key: "test".into(),
            model: "claude-haiku-4-5-20251001".into(),
            base_url: server.url(),
            timeout: Duration::from_secs(5),
            max_facts: 5,
        };
        let facts = c.consolidate(&["chose ledger".into()]).unwrap();
        m.assert();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].tier, "semantic");
        assert_eq!(facts[1].tier, "procedural");
    }
}
