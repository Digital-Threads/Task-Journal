//! Anthropic API HTTP client implementing DreamBackend. Mirrors
//! classifier::http but returns a list of missed events.

use crate::dream::backend::{BackfillEvent, BackfillInput, DreamBackend};
use crate::dream::prompt::build_prompt;
use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Stronger than the classifier default — dream sees the whole transcript.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
pub const DEFAULT_MAX_TOKENS: u32 = 2048;

pub struct AnthropicDreamBackend {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub max_tokens: u32,
    pub timeout: Duration,
}

impl AnthropicDreamBackend {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key =
            std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY env var not set")?;
        let model = std::env::var("TJ_DREAM_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        let max_tokens = std::env::var("TJ_DREAM_MAX_TOKENS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MAX_TOKENS);
        Ok(Self {
            api_key,
            model,
            base_url: "https://api.anthropic.com".into(),
            max_tokens,
            timeout: DEFAULT_TIMEOUT,
        })
    }
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

impl DreamBackend for AnthropicDreamBackend {
    fn backfill(&self, input: &BackfillInput) -> anyhow::Result<Vec<BackfillEvent>> {
        let prompt = build_prompt(input);
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
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

        let json_str = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        let out: Vec<BackfillEvent> = serde_json::from_str(json_str)
            .with_context(|| format!("dream JSON parse failed; got: {json_str}"))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dream::backend::BackfillInput;

    #[test]
    fn backend_parses_event_array() {
        let mut server = mockito::Server::new();
        let url = server.url();
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "[{\"event_type\":\"finding\",\"task_id\":\"tj-2\",\"text\":\"Found the bug.\",\"timestamp\":\"2026-06-08T10:00:00Z\"}]" }
            ]
        });
        let _m = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create();

        let be = AnthropicDreamBackend {
            api_key: "k".into(),
            model: "m".into(),
            base_url: url,
            max_tokens: 256,
            timeout: Duration::from_secs(5),
        };
        let input = BackfillInput {
            tasks: vec![],
            transcript: "t".into(),
        };
        let out = be.backfill(&input).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Found the bug.");
        assert_eq!(out[0].task_id, "tj-2");
    }
}
