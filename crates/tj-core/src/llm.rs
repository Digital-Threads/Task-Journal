//! Pluggable LLM backend for the journal's optional AI operations
//! (consolidation, dream backfill). One small trait, several adapters, picked by
//! name so this public package can grow new providers without touching callers.
//!
//! Default is **`claude-p`** — the local Claude CLI on your subscription, so the
//! out-of-the-box experience needs no API key. Override with `TJ_BACKEND` (env,
//! global) or a per-command `--backend`:
//!
//! - `claude-p` (default) — local `claude -p`, Haiku, subscription auth.
//! - `anthropic` — direct Anthropic API (`ANTHROPIC_API_KEY`).
//! - `openai` — any OpenAI-compatible chat API (`OPENAI_API_KEY`,
//!   `TJ_OPENAI_BASE_URL`, `TJ_OPENAI_MODEL`). Covers OpenAI, Codex, and other
//!   compatible providers by pointing the base URL.
//! - `ollama` — a local Ollama model (its OpenAI-compatible endpoint), **free**:
//!   no key, no network beyond localhost. `TJ_OLLAMA_URL`, `TJ_OLLAMA_MODEL`.
//!
//! A backend that isn't usable (no key, no `claude` on PATH) yields `Ok(None)`
//! from [`backend_from_env`] so the caller skips cleanly — we never fabricate
//! output without a model.

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Token usage reported by a backend for one call. `cost_usd` is `None` when
/// the backend doesn't report a price (most APIs report tokens, not dollars;
/// `claude -p` reports `total_cost_usd`, which is 0 under a subscription).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LlmUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
}

impl LlmUsage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Accumulate another call's usage into this one.
    pub fn add(&mut self, other: LlmUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (a, None) => a,
            (None, b) => b,
        };
    }
}

/// One AI call: a prompt in, the model's text reply out.
pub trait LlmBackend: Send + Sync {
    fn complete(&self, prompt: &str, max_tokens: u32) -> anyhow::Result<String>;
    /// Stable label for logs / provenance.
    fn name(&self) -> &'static str;
    /// Like [`complete`](Self::complete) but also reports token usage when the
    /// backend exposes it. Default: run `complete` and report no usage, so
    /// mocks and minimal backends need not implement it.
    fn complete_usage(&self, prompt: &str, max_tokens: u32) -> anyhow::Result<(String, LlmUsage)> {
        Ok((self.complete(prompt, max_tokens)?, LlmUsage::default()))
    }
}

/// Resolve the backend from an explicit name (e.g. a `--backend` flag) or
/// `TJ_BACKEND`, defaulting to `claude-p`. Returns:
/// - `Ok(Some(_))` — a usable backend,
/// - `Ok(None)` — the chosen backend is unavailable (no key / no `claude`); the
///   caller should skip,
/// - `Err(_)` — an unknown backend name (a typo worth surfacing).
pub fn backend_from_env(explicit: Option<&str>) -> anyhow::Result<Option<Box<dyn LlmBackend>>> {
    let name = explicit
        .map(str::to_string)
        .or_else(|| std::env::var("TJ_BACKEND").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "claude-p".to_string());

    match name.trim() {
        "claude-p" | "claude" | "agent-sdk" => {
            if crate::classifier::agent_sdk::claude_on_path() {
                Ok(Some(Box::new(ClaudeCliBackend::from_env())))
            } else {
                Ok(None)
            }
        }
        "anthropic" | "api" => match std::env::var("ANTHROPIC_API_KEY") {
            Ok(key) if !key.is_empty() => Ok(Some(Box::new(AnthropicBackend::new(key)))),
            _ => Ok(None),
        },
        "openai" | "codex" => match std::env::var("OPENAI_API_KEY") {
            Ok(key) if !key.is_empty() => Ok(Some(Box::new(OpenAiBackend::openai(key)))),
            _ => Ok(None),
        },
        "ollama" => Ok(Some(Box::new(OpenAiBackend::ollama()))),
        other => Err(anyhow!(
            "unknown backend '{other}' (expected: claude-p, anthropic, openai, ollama)"
        )),
    }
}

// ---------------------------------------------------------------------------
// claude -p (default) — local CLI, subscription auth, no API key.
// ---------------------------------------------------------------------------

pub struct ClaudeCliBackend {
    model: String,
}

impl ClaudeCliBackend {
    pub fn from_env() -> Self {
        let model = std::env::var("TJ_CONSOLIDATE_MODEL")
            .unwrap_or_else(|_| crate::classifier::agent_sdk::DEFAULT_MODEL.to_string());
        Self { model }
    }
}

impl LlmBackend for ClaudeCliBackend {
    fn complete(&self, prompt: &str, max_tokens: u32) -> anyhow::Result<String> {
        self.complete_usage(prompt, max_tokens).map(|(t, _)| t)
    }
    fn name(&self) -> &'static str {
        "claude-p"
    }
    fn complete_usage(&self, prompt: &str, _max_tokens: u32) -> anyhow::Result<(String, LlmUsage)> {
        crate::classifier::agent_sdk::run_claude_json_usage(
            &crate::classifier::agent_sdk::ClaudeBinaryStdinRunner,
            &self.model,
            prompt,
        )
    }
}

// ---------------------------------------------------------------------------
// Anthropic direct API.
// ---------------------------------------------------------------------------

pub struct AnthropicBackend {
    api_key: String,
    model: String,
    base_url: String,
    timeout: Duration,
}

impl AnthropicBackend {
    pub fn new(api_key: String) -> Self {
        let model = std::env::var("TJ_CONSOLIDATE_MODEL")
            .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string());
        let base_url = std::env::var("TJ_CONSOLIDATE_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
        Self {
            api_key,
            model,
            base_url,
            timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Serialize)]
struct AnthropicReq<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthropicMsg<'a>>,
}
#[derive(Serialize)]
struct AnthropicMsg<'a> {
    role: &'a str,
    content: &'a str,
}
#[derive(Deserialize)]
struct AnthropicResp {
    content: Vec<AnthropicBlock>,
    #[serde(default)]
    usage: AnthropicUsage,
}
#[derive(Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}
#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

impl LlmBackend for AnthropicBackend {
    fn complete(&self, prompt: &str, max_tokens: u32) -> anyhow::Result<String> {
        self.complete_usage(prompt, max_tokens).map(|(t, _)| t)
    }
    fn name(&self) -> &'static str {
        "anthropic"
    }
    fn complete_usage(&self, prompt: &str, max_tokens: u32) -> anyhow::Result<(String, LlmUsage)> {
        let body = AnthropicReq {
            model: &self.model,
            max_tokens,
            messages: vec![AnthropicMsg {
                role: "user",
                content: prompt,
            }],
        };
        let resp: AnthropicResp = ureq::post(&format!("{}/v1/messages", self.base_url))
            .timeout(self.timeout)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .set("content-type", "application/json")
            .send_json(serde_json::to_value(&body)?)
            .context("Anthropic API request failed")?
            .into_json()
            .context("decode Anthropic response")?;
        let usage = LlmUsage {
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
            cost_usd: None,
        };
        let text = resp
            .content
            .iter()
            .find(|b| b.kind == "text")
            .map(|b| b.text.clone())
            .ok_or_else(|| anyhow!("no text content in Anthropic response"))?;
        Ok((text, usage))
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible — covers OpenAI, Codex, Ollama, and any compatible server.
// ---------------------------------------------------------------------------

pub struct OpenAiBackend {
    api_key: Option<String>,
    model: String,
    base_url: String,
    label: &'static str,
    timeout: Duration,
}

impl OpenAiBackend {
    pub fn openai(api_key: String) -> Self {
        Self {
            api_key: Some(api_key),
            model: std::env::var("TJ_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
            base_url: std::env::var("TJ_OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com".to_string()),
            label: "openai",
            timeout: Duration::from_secs(60),
        }
    }

    pub fn ollama() -> Self {
        Self {
            api_key: None, // local; no auth
            model: std::env::var("TJ_OLLAMA_MODEL").unwrap_or_else(|_| "llama3.1".to_string()),
            base_url: std::env::var("TJ_OLLAMA_URL")
                .unwrap_or_else(|_| "http://localhost:11434".to_string()),
            label: "ollama",
            timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Serialize)]
struct OpenAiReq<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthropicMsg<'a>>,
}
#[derive(Deserialize)]
struct OpenAiResp {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: OpenAiUsage,
}
#[derive(Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}
#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMsg,
}
#[derive(Deserialize)]
struct OpenAiMsg {
    #[serde(default)]
    content: String,
}

impl LlmBackend for OpenAiBackend {
    fn complete(&self, prompt: &str, max_tokens: u32) -> anyhow::Result<String> {
        self.complete_usage(prompt, max_tokens).map(|(t, _)| t)
    }
    fn complete_usage(&self, prompt: &str, max_tokens: u32) -> anyhow::Result<(String, LlmUsage)> {
        let body = OpenAiReq {
            model: &self.model,
            max_tokens,
            messages: vec![AnthropicMsg {
                role: "user",
                content: prompt,
            }],
        };
        let mut req = ureq::post(&format!("{}/v1/chat/completions", self.base_url))
            .timeout(self.timeout)
            .set("content-type", "application/json");
        if let Some(key) = &self.api_key {
            req = req.set("authorization", &format!("Bearer {key}"));
        }
        let resp: OpenAiResp = req
            .send_json(serde_json::to_value(&body)?)
            .with_context(|| format!("{} request failed", self.label))?
            .into_json()
            .context("decode OpenAI-compatible response")?;
        let usage = LlmUsage {
            input_tokens: resp.usage.prompt_tokens,
            output_tokens: resp.usage.completion_tokens,
            cost_usd: None,
        };
        let text = resp
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow!("no choices in {} response", self.label))?;
        Ok((text, usage))
    }
    fn name(&self) -> &'static str {
        self.label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard(&'static str, Option<String>);
    impl EnvGuard {
        fn set(k: &'static str, v: &str) -> Self {
            let prev = std::env::var(k).ok();
            std::env::set_var(k, v);
            Self(k, prev)
        }
        fn unset(k: &'static str) -> Self {
            let prev = std::env::var(k).ok();
            std::env::remove_var(k);
            Self(k, prev)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.1 {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }

    // Serialise env-touching tests (process-global env).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn unknown_backend_errors() {
        let _l = ENV_LOCK.lock().unwrap();
        assert!(backend_from_env(Some("nonsense")).is_err());
    }

    #[test]
    fn anthropic_unavailable_without_key_is_none() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("ANTHROPIC_API_KEY");
        assert!(backend_from_env(Some("anthropic")).unwrap().is_none());
    }

    #[test]
    fn anthropic_with_key_resolves() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("ANTHROPIC_API_KEY", "k");
        let b = backend_from_env(Some("anthropic")).unwrap().unwrap();
        assert_eq!(b.name(), "anthropic");
    }

    #[test]
    fn ollama_always_resolves_no_key() {
        let _l = ENV_LOCK.lock().unwrap();
        let b = backend_from_env(Some("ollama")).unwrap().unwrap();
        assert_eq!(b.name(), "ollama");
    }

    #[test]
    fn openai_calls_chat_completions_and_parses() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "choices": [{"message": {"role": "assistant", "content": "hello from openai"}}]
                })
                .to_string(),
            )
            .create();
        let b = OpenAiBackend {
            api_key: Some("k".into()),
            model: "gpt-4o-mini".into(),
            base_url: server.url(),
            label: "openai",
            timeout: Duration::from_secs(5),
        };
        let out = b.complete("hi", 64).unwrap();
        m.assert();
        assert_eq!(out, "hello from openai");
    }

    #[test]
    fn anthropic_calls_messages_and_parses() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "content": [{"type": "text", "text": "hello from anthropic"}]
                })
                .to_string(),
            )
            .create();
        let b = AnthropicBackend {
            api_key: "k".into(),
            model: "claude-haiku-4-5-20251001".into(),
            base_url: server.url(),
            timeout: Duration::from_secs(5),
        };
        let out = b.complete("hi", 64).unwrap();
        m.assert();
        assert_eq!(out, "hello from anthropic");
    }
}
