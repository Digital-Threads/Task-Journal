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
/// - `command`: full command line that produces `claude` invocation;
///   default `"claude"`. May contain spaces to wrap the binary in a
///   workspace orchestrator like `aimux run dt claude` or a Nix
///   shell. Override via `TJ_CLASSIFIER_CLI` env var.
/// - `model`: model alias passed via `--model`. Overridable via
///   `TJ_CLASSIFIER_MODEL`; falls back to `DEFAULT_MODEL` (haiku —
///   cheaper than the user's session model).
pub struct ClaudeCliClassifier {
    pub command: String,
    pub model: String,
}

/// Default model when `TJ_CLASSIFIER_MODEL` is not set.
pub const DEFAULT_MODEL: &str = "haiku";

impl Default for ClaudeCliClassifier {
    fn default() -> Self {
        Self {
            command: std::env::var("TJ_CLASSIFIER_CLI").unwrap_or_else(|_| "claude".into()),
            model: std::env::var("TJ_CLASSIFIER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into()),
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

        // Split command on whitespace so users can wrap the binary
        // in a workspace orchestrator: `aimux run dt claude`,
        // `nix run nixpkgs#claude --`, etc.
        let mut parts = self.command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| anyhow!("TJ_CLASSIFIER_CLI is empty"))?;
        let base_args: Vec<&str> = parts.collect();

        let output = std::process::Command::new(program)
            .args(&base_args)
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

// Tests use a tiny shell/.cmd shim to fake the `claude` CLI. Cross-platform
// strategy: write the JSON envelope to a file, then a one-liner script that
// `cat`s (Unix) or `type`s (Windows) it back. The `type` form sidesteps cmd
// .exe escaping pain for the JSON payload's quotes.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    /// Build a fake `claude` shim that prints a canned `--output-format json`
    /// envelope. Returns the path so we can point ClaudeCliClassifier at it.
    fn fake_claude(dir: &std::path::Path, envelope: &str) -> std::path::PathBuf {
        let json_path = dir.join("fake-claude-output.json");
        std::fs::write(&json_path, envelope).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("fake-claude.sh");
            let script = format!("#!/bin/sh\ncat \"{}\"\n", json_path.to_string_lossy());
            std::fs::write(&path, script).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        }
        #[cfg(windows)]
        {
            let path = dir.join("fake-claude.cmd");
            // `type "PATH"` outputs file content verbatim; double quotes
            // handle spaces, and JSON's special chars stay literal because
            // type does not interpret content as commands.
            let script = format!("@echo off\r\ntype \"{}\"\r\n", json_path.to_string_lossy());
            std::fs::write(&path, script).unwrap();
            path
        }
    }

    #[test]
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
    fn classifier_surfaces_not_logged_in_with_friendly_hint() {
        let dir = tempfile::TempDir::new().unwrap();
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            // ASCII-only payload: Windows `type` (used by fake-claude.cmd)
            // emits via the console code page, which mangles non-ASCII bytes
            // (U+00B7 etc.) before our UTF-8 decode in `classify`. Real
            // `claude` always emits UTF-8 directly, so this is a fake-only
            // concern, not a production behavior change.
            "result": "Not logged in - Please run /login",
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

    #[test]
    fn classifier_command_with_spaces_runs_wrapper_then_target() {
        // Simulates `aimux run dt claude` style wrappers: a launcher
        // script that ignores its first argv, then forwards everything
        // else to the real fake-claude. We verify TJ_CLASSIFIER_CLI
        // splitting works end-to-end.
        let dir = tempfile::TempDir::new().unwrap();

        let inner = r#"{"event_type":"finding","task_id_guess":null,"confidence":0.9,"evidence_strength":null,"suggested_text":"x"}"#;
        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": inner,
        });
        let real_fake = fake_claude(dir.path(), &envelope.to_string());

        // Wrapper script that takes a "profile" arg and delegates.
        #[cfg(unix)]
        let wrapper = {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join("fake-aimux.sh");
            // shellcheck-clean: we intentionally drop $1 (profile name)
            // and forward $2..$N to the real fake.
            let script = format!(
                "#!/bin/sh\nshift\nshift\nshift\nexec \"{}\" \"$@\"\n",
                real_fake.to_string_lossy()
            );
            std::fs::write(&path, script).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        };
        #[cfg(windows)]
        let wrapper = {
            let path = dir.path().join("fake-aimux.cmd");
            // Drop %1 %2 %3 (run dt claude) and pass the rest.
            let script = format!(
                "@echo off\r\ncall \"{}\" %4 %5 %6 %7 %8 %9\r\n",
                real_fake.to_string_lossy()
            );
            std::fs::write(&path, script).unwrap();
            path
        };

        let c = ClaudeCliClassifier {
            command: format!("{} run dt claude", wrapper.to_string_lossy()),
            model: "haiku".into(),
        };
        let out = c
            .classify(&ClassifyInput {
                text: "x".into(),
                author_hint: "user".into(),
                recent_tasks: vec![],
            })
            .unwrap();
        assert_eq!(out.event_type, EventType::Finding);
    }
}
