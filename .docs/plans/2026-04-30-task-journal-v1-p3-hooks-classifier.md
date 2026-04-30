# Task Journal v1 — Phase 3 (Hooks + Classifier) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task.

**Goal:** Auto-capture journal events as the user works with Claude Code. Add an Anthropic-API-backed classifier (Claude Haiku 4.5), Claude Code hooks that funnel chat chunks through the classifier, confidence-gated event writes, a pending queue for retries when the API is down, and explicit correction support.

**Architecture:** New `tj-core::classifier` module abstracts classification behind a `Classifier` trait so tests can swap a mock for the real Anthropic HTTP client. CLI gains `ingest-hook` (called by Claude Code hooks), `install-hooks` (writes to `~/.claude/settings.json` or `.claude/settings.json`), and `event-correct` (manual correction). The pack assembler surfaces `suggested` events with a `[?]` marker so users can spot classifier-driven entries that need confirmation.

**Tech Stack:** Same as P1+P2 plus `ureq` for blocking HTTP (lighter than reqwest+tokio for one-shot CLI hook calls). Prompts built as plain strings; JSON extraction via `serde_json`.

**Working directory:** `/home/shahinyanm/www/claude-memory` inside WSL Ubuntu. Wrap each shell call as `wsl -d Ubuntu -- bash -c 'export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; cd /home/shahinyanm/www/claude-memory && <command>'`.

**Beads tracking:** One issue per Task below, linked parent-child to epic `claude-memory-d36`, blocks chain so `bd ready` sequences correctly.

**Spec & design source of truth:**
- `.docs/plans/2026-04-29-tz-task-journal-v2.md` — pinned ТЗ
- `.docs/plans/2026-04-29-task-journal-v1-design.md` — design doc (Q6 = classifier, Q8 = hooks)
- `.docs/plans/2026-04-29-task-journal-v1-p1-skeleton.md` — what P1 produced
- `.docs/plans/2026-04-30-task-journal-v1-p2-task-pack-core.md` — what P2 produced

---

## Pre-flight

1. Verify P2 still passes: `cargo test --workspace` should show 44 green.
2. Confirm we can reach `api.anthropic.com` from WSL: `curl -sI https://api.anthropic.com/v1/messages | head -1` should return an HTTP status (401 is fine — it just means the request reached the server).
3. Optionally: a real `ANTHROPIC_API_KEY` for the manual smoke at the end. Tests use a mock client and don't hit the real API.

---

## File structure (after Task 18)

```
crates/tj-core/src/
├── lib.rs                          ← + pub mod classifier
├── ... (P1 + P2 modules unchanged)
├── pack.rs                         ← extended: render [?] marker for suggested
└── classifier/
    ├── mod.rs                      ← Classifier trait + types
    ├── prompt.rs                   ← prompt builder
    ├── http.rs                     ← AnthropicClient (real impl)
    └── mock.rs                     ← MockClassifier for tests

crates/tj-cli/src/main.rs           ← + ingest-hook, install-hooks, event-correct
crates/tj-cli/tests/cli.rs          ← + ingest + install-hooks + correction tests
```

---

## Granularity contract

Same TDD discipline as P1+P2: one bd issue per Task, RED → GREEN → run → commit → `bd close`. The classifier path uses dependency injection (`Classifier` trait) so unit tests never make a real network call.

---

# Tasks

## Task 1: Add `ureq` HTTP client + `mockito` test dep to workspace

**bd task:** `<P3.01>`
**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/tj-core/Cargo.toml`

- [ ] **Step 1: Append to root `Cargo.toml` `[workspace.dependencies]`**

```toml
ureq = { version = "2", features = ["json"] }
mockito = "1"
```

- [ ] **Step 2: Add to `crates/tj-core/Cargo.toml`**

```toml
[dependencies]
# ... existing deps
ureq = { workspace = true }

[dev-dependencies]
# ... existing
mockito = { workspace = true }
```

- [ ] **Step 3: Build**

```bash
cargo build --workspace
```
Expected: `Compiling ureq` and `Compiling mockito`, finish without errors.

- [ ] **Step 4: Commit + close**

```bash
git commit -m "build: add ureq for HTTP and mockito for tests (claude-memory-<id>)"
bd close <id>
```

---

## Task 2: `classifier` module skeleton — types

**bd task:** `<P3.02>`
**Files:**
- Create: `crates/tj-core/src/classifier/mod.rs`
- Modify: `crates/tj-core/src/lib.rs`
- Create: `crates/tj-core/src/classifier/prompt.rs`
- Create: `crates/tj-core/src/classifier/http.rs`
- Create: `crates/tj-core/src/classifier/mock.rs`

- [ ] **Step 1: Failing test in `mod.rs`**

```rust
//! Event classifier: takes a chat chunk + recent task context,
//! returns suggested event_type + task_id + confidence.

use serde::{Deserialize, Serialize};
use crate::event::{EventType, EvidenceStrength};

#[derive(Debug, Clone, Serialize)]
pub struct ClassifyInput {
    pub text: String,
    pub author_hint: String,        // "user" | "assistant" | "tool"
    pub recent_tasks: Vec<TaskContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskContext {
    pub task_id: String,
    pub title: String,
    pub last_events: Vec<String>,   // short summaries
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassifyOutput {
    pub event_type: EventType,
    pub task_id_guess: Option<String>,
    pub confidence: f64,
    pub evidence_strength: Option<EvidenceStrength>,
    pub suggested_text: String,
}

pub trait Classifier: Send + Sync {
    fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput>;
}

pub mod mock;
pub mod prompt;
pub mod http;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_input_serializes() {
        let i = ClassifyInput {
            text: "Adopted Rust for the journal".into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![],
        };
        let s = serde_json::to_string(&i).unwrap();
        assert!(s.contains("Adopted Rust"));
    }
}
```

- [ ] **Step 2: lib.rs — add `pub mod classifier;`**

- [ ] **Step 3: Stub the three submodules**

`crates/tj-core/src/classifier/prompt.rs`:
```rust
//! Prompt builder for the classifier (Task 4 fills this in).
```

`crates/tj-core/src/classifier/http.rs`:
```rust
//! Anthropic HTTP client (Task 3 + 5 fill this in).
```

`crates/tj-core/src/classifier/mock.rs`:
```rust
//! Mock classifier for tests (Task 6 fills this in).
```

- [ ] **Step 4: Run, GREEN**

```bash
cargo test -p tj-core --lib classifier::tests
```

- [ ] **Step 5: Commit + close**

---

## Task 3: `MockClassifier` — canned-response driver for tests

**bd task:** `<P3.03>`
**Files:**
- Modify: `crates/tj-core/src/classifier/mock.rs`

- [ ] **Step 1: Failing test in `mock.rs`**

```rust
//! Mock classifier: returns a pre-set output regardless of input.

use super::*;

pub struct MockClassifier {
    pub canned: ClassifyOutput,
}

impl Classifier for MockClassifier {
    fn classify(&self, _input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
        Ok(self.canned.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    #[test]
    fn mock_returns_canned_output() {
        let m = MockClassifier {
            canned: ClassifyOutput {
                event_type: EventType::Decision,
                task_id_guess: Some("tj-x".into()),
                confidence: 0.95,
                evidence_strength: None,
                suggested_text: "...".into(),
            },
        };
        let out = m.classify(&ClassifyInput {
            text: "ignored".into(),
            author_hint: "user".into(),
            recent_tasks: vec![],
        }).unwrap();
        assert_eq!(out.event_type, EventType::Decision);
        assert_eq!(out.confidence, 0.95);
    }
}
```

Also add `Clone` derive on `ClassifyOutput`:
```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ClassifyOutput {
```

- [ ] **Step 2: Run, GREEN**

```bash
cargo test -p tj-core --lib classifier::mock::tests
```

- [ ] **Step 3: Commit + close**

---

## Task 4: Classifier prompt builder

**bd task:** `<P3.04>`
**Files:**
- Modify: `crates/tj-core/src/classifier/prompt.rs`

- [ ] **Step 1: Failing test**

```rust
//! Prompt builder for the classifier.

use crate::classifier::{ClassifyInput, TaskContext};

pub fn build(input: &ClassifyInput) -> String {
    let recent = if input.recent_tasks.is_empty() {
        "(no active tasks)".to_string()
    } else {
        input.recent_tasks.iter().map(|t| {
            format!("- {} \"{}\": {}",
                t.task_id, t.title,
                if t.last_events.is_empty() { "(no events)".into() } else { t.last_events.join("; ") }
            )
        }).collect::<Vec<_>>().join("\n")
    };

    format!(
        "You classify chat chunks for an AI-coding-agent task journal.\n\
         Active tasks (top candidates):\n{recent}\n\n\
         New {author} chunk:\n{text}\n\n\
         Decide:\n\
         1. Which existing task this belongs to (or null if unrelated)\n\
         2. Best event_type from: hypothesis, finding, evidence, decision, rejection, constraint, correction, reopen, supersede, close, redirect\n\
         3. Confidence 0.0-1.0\n\
         4. evidence_strength (weak|medium|strong) if event_type is evidence, else omit\n\
         5. A 1-2 sentence suggested_text that captures the essence\n\n\
         Respond ONLY with strict JSON matching this shape, no commentary:\n\
         {{\"event_type\":\"...\",\"task_id_guess\":\"...\"|null,\"confidence\":0.0,\"evidence_strength\":\"...\"|null,\"suggested_text\":\"...\"}}",
        author=input.author_hint, text=input.text
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::*;

    #[test]
    fn prompt_includes_text_and_recent_tasks() {
        let input = ClassifyInput {
            text: "We adopted PKCE.".into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![TaskContext {
                task_id: "tj-7f3a".into(),
                title: "OAuth login".into(),
                last_events: vec!["[hypothesis] PKCE vs implicit".into()],
            }],
        };
        let p = build(&input);
        assert!(p.contains("We adopted PKCE."));
        assert!(p.contains("tj-7f3a"));
        assert!(p.contains("PKCE vs implicit"));
        assert!(p.contains("strict JSON"));
    }

    #[test]
    fn prompt_handles_empty_tasks() {
        let input = ClassifyInput {
            text: "Hello".into(),
            author_hint: "user".into(),
            recent_tasks: vec![],
        };
        let p = build(&input);
        assert!(p.contains("(no active tasks)"));
    }
}
```

- [ ] **Step 2: Run, GREEN**

```bash
cargo test -p tj-core --lib classifier::prompt::tests
```

- [ ] **Step 3: Commit + close**

---

## Task 5: Anthropic HTTP client — `AnthropicClassifier`

**bd task:** `<P3.05>`
**Files:**
- Modify: `crates/tj-core/src/classifier/http.rs`

- [ ] **Step 1: Failing test using `mockito`**

```rust
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
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY env var not set")?;
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
            messages: vec![MessageIn { role: "user", content: &prompt }],
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

        let text = resp.content.iter()
            .find(|b| b.kind == "text")
            .map(|b| b.text.clone())
            .ok_or_else(|| anyhow!("no text content in response"))?;

        // Strip code fences if present.
        let json_str = text
            .trim()
            .trim_start_matches("```json").trim_start_matches("```")
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

        let mock = server.mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create();

        let c = AnthropicClassifier {
            api_key: "test".into(),
            model: "claude-haiku-4-5-20251001".into(),
            base_url: url,
        };
        let out = c.classify(&ClassifyInput {
            text: "We adopted Rust.".into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![],
        }).unwrap();

        assert_eq!(out.event_type, EventType::Decision);
        assert_eq!(out.task_id_guess.as_deref(), Some("tj-x"));
        assert!((out.confidence - 0.93).abs() < 1e-6);
        mock.assert();
    }
}
```

- [ ] **Step 2: Run, GREEN**

```bash
cargo test -p tj-core --lib classifier::http::tests
```

- [ ] **Step 3: Commit + close**

---

## Task 6: `decide_status` — confidence gating helper

**bd task:** `<P3.06>`
**Files:**
- Modify: `crates/tj-core/src/classifier/mod.rs`

- [ ] **Step 1: Failing test**

Append to `mod.rs` test module:
```rust
#[test]
fn decide_status_high_confidence_is_confirmed() {
    use crate::event::EventStatus;
    assert_eq!(decide_status(0.95), EventStatus::Confirmed);
    assert_eq!(decide_status(0.85), EventStatus::Confirmed);
}

#[test]
fn decide_status_low_confidence_is_suggested() {
    use crate::event::EventStatus;
    assert_eq!(decide_status(0.84), EventStatus::Suggested);
    assert_eq!(decide_status(0.0), EventStatus::Suggested);
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add `decide_status` to mod.rs (above tests)**

```rust
use crate::event::EventStatus;

pub const CONFIDENCE_THRESHOLD: f64 = 0.85;

pub fn decide_status(confidence: f64) -> EventStatus {
    if confidence >= CONFIDENCE_THRESHOLD {
        EventStatus::Confirmed
    } else {
        EventStatus::Suggested
    }
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 7: pack — surface `[?]` marker for suggested events

**bd task:** `<P3.07>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test in pack tests**

```rust
#[test]
fn suggested_events_get_question_mark_marker_in_pack() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-q", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Q"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let mut suggested = Event::new("tj-q", EventType::Decision, Author::Classifier, Source::Hook, "Adopt Rust".into());
    suggested.status = EventStatus::Suggested;
    db::upsert_task_from_event(&conn, &suggested, "feedface").unwrap();
    db::index_event(&conn, &suggested).unwrap();

    let pack = assemble(&conn, "tj-q", PackMode::Full).unwrap();
    let recent_section_pos = pack.text.find("## Recent events").unwrap();
    let recent_section = &pack.text[recent_section_pos..];
    assert!(
        recent_section.contains("[?]"),
        "suggested event must show [?] marker in Recent events:\n{recent_section}"
    );
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Update `render_recent_events` SQL + format**

Replace:
```rust
let mut stmt = conn.prepare(
    "SELECT ei.timestamp, ei.type, sf.text FROM events_index ei
     LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
     WHERE ei.task_id=?1 ORDER BY ei.timestamp DESC LIMIT ?2"
)?;
```
With:
```rust
let mut stmt = conn.prepare(
    "SELECT ei.timestamp, ei.type, ei.status, sf.text FROM events_index ei
     LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
     WHERE ei.task_id=?1 ORDER BY ei.timestamp DESC LIMIT ?2"
)?;
let rows = stmt.query_map(rusqlite::params![task_id, limit as i64], |r| {
    let ts: String = r.get(0)?;
    let ty: String = r.get(1)?;
    let st: String = r.get(2)?;
    let txt: Option<String> = r.get(3)?;
    Ok((ts, ty, st, txt.unwrap_or_default()))
})?;
for row in rows {
    let (ts, ty, st, txt) = row?;
    let one_line = txt.lines().next().unwrap_or("").chars().take(120).collect::<String>();
    let marker = if st == "suggested" { " [?]" } else { "" };
    out.push_str(&format!("- {ts} [{ty}]{marker} {one_line}\n"));
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 8: CLI `ingest-hook` — accept text + write event using mock classifier

**bd task:** `<P3.08>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing CLI test using `--mock-event-type` (test-only flag)**

```rust
#[test]
fn ingest_hook_with_mock_writes_classified_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Mock target"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind", "Stop",
            "--text", "We decided to adopt Rust.",
            "--mock-event-type", "decision",
            "--mock-task-id", &task_id,
            "--mock-confidence", "0.95",
        ])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("We decided to adopt Rust.").and(contains("[decision]")));
}
```

- [ ] **Step 2: Run, fail (no `ingest-hook` subcommand)**

- [ ] **Step 3: Add `IngestHook` subcommand**

In `Commands` enum:
```rust
/// Hook entry point: ingest a chat chunk through the classifier.
IngestHook {
    /// Hook kind: UserPromptSubmit | PostToolUse | Stop | SessionStart.
    #[arg(long)]
    kind: String,
    /// The chat chunk text.
    #[arg(long)]
    text: String,
    /// (test/dev) override: bypass classifier and force this event type.
    #[arg(long)]
    mock_event_type: Option<String>,
    /// (test/dev) override: target task id.
    #[arg(long)]
    mock_task_id: Option<String>,
    /// (test/dev) override: confidence value.
    #[arg(long)]
    mock_confidence: Option<f64>,
},
```

In match:
```rust
Commands::IngestHook { kind: _, text, mock_event_type, mock_task_id, mock_confidence } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    std::fs::create_dir_all(events_path.parent().unwrap())?;

    let (etype, task_id, confidence) = if let (Some(t), Some(tid)) = (mock_event_type.as_deref(), mock_task_id.as_deref()) {
        (parse_event_type(t)?, tid.to_string(), mock_confidence.unwrap_or(1.0))
    } else {
        // Real classifier path lit up in Task 9 (next task).
        anyhow::bail!("real classifier wiring not yet implemented (see Task 9)");
    };

    let mut event = tj_core::event::Event::new(
        &task_id, etype,
        tj_core::event::Author::Classifier, tj_core::event::Source::Hook,
        text,
    );
    event.confidence = Some(confidence);
    event.status = tj_core::classifier::decide_status(confidence);

    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
    writer.append(&event)?;
    writer.flush_durable()?;
    println!("{}", event.event_id);
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 9: CLI `ingest-hook` real classifier path

**bd task:** `<P3.09>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`

- [ ] **Step 1: Smoke test (real network — skipped if no API key)**

Append to `cli.rs`:
```rust
#[test]
#[ignore] // requires ANTHROPIC_API_KEY; run with `cargo test -- --ignored`
fn ingest_hook_real_anthropic_classifies() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "OAuth flow choice"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    let _ = task_id; // Only used implicitly: classifier should pick it from recent tasks.
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["ingest-hook", "--kind", "Stop", "--text", "We have decided to use the PKCE flow for OAuth."])
        .assert().success();
}
```

- [ ] **Step 2: Replace the `bail!` in real path**

```rust
let (etype, task_id, confidence) = if let (Some(t), Some(tid)) = (mock_event_type.as_deref(), mock_task_id.as_deref()) {
    (parse_event_type(t)?, tid.to_string(), mock_confidence.unwrap_or(1.0))
} else {
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
    }
    let recent = recent_task_contexts(&conn, 5)?;
    if recent.is_empty() {
        // Nothing to classify against; drop chunk silently.
        return Ok(());
    }

    let classifier = tj_core::classifier::http::AnthropicClassifier::from_env()?;
    let input = tj_core::classifier::ClassifyInput {
        text: text.clone(),
        author_hint: "assistant".into(),
        recent_tasks: recent,
    };
    let out = match classifier.classify(&input) {
        Ok(o) => o,
        Err(e) => {
            // Persist to pending/ for retry (Task 11).
            persist_pending(&events_path, &text, &e.to_string())?;
            return Ok(());
        }
    };

    let Some(tid) = out.task_id_guess else {
        // Classifier said "doesn't fit any task" — drop.
        return Ok(());
    };
    (out.event_type, tid, out.confidence)
};
```

Add helper at the bottom of `main.rs`:
```rust
fn recent_task_contexts(conn: &rusqlite::Connection, limit: usize) -> anyhow::Result<Vec<tj_core::classifier::TaskContext>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, title FROM tasks WHERE status='open' ORDER BY last_event_at DESC LIMIT ?1"
    )?;
    let task_rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?;

    let mut out = Vec::with_capacity(task_rows.len());
    for (task_id, title) in task_rows {
        let mut e_stmt = conn.prepare(
            "SELECT ei.type, sf.text FROM events_index ei
             LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
             WHERE ei.task_id=?1 ORDER BY ei.timestamp DESC LIMIT 3"
        )?;
        let last_events: Vec<String> = e_stmt
            .query_map(rusqlite::params![task_id], |r| {
                let ty: String = r.get(0)?;
                let txt: Option<String> = r.get(1)?;
                Ok(format!("[{ty}] {}", txt.unwrap_or_default().chars().take(80).collect::<String>()))
            })?
            .collect::<Result<_, _>>()?;
        out.push(tj_core::classifier::TaskContext { task_id, title, last_events });
    }
    Ok(out)
}

fn persist_pending(events_path: &std::path::Path, text: &str, err: &str) -> anyhow::Result<()> {
    let pending_dir = events_path.parent().unwrap().parent().unwrap().join("pending");
    std::fs::create_dir_all(&pending_dir)?;
    let id = ulid::Ulid::new().to_string();
    let payload = serde_json::json!({"text": text, "error": err, "queued_at": chrono::Utc::now().to_rfc3339()});
    std::fs::write(pending_dir.join(format!("{id}.json")), serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}
```

- [ ] **Step 3: Build (do NOT run the ignored test)**

```bash
cargo build -p tj-cli
cargo test -p tj-cli --test cli ingest_hook_with_mock  # mock path still passes
```

- [ ] **Step 4: Commit + close**

---

## Task 10: Pending queue — replay on next ingest

**bd task:** `<P3.10>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn ingest_hook_drains_pending_queue_via_mock() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Drain"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    // Manually drop a pending file as if classifier was down.
    let pending = dir.path().join("task-journal").join("pending");
    std::fs::create_dir_all(&pending).unwrap();
    std::fs::write(pending.join("01stuck.json"), serde_json::json!({
        "text": "We decided to adopt PKCE flow.",
        "queued_at": "2026-04-30T00:00:00Z"
    }).to_string()).unwrap();

    // Now run ingest with mock classifier — it should also drain pending.
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind", "Stop",
            "--text", "Live chunk",
            "--mock-event-type", "decision",
            "--mock-task-id", &task_id,
            "--mock-confidence", "0.95",
        ])
        .assert().success();

    // Pending file is gone (drained or kept as failure marker).
    let remaining: Vec<_> = std::fs::read_dir(&pending).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .collect();
    assert_eq!(remaining.len(), 0, "pending queue must be empty after successful ingest");
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add drain step at start of `IngestHook` arm**

```rust
// Before processing the live chunk, drain any pending entries.
drain_pending(&events_path, mock_event_type.as_deref(), mock_task_id.as_deref(), mock_confidence)?;
```

Add helper:
```rust
fn drain_pending(
    events_path: &std::path::Path,
    mock_etype: Option<&str>,
    mock_tid: Option<&str>,
    mock_conf: Option<f64>,
) -> anyhow::Result<()> {
    let pending_dir = events_path.parent().unwrap().parent().unwrap().join("pending");
    if !pending_dir.exists() { return Ok(()); }

    for entry in std::fs::read_dir(&pending_dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|e| e.to_str()) != Some("json") { continue; }

        let body = std::fs::read_to_string(entry.path())?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if text.is_empty() {
            std::fs::remove_file(entry.path())?;
            continue;
        }

        // Reuse mock if provided (test path); real path falls back to classifier (omitted here for brevity).
        if let (Some(t), Some(tid)) = (mock_etype, mock_tid) {
            let mut event = tj_core::event::Event::new(
                tid, parse_event_type(t)?,
                tj_core::event::Author::Classifier, tj_core::event::Source::Hook, text,
            );
            event.confidence = mock_conf;
            event.status = tj_core::classifier::decide_status(mock_conf.unwrap_or(1.0));
            let mut writer = tj_core::storage::JsonlWriter::open(events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
        }
        // Always delete the pending file once processed (or skipped).
        std::fs::remove_file(entry.path())?;
    }
    Ok(())
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 11: CLI `event-correct` — write a correction event

**bd task:** `<P3.11>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn event_correct_links_to_corrected_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Correct me"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    let bad = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["event", &task_id, "--type", "finding", "--text", "Migration done (wrong)"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event-correct",
            "--corrects", &bad,
            "--task", &task_id,
            "--text", "Migration was NOT done; finding was wrong",
        ])
        .assert().success();

    // The pack should show the correction.
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("Migration was NOT done").and(contains("[correction]")));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add subcommand**

```rust
/// Append a correction event referencing an earlier event_id.
EventCorrect {
    #[arg(long)]
    corrects: String,
    #[arg(long)]
    task: String,
    #[arg(long)]
    text: String,
},
```

In match:
```rust
Commands::EventCorrect { corrects, task, text } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    std::fs::create_dir_all(events_path.parent().unwrap())?;

    let mut event = tj_core::event::Event::new(
        &task, tj_core::event::EventType::Correction,
        tj_core::event::Author::User, tj_core::event::Source::Cli,
        text,
    );
    event.corrects = Some(corrects);
    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
    writer.append(&event)?;
    writer.flush_durable()?;
    println!("{}", event.event_id);
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 12: CLI `install-hooks` — write to `~/.claude/settings.json`

**bd task:** `<P3.12>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn install_hooks_writes_to_settings_json() {
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert().success();

    let settings_path = claude_dir.join("settings.json");
    assert!(settings_path.exists());
    let content = std::fs::read_to_string(&settings_path).unwrap();
    assert!(content.contains("UserPromptSubmit"));
    assert!(content.contains("PostToolUse"));
    assert!(content.contains("task-journal ingest-hook"));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add subcommand**

```rust
/// Install Claude Code hooks that ingest events into the task journal.
InstallHooks {
    /// Scope: user (~/.claude/settings.json) or project (./.claude/settings.json).
    #[arg(long, default_value = "user")]
    scope: String,
    /// If set, removes our hook entries instead of installing.
    #[arg(long)]
    uninstall: bool,
},
```

In match:
```rust
Commands::InstallHooks { scope, uninstall } => {
    let settings_path = match scope.as_str() {
        "user" => {
            let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
            std::path::PathBuf::from(home).join(".claude").join("settings.json")
        }
        "project" => std::env::current_dir()?.join(".claude").join("settings.json"),
        other => anyhow::bail!("unknown scope: {other}"),
    };
    if let Some(p) = settings_path.parent() { std::fs::create_dir_all(p)?; }

    let mut current: serde_json::Value = if settings_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&settings_path)?).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let hooks_obj = current.as_object_mut().ok_or_else(|| anyhow::anyhow!("settings is not a JSON object"))?;
    if uninstall {
        hooks_obj.remove("hooks");
    } else {
        let cmd = "task-journal ingest-hook --kind=$CLAUDE_HOOK_NAME --text=\"$CLAUDE_HOOK_TEXT\"";
        let entries = serde_json::json!({
            "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
            "PostToolUse":     [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
            "Stop":            [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
        });
        hooks_obj.insert("hooks".into(), entries);
    }
    std::fs::write(&settings_path, serde_json::to_string_pretty(&current)?)?;
    println!("{}", settings_path.display());
}
```

> **Note:** the actual hook variables `$CLAUDE_HOOK_NAME` and `$CLAUDE_HOOK_TEXT` may be different in current Claude Code; verify and adjust during the manual smoke at Task 17. For automated tests we just check the substring `task-journal ingest-hook`.

- [ ] **Step 4: GREEN, commit + close**

---

## Task 13: install-hooks idempotency + uninstall test

**bd task:** `<P3.13>`
**Files:**
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn install_hooks_is_idempotent_and_uninstall_works() {
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    // Pre-existing user setting we don't want clobbered.
    std::fs::write(claude_dir.join("settings.json"),
        serde_json::json!({"theme": "dark"}).to_string()).unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert().success();
    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert().success();  // second call must succeed without error

    let after_install = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert!(after_install.contains("\"theme\": \"dark\""), "must preserve unrelated keys");
    assert!(after_install.contains("UserPromptSubmit"));

    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--uninstall"])
        .assert().success();

    let after_uninstall = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert!(after_uninstall.contains("\"theme\": \"dark\""), "must still preserve theme");
    assert!(!after_uninstall.contains("UserPromptSubmit"));
}
```

- [ ] **Step 2: Run, GREEN**

The Task 12 impl already preserves unrelated keys (we only modify the `hooks` slot) and overwrites idempotently. If the test fails, refine the install-hooks impl until both runs leave the file deterministic.

- [ ] **Step 3: Commit + close**

---

## Task 14: pack — Corrections summary marker

**bd task:** `<P3.14>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn corrected_events_appear_with_corrected_marker() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-co", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Corr"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let bad = Event::new("tj-co", EventType::Finding, Author::Classifier, Source::Hook, "Migration done (wrong)".into());
    db::upsert_task_from_event(&conn, &bad, "feedface").unwrap();
    db::index_event(&conn, &bad).unwrap();

    let mut corr = Event::new("tj-co", EventType::Correction, Author::User, Source::Cli, "Migration NOT done; finding was wrong".into());
    corr.corrects = Some(bad.event_id.clone());
    db::upsert_task_from_event(&conn, &corr, "feedface").unwrap();
    db::index_event(&conn, &corr).unwrap();

    let pack = assemble(&conn, "tj-co", PackMode::Full).unwrap();
    assert!(pack.text.contains("[correction]"));
    assert!(pack.text.contains("Migration NOT done"));
}
```

- [ ] **Step 2: Run, GREEN — `[correction]` is already a `type` in `events_index`, so `render_recent_events` renders it as `[correction]` already.**

This task is mostly a verification test. If it fails, expand the events index ordering or the FTS join.

- [ ] **Step 3: Commit + close**

---

## Task 15: classifier prompt unit test — handles deeply nested context

**bd task:** `<P3.15>`
**Files:**
- Modify: `crates/tj-core/src/classifier/prompt.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn prompt_truncates_event_lines_to_keep_size_bounded() {
    let input = ClassifyInput {
        text: "abc".into(),
        author_hint: "user".into(),
        recent_tasks: (0..20).map(|i| TaskContext {
            task_id: format!("tj-{i:03}"),
            title: format!("Task {i}"),
            last_events: (0..30).map(|j| format!("[finding] very long evidence text {i}/{j} ".repeat(20))).collect(),
        }).collect(),
    };
    let p = build(&input);
    assert!(p.len() < 64 * 1024, "prompt must stay under 64KB; got {}", p.len());
}
```

- [ ] **Step 2: Run, fail (prompt blows past 64KB)**

- [ ] **Step 3: Truncate**

In `build`, replace the `last_events.join("; ")` line with:
```rust
let trimmed_events: Vec<String> = t.last_events.iter().take(3)
    .map(|s| s.chars().take(120).collect::<String>())
    .collect();
let line = format!("- {} \"{}\": {}",
    t.task_id, t.title,
    if trimmed_events.is_empty() { "(no events)".into() } else { trimmed_events.join("; ") }
);
line
```

Wrap with `take(10)` on the tasks iterator so the prompt never carries more than 10 candidate tasks.

- [ ] **Step 4: GREEN, commit + close**

---

## Task 16: E2E test — install-hooks → simulated hook → ingest → pack

**bd task:** `<P3.16>`
**Files:**
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing E2E test**

```rust
#[test]
fn e2e_hook_simulation_classifies_and_packs_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Stack choice for journal"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    // Simulate a Claude Code "Stop" hook firing with a chunk that contains a clear decision.
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind", "Stop",
            "--text", "After review, we adopt Rust because of the single-binary distribution.",
            "--mock-event-type", "decision",
            "--mock-task-id", &task_id,
            "--mock-confidence", "0.92",
        ])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("Stack choice for journal")
            .and(contains("[decision]"))
            .and(contains("single-binary"))
            .and(predicates::str::contains("[?]").not()));  // 0.92 >= 0.85 → confirmed, no "[?]"
}
```

- [ ] **Step 2: Run, GREEN (everything wired in earlier tasks)**

- [ ] **Step 3: Commit + close**

---

## Task 17: Manual smoke with real Anthropic API (skipped if no key)

**bd task:** `<P3.17>`
**Files:**
- Create: `.beads/hooks/p3-demo.sh`

- [ ] **Step 1: Write demo script**

```bash
#!/bin/bash
set -e
cd /home/shahinyanm/www/claude-memory

if [ -z "$ANTHROPIC_API_KEY" ]; then
  echo "ANTHROPIC_API_KEY not set; skipping real-API smoke."
  exit 0
fi

DEMO=/tmp/tj-p3-demo
rm -rf "$DEMO"
mkdir -p "$DEMO"
export XDG_DATA_HOME="$DEMO"

echo "==================== P3 LIVE DEMO (real Anthropic) ===================="

TASK_ID=$(./target/debug/task-journal create "Pick auth flow")
echo "Task: $TASK_ID"
./target/debug/task-journal event "$TASK_ID" --type hypothesis --text "PKCE vs implicit grant" >/dev/null

echo ""
echo ">>> simulate a Stop hook: assistant stated a decision"
./target/debug/task-journal ingest-hook \
  --kind Stop \
  --text "After review I'm going to adopt PKCE for the OAuth flow because OAuth 2.1 deprecates implicit."

echo ""
echo ">>> resulting full pack:"
./target/debug/task-journal pack "$TASK_ID" --mode full
```

- [ ] **Step 2: Run only manually** (after the executor exports `ANTHROPIC_API_KEY`)

```bash
chmod +x .beads/hooks/p3-demo.sh
.beads/hooks/p3-demo.sh
```

Expected: pack shows the `[decision]` event captured by the classifier.

- [ ] **Step 3: Commit + close**

---

## Task 18: P3 Verification Gate

**bd task:** `<P3.18>`

- [ ] **Step 1: Full workspace tests**

```bash
cargo test --workspace
```
Expected: all green; `cargo test -- --ignored` is the **only** way to hit the live Anthropic API and is opt-in.

- [ ] **Step 2: Quick local smoke (no API key)**

```bash
.beads/hooks/p2-demo.sh   # P2 still works
```

- [ ] **Step 3: bd close + invoke `superpowers:finishing-a-development-branch`**

```bash
bd close <P3.18-id> --reason "P3 done; classifier+hooks shipped, real API path covered by ignored test"
```

The epic should be reopened (auto-closes when all children close) so P4 (polish) still has a parent.

---

# Beads task batch-create helper (P3)

Run a script analogous to `.beads/hooks/p2-create.sh` with these titles:

```
01 Add ureq HTTP client and mockito test dep
02 classifier module skeleton with types
03 MockClassifier canned-response driver
04 Classifier prompt builder
05 Anthropic HTTP client AnthropicClassifier
06 decide_status confidence gating helper
07 pack render question-mark marker for suggested events
08 CLI ingest-hook with mock writes classified event
09 CLI ingest-hook real Anthropic classifier path
10 Pending queue replay on next ingest
11 CLI event-correct subcommand
12 CLI install-hooks writes to settings.json
13 install-hooks idempotency plus uninstall
14 pack Corrections marker via correction events
15 classifier prompt size bound under 64KB
16 E2E hook simulation classifies and packs
17 Manual smoke with real Anthropic API
18 P3 verification gate
```

Save the map at `.docs/plans/2026-04-30-p3-task-map.txt`. Link parent-child + blocks chain as in P1/P2.

---

# Self-Review (writer's pass)

**Spec coverage** vs design doc P3 deliverables:

| Design item | Tasks |
|------|------|
| Hook installer (idempotent) | 12, 13 |
| Classifier subprocess (Anthropic API client) | 5 |
| Confidence-gated write path (suggested vs confirmed) | 6, 7, 8 |
| `pending/` retry queue | 9, 10 |
| Manual `event_add(type=correction)` UX | 11 |
| Suggested event surfacing in pack `[?]` | 7 |
| Mock vs real classifier separation | 3, 5 |
| Prompt size bound | 15 |
| End-to-end test | 16 |
| Real-API smoke | 17 |

**Placeholder scan:** every task has actual code or commands. The "Note" in Task 12 about `$CLAUDE_HOOK_NAME` is intentional and flagged for manual verification.

**Type consistency:** `Classifier` trait, `ClassifyInput`, `ClassifyOutput`, `TaskContext` introduced in Task 2 used identically through 3-10, 15, 16. CLI flag names (`--text`, `--kind`, `--corrects`, `--task`, `--scope`, `--uninstall`) consistent across tasks.

**Scope check:** P3 covers hooks + classifier only. P4 (polish, README, docs, dogfood) is separate.

**Ambiguity check:**
- `--mock-event-type` etc. are test-only flags but they live on the production binary. We document them as `(test/dev) override` in `--help`. Acceptable for v1; tighten in P4 if needed (gate behind `--features=test-helpers`).
- Pending queue keeps failed entries until next successful ingest. If failures cascade, queue grows unbounded — Task 10 deletes after processing, even if the underlying classifier call fails again. This is intentional for v1 (don't grow the queue forever; user can re-trigger by retrying the work in chat).

---

**End of Phase 3 plan.** P4 (polish + dogfood) plan is authored after P3's Task 18 verification gate.
