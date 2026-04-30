//! Anthropic API HTTP client implementing Classifier.

use super::*;
use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

pub struct AnthropicClassifier {
    pub api_key: String,
    pub model: String,
    pub base_url: String, // overridable for tests
}

impl AnthropicClassifier {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key =
            std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY env var not set")?;
        Ok(Self {
            api_key,
            model: "claude-haiku-4-5-20251001".into(),
            base_url: "https://api.anthropic.com".into(),
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

impl Classifier for AnthropicClassifier {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
        let prompt = crate::classifier::prompt::build(input);
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: 256,
            messages: vec![MessageIn {
                role: "user",
                content: &prompt,
            }],
        };

        let url = format!("{}/v1/messages", self.base_url);
        let resp: MessagesResponse = ureq::post(&url)
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
        let out: ClassifyOutput = serde_json::from_str(json_str)
            .with_context(|| format!("classifier JSON parse failed; got: {json_str}"))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    #[test]
    fn classifier_parses_anthropic_response() {
        let mut server = mockito::Server::new();
        let url = server.url();

        let body = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": "claude-haiku-4-5-20251001",
            "content": [
                { "type": "text", "text": "{\"event_type\":\"decision\",\"task_id_guess\":\"tj-x\",\"confidence\":0.93,\"evidence_strength\":null,\"suggested_text\":\"Adopt Rust.\"}" }
            ],
            "stop_reason": "end_turn"
        });

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create();

        let c = AnthropicClassifier {
            api_key: "test".into(),
            model: "claude-haiku-4-5-20251001".into(),
            base_url: url,
        };
        let out = c
            .classify(&ClassifyInput {
                text: "We adopted Rust.".into(),
                author_hint: "assistant".into(),
                recent_tasks: vec![],
            })
            .unwrap();

        assert_eq!(out.event_type, EventType::Decision);
        assert_eq!(out.task_id_guess.as_deref(), Some("tj-x"));
        assert!((out.confidence - 0.93).abs() < 1e-6);
        mock.assert();
    }
}
