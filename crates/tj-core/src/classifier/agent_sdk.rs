//! Claude CLI ("agent SDK") classifier backend.
//!
//! Runs the locally-installed, already-authenticated `claude` binary in
//! non-interactive print mode, pinned to Haiku, to classify a chunk *without*
//! an `ANTHROPIC_API_KEY`. This resurrects the v0.7.x `cli` backend that was
//! removed in v0.8.0 — but honestly: since **2026-06-15** a headless
//! `claude -p` run draws from the separate **Agent SDK** monthly credit pool
//! (~$20 Pro / $100 Max 5x / $200 Max 20x, at API rates), not the interactive
//! Pro/Max pool. Classification is Haiku-class and tiny (a few hundred tokens
//! per chunk), so the credit lasts a long time — but it is not strictly free.
//!
//! The command execution is abstracted behind [`CommandRunner`] so the parsing
//! path is unit-testable with a fake; the suite never shells out to `claude`.

use super::{Classifier, ClassifyInput, ClassifyOutput};
use anyhow::{anyhow, Context};
use std::process::Command;

/// Default model. `claude --model` accepts the short alias and resolves it to
/// the current dated id (`claude-haiku-4-5-20251001`). Override with
/// `TJ_AGENT_SDK_MODEL`.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

/// Env var stamped onto every spawned classifier `claude -p` subprocess. That
/// subprocess is a full Claude Code instance, so on startup it re-runs the
/// user's SessionStart hooks — including `task-journal ingest-hook`, which
/// would spawn yet another classifier `claude -p`, and so on: an unbounded
/// fork bomb. `ingest-hook` checks for this marker and no-ops when it is set,
/// breaking the recursion. The CLI guard and the worker's `env_remove` both
/// reference this constant so the setter and the checker can never drift
/// (which is exactly the bug that let the fork bomb through: the guard checked
/// `TJ_IN_CLASSIFIER` but no spawn site ever set it).
pub const IN_CLASSIFIER_ENV: &str = "TJ_IN_CLASSIFIER";

/// "Run the classifier command and hand back its raw stdout." The production
/// impl shells out to `claude`; tests inject a fake returning canned JSON.
pub trait CommandRunner: Send + Sync {
    /// Run the classification for `prompt` against `model`, returning the raw
    /// stdout (the `--output-format json` wrapper) on success.
    fn run(&self, model: &str, prompt: &str) -> anyhow::Result<String>;
}

/// Build the base `claude` invocation shared by both runners: print mode, the
/// pinned model, the JSON envelope, an isolated MCP config, and — critically —
/// the [`IN_CLASSIFIER_ENV`] recursion marker. The argv runner appends the
/// prompt as a positional arg; the stdin runner feeds it on stdin. Extracted so
/// a unit test can assert the marker is present without spawning `claude` (the
/// missing marker is exactly what let the fork bomb through before).
fn base_claude_command(model: &str) -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg("--model")
        .arg(model)
        .arg("--output-format")
        .arg("json")
        .arg("--strict-mcp-config")
        .env(IN_CLASSIFIER_ENV, "1");
    cmd
}

/// Production runner: invokes the local `claude` binary in print mode, pinned
/// to the given model, asking for the JSON envelope and an isolated MCP config
/// (`--strict-mcp-config` keeps the project's own MCP servers — including this
/// very journal — out of the classification subprocess).
pub struct ClaudeBinaryRunner;

/// Build the error for a non-zero `claude -p` exit. With `--output-format
/// json` claude reports the real cause (invalid model, usage limit, auth) as
/// JSON on **stdout**, not stderr — so surface both, capped, or the user just
/// sees a bare "exit status 1".
fn claude_exit_error(
    status: std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> anyhow::Error {
    let cap = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        let s = s.trim().to_string();
        if s.chars().count() > 600 {
            format!("{}…", s.chars().take(600).collect::<String>())
        } else {
            s
        }
    };
    let out = cap(stdout);
    let err = cap(stderr);
    let detail = match (out.is_empty(), err.is_empty()) {
        (true, true) => "(no output)".to_string(),
        (false, true) => out,
        (true, false) => err,
        (false, false) => format!("{err} | stdout: {out}"),
    };
    anyhow!("`claude -p` exited with {status}: {detail}")
}

/// Per-call wall-clock ceiling for a `claude -p` invocation. A spawned full
/// Claude Code instance normally answers in seconds; this kills a wedged one so
/// a multi-chunk enrich can't hang the whole `complete`. Override with
/// `TJ_CLAUDE_TIMEOUT_SECS`.
fn claude_timeout() -> std::time::Duration {
    let secs = std::env::var("TJ_CLAUDE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(90);
    std::time::Duration::from_secs(secs)
}

/// Wait for `child` up to `timeout`, draining stdout/stderr concurrently so a
/// full pipe can't deadlock the wait. On timeout the child is killed and an
/// error returned; otherwise the captured output is handed back.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    use std::io::Read;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let so = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut b);
        }
        b
    });
    let se = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut b);
        }
        b
    });
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("`claude -p` timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    };
    Ok(std::process::Output {
        status,
        stdout: so.join().unwrap_or_default(),
        stderr: se.join().unwrap_or_default(),
    })
}

impl CommandRunner for ClaudeBinaryRunner {
    fn run(&self, model: &str, prompt: &str) -> anyhow::Result<String> {
        let child = base_claude_command(model)
            .arg(prompt)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to spawn `claude` (is Claude Code installed and on PATH?)")?;
        let output = wait_with_timeout(child, claude_timeout())?;
        if !output.status.success() {
            return Err(claude_exit_error(
                output.status,
                &output.stdout,
                &output.stderr,
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Like [`ClaudeBinaryRunner`] but feeds the prompt on **stdin** instead of as
/// an argv argument. Use for large prompts (e.g. a whole session transcript in
/// dream backfill) that would otherwise blow the per-argument size limit
/// (`E2BIG`, ~128 KiB on Linux). `claude -p` with no positional prompt reads
/// the prompt from stdin.
pub struct ClaudeBinaryStdinRunner;

impl CommandRunner for ClaudeBinaryStdinRunner {
    fn run(&self, model: &str, prompt: &str) -> anyhow::Result<String> {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = base_claude_command(model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn `claude` (is Claude Code installed and on PATH?)")?;
        // Write the prompt, then drop the handle to close stdin so `claude`
        // sees EOF and starts working.
        child
            .stdin
            .take()
            .context("claude stdin was not captured")?
            .write_all(prompt.as_bytes())
            .context("failed to write prompt to claude stdin")?;
        let output = wait_with_timeout(child, claude_timeout())?;
        if !output.status.success() {
            return Err(claude_exit_error(
                output.status,
                &output.stdout,
                &output.stderr,
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

pub struct ClaudeCliClassifier {
    model: String,
    runner: Box<dyn CommandRunner>,
}

impl ClaudeCliClassifier {
    /// Build from environment. Returns `None` unless a `claude` binary is on
    /// PATH (probed with `claude --version`) — the caller then falls through to
    /// the next backend. Model comes from `TJ_AGENT_SDK_MODEL`, else Haiku.
    pub fn from_env() -> Option<Self> {
        if !claude_on_path() {
            return None;
        }
        let model = std::env::var("TJ_AGENT_SDK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        Some(Self {
            model,
            runner: Box::new(ClaudeBinaryRunner),
        })
    }

    /// Test/dev constructor: inject a fake runner and an explicit model so the
    /// parse path can be exercised without a live `claude` login.
    pub fn with_runner(model: impl Into<String>, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            model: model.into(),
            runner,
        }
    }
}

/// The JSON wrapper emitted by `claude --output-format json`. We only need the
/// error flag and the `result` string (the model's verdict text); the rest of
/// the envelope (usage, cost, timings) is ignored.
#[derive(serde::Deserialize)]
struct CliEnvelope {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
}

impl Classifier for ClaudeCliClassifier {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
        let prompt = crate::classifier::prompt::build(input);
        let verdict = run_claude_json(self.runner.as_ref(), &self.model, &prompt)?;
        super::parse_verdict(&verdict)
    }
}

/// Run `prompt` through the claude CLI (via `runner`) and return the model's
/// reply text — the `result` field of the `--output-format json` envelope.
/// Shared by the classifier and the dream agent-sdk backend so the envelope
/// handling lives in one place.
pub fn run_claude_json(
    runner: &dyn CommandRunner,
    model: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    let stdout = runner.run(model, prompt)?;
    let envelope: CliEnvelope = serde_json::from_str(stdout.trim()).with_context(|| {
        format!(
            "claude --output-format json wrapper parse failed; got: {}",
            stdout.trim()
        )
    })?;
    if envelope.is_error {
        return Err(anyhow!(
            "claude reported an error (subtype={})",
            envelope.subtype.as_deref().unwrap_or("unknown")
        ));
    }
    envelope
        .result
        .ok_or_else(|| anyhow!("claude json wrapper had no `result` field"))
}

/// Probe whether `claude` resolves on PATH and runs. Cheap (`--version` does
/// no network) and tolerant — any spawn/exec failure means "not available".
pub fn claude_on_path() -> bool {
    Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::{decide_status, CONFIDENCE_THRESHOLD};
    use crate::event::{EventStatus, EventType};

    /// Fake runner: returns canned stdout, ignoring model/prompt. Captures the
    /// model it was asked for so tests can assert the pin.
    struct FakeRunner {
        canned: String,
        seen_model: std::sync::Mutex<Option<String>>,
    }

    impl FakeRunner {
        fn new(canned: impl Into<String>) -> Self {
            Self {
                canned: canned.into(),
                seen_model: std::sync::Mutex::new(None),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, model: &str, _prompt: &str) -> anyhow::Result<String> {
            *self.seen_model.lock().unwrap() = Some(model.to_string());
            Ok(self.canned.clone())
        }
    }

    fn input() -> ClassifyInput {
        ClassifyInput {
            text: "We adopted Rust for the journal core.".into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![],
        }
    }

    fn envelope(result_json: &str) -> String {
        serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": result_json,
        })
        .to_string()
    }

    #[test]
    fn base_command_carries_recursion_marker() {
        use std::ffi::OsStr;
        // The tj-cli ingest-hook guard short-circuits on this exact var; if the
        // const and the spawn site ever drift, the fork bomb returns.
        assert_eq!(IN_CLASSIFIER_ENV, "TJ_IN_CLASSIFIER");
        let cmd = base_claude_command("claude-haiku-4-5");
        let marker = cmd
            .get_envs()
            .any(|(k, v)| k == OsStr::new(IN_CLASSIFIER_ENV) && v == Some(OsStr::new("1")));
        assert!(
            marker,
            "every spawned `claude -p` must set {IN_CLASSIFIER_ENV}=1 to break ingest-hook recursion"
        );
    }

    #[test]
    fn parses_canned_verdict_into_classify_output() {
        let verdict = r#"{"event_type":"decision","task_id_guess":"tj-x","confidence":0.93,"evidence_strength":null,"suggested_text":"Adopt Rust."}"#;
        let c = ClaudeCliClassifier::with_runner(
            DEFAULT_MODEL,
            Box::new(FakeRunner::new(envelope(verdict))),
        );
        let out = c.classify(&input()).unwrap();
        assert_eq!(out.event_type, EventType::Decision);
        assert_eq!(out.task_id_guess.as_deref(), Some("tj-x"));
        assert!((out.confidence - 0.93).abs() < 1e-6);
        // 0.93 >= 0.85 → confirmed.
        assert_eq!(decide_status(out.confidence), EventStatus::Confirmed);
    }

    /// Adapter so a test can keep an `Arc` handle to inspect the runner after
    /// it is boxed into the classifier.
    struct ArcRunner(std::sync::Arc<FakeRunner>);
    impl CommandRunner for ArcRunner {
        fn run(&self, model: &str, prompt: &str) -> anyhow::Result<String> {
            self.0.run(model, prompt)
        }
    }

    #[test]
    fn pins_the_configured_model() {
        let verdict = r#"{"event_type":"finding","task_id_guess":null,"confidence":0.9,"evidence_strength":null,"suggested_text":"x"}"#;
        let captured = std::sync::Arc::new(FakeRunner::new(envelope(verdict)));
        let c = ClaudeCliClassifier::with_runner(
            "claude-haiku-4-5",
            Box::new(ArcRunner(captured.clone())),
        );
        let _ = c.classify(&input()).unwrap();
        assert_eq!(
            captured.seen_model.lock().unwrap().as_deref(),
            Some("claude-haiku-4-5"),
            "classifier must pin the model it was constructed with"
        );
    }

    #[test]
    fn decide_status_at_the_0_85_threshold() {
        for (conf, expect) in [
            (0.85_f64, EventStatus::Confirmed),
            (0.84_f64, EventStatus::Suggested),
        ] {
            let verdict = format!(
                r#"{{"event_type":"evidence","task_id_guess":null,"confidence":{conf},"evidence_strength":"strong","suggested_text":"t"}}"#
            );
            let c = ClaudeCliClassifier::with_runner(
                DEFAULT_MODEL,
                Box::new(FakeRunner::new(envelope(&verdict))),
            );
            let out = c.classify(&input()).unwrap();
            assert!((out.confidence - conf).abs() < 1e-6);
            assert_eq!(decide_status(out.confidence), expect);
            assert_eq!(CONFIDENCE_THRESHOLD, 0.85);
        }
    }

    #[test]
    fn tolerates_code_fence_wrapped_verdict() {
        let verdict = "```json\n{\"event_type\":\"rejection\",\"task_id_guess\":null,\"confidence\":0.88,\"evidence_strength\":null,\"suggested_text\":\"won't work\"}\n```";
        let c = ClaudeCliClassifier::with_runner(
            DEFAULT_MODEL,
            Box::new(FakeRunner::new(envelope(verdict))),
        );
        let out = c.classify(&input()).unwrap();
        assert_eq!(out.event_type, EventType::Rejection);
    }

    #[test]
    fn errors_when_claude_reports_is_error() {
        let canned = serde_json::json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "result": null,
        })
        .to_string();
        let c = ClaudeCliClassifier::with_runner(DEFAULT_MODEL, Box::new(FakeRunner::new(canned)));
        let err = c.classify(&input()).unwrap_err();
        assert!(format!("{err}").contains("error"), "got: {err}");
    }
}
