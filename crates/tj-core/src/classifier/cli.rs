//! Subscription-based classifier: shells out to `claude -p` (Claude Code CLI)
//! and uses the user's existing Pro/Max subscription instead of an API key.
//!
//! Rationale: most Claude Code users have a Pro/Max subscription but no
//! separate `ANTHROPIC_API_KEY` from console.anthropic.com. The CLI's `--print`
//! mode runs inference through the same auth as the interactive session, so
//! we can reuse it for classification without a second paid product.

use super::*;
use anyhow::{anyhow, Context};
use serde::Deserialize;

/// Backend that invokes `claude -p` via subprocess.
///
/// Configuration:
/// - `command`: program name (default `"claude"`); override for tests/dev.
/// - `model`: model alias passed via `--model` (default `"haiku"`; cheaper than the user's session model).
pub struct ClaudeCliClassifier {
    pub command: String,
    pub model: String,
}

impl Default for ClaudeCliClassifier {
    fn default() -> Self {
        Self {
            command: "claude".into(),
            model: "haiku".into(),
        }
    }
}

#[derive(Deserialize)]
struct CliResult {
    /// `result` in `--output-format json` is the model's text output.
    result: String,
    is_error: bool,
}

impl Classifier for ClaudeCliClassifier {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
        let prompt = crate::classifier::prompt::build(input);

        let output = std::process::Command::new(&self.command)
            .args([
                "-p",
                "--model",
                &self.model,
                "--output-format",
                "json",
                "--bare", // skip hooks/skills/CLAUDE.md to avoid recursion + speed up
                &prompt,
            ])
            .output()
            .with_context(|| format!("spawn `{}` for classification", self.command))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "claude -p exited with {} — stderr: {}",
                output.status,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8(output.stdout).context("claude -p stdout not UTF-8")?;
        let cli_result: CliResult = serde_json::from_str(stdout.trim())
            .with_context(|| format!("parse claude -p JSON envelope; got: {}", stdout.trim()))?;

        if cli_result.is_error {
            return Err(anyhow!(
                "claude -p reported error: {}. If 'Not logged in' — run `claude /login` first.",
                cli_result.result
            ));
        }

        // The model's reply is in `result`; it MUST be JSON matching ClassifyOutput.
        let inner_text = cli_result
            .result
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        let out: ClassifyOutput = serde_json::from_str(inner_text)
            .with_context(|| format!("classifier inner JSON parse failed; got: {inner_text}"))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    /// Build a fake `claude` script that prints a canned `--output-format json` envelope.
    /// Returns the path so we can point ClaudeCliClassifier at it.
    fn fake_claude(dir: &std::path::Path, envelope: &str) -> std::path::PathBuf {
        let path = dir.join("fake-claude");
        let script = format!("#!/bin/bash\ncat <<'EOF'\n{envelope}\nEOF\n");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
    fn classifier_parses_cli_envelope_and_returns_classified_output() {
        let dir = tempfile::TempDir::new().unwrap();

        // Fake CLI that pretends to be claude -p: returns the wrapper JSON
        // with the inner classifier JSON as `result`.
        let inner = r#"{"event_type":"decision","task_id_guess":"tj-x","confidence":0.93,"evidence_strength":null,"suggested_text":"Adopt Rust."}"#;
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": inner,
        });
        let fake = fake_claude(dir.path(), &envelope.to_string());

        let c = ClaudeCliClassifier {
            command: fake.to_string_lossy().to_string(),
            model: "haiku".into(),
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
    }

    #[test]
    #[cfg(unix)]
    fn classifier_surfaces_not_logged_in_with_friendly_hint() {
        let dir = tempfile::TempDir::new().unwrap();
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "result": "Not logged in · Please run /login",
        });
        let fake = fake_claude(dir.path(), &envelope.to_string());

        let c = ClaudeCliClassifier {
            command: fake.to_string_lossy().to_string(),
            model: "haiku".into(),
        };
        let err = c
            .classify(&ClassifyInput {
                text: "x".into(),
                author_hint: "user".into(),
                recent_tasks: vec![],
            })
            .unwrap_err()
            .to_string();
        assert!(err.contains("Not logged in"));
        assert!(err.contains("claude /login"));
    }
}
