# Task Journal v1 — Phase 4 (Polish + Dogfood) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task.

**Goal:** Take Task Journal from "feature complete after P3" to "ready to dogfood with a real Claude Code session". Add cross-project search, token-budget truncation for `task_pack`, classifier telemetry, hook resilience, test-flag gating, and user-facing docs (README + INSTALL).

**Architecture:** No new modules — extend existing ones. `tj-core::pack` gets truncation logic, `tj-core::classifier` writes telemetry to `metrics/<project_hash>.jsonl`, `tj-core::db` exposes a cross-project query, CLI gets `--all-projects` flag and a `stats` subcommand. Test-only flags (`--mock-*`) move behind a `test-helpers` Cargo feature so production binaries no longer expose them.

**Tech Stack:** Same as P1+P2+P3. No new crates.

**Working directory:** `/home/shahinyanm/www/claude-memory` inside WSL Ubuntu. Wrap shell calls as `wsl -d Ubuntu -- bash -c 'export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; cd /home/shahinyanm/www/claude-memory && <command>'`.

**Beads tracking:** One issue per Task, parent-child to epic `claude-memory-d36`, blocks chain.

**Spec source of truth:** `.docs/plans/2026-04-29-task-journal-v1-design.md` (P4 = Phase 4 in §2 Q9).

---

## Pre-flight

1. `cargo test --workspace` → 60 green from P3.
2. `.beads/hooks/p2-demo.sh` and `.beads/hooks/p3-mock-demo.sh` still pass.

---

## File structure (after Task 12)

```
crates/
├── tj-core/
│   └── src/
│       ├── pack.rs                ← + token-budget truncation
│       ├── db.rs                  ← + list_all_projects()
│       └── classifier/
│           └── telemetry.rs       ← NEW: write classification stats
├── tj-cli/
│   ├── Cargo.toml                 ← + [features] test-helpers
│   └── src/main.rs                ← + search --all-projects, stats subcmd, gate --mock-*
└── tj-mcp/                        ← unchanged

(root)
├── README.md                      ← NEW: install + getting started
├── INSTALL.md                     ← NEW: hook setup walkthrough
└── .beads/hooks/
    └── p4-demo.sh                 ← NEW: full P1+P2+P3+P4 smoke
```

---

# Tasks

## Task 1: `db::list_all_projects` — enumerate per-project SQLite files

**bd task:** `<P4.01>`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn list_all_projects_returns_hashes_from_state_dir() {
    use std::fs::File;
    let d = TempDir::new().unwrap();
    let state_dir = d.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    File::create(state_dir.join("aaaa1111aaaa1111.sqlite")).unwrap();
    File::create(state_dir.join("bbbb2222bbbb2222.sqlite")).unwrap();
    File::create(state_dir.join("not-a-project.txt")).unwrap();

    let mut hashes = list_all_projects(&state_dir).unwrap();
    hashes.sort();
    assert_eq!(hashes, vec!["aaaa1111aaaa1111", "bbbb2222bbbb2222"]);
}
```

- [ ] **Step 2: Implement**

```rust
pub fn list_all_projects(state_dir: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    let dir = state_dir.as_ref();
    if !dir.exists() { return Ok(vec![]); }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("sqlite") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 3: GREEN, commit, close**

```bash
git commit -m "feat(core): list_all_projects helper for cross-project queries (claude-memory-<id>)"
bd close <id>
```

---

## Task 2: CLI `search --all-projects`

**bd task:** `<P4.02>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn search_all_projects_finds_match_in_other_project_hash() {
    let dir = assert_fs::TempDir::new().unwrap();

    // Synthesize TWO project_hash directories with FTS-bearing events.
    // Easier path: pre-build state files manually.
    let state = dir.path().join("task-journal").join("state");
    std::fs::create_dir_all(&state).unwrap();

    use rusqlite::Connection;
    for hash in ["aaaa1111aaaa1111", "bbbb2222bbbb2222"] {
        let db_path = state.join(format!("{hash}.sqlite"));
        let conn = tj_core::db::open(&db_path).unwrap();
        let mut e = tj_core::event::Event::new(
            format!("tj-{}", &hash[..6]),
            tj_core::event::EventType::Open,
            tj_core::event::Author::User,
            tj_core::event::Source::Cli,
            format!("Marker {hash}"),
        );
        e.meta = serde_json::json!({"title": format!("Title {hash}")});
        tj_core::db::upsert_task_from_event(&conn, &e, hash).unwrap();
        tj_core::db::index_event(&conn, &e).unwrap();
    }

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "Marker", "--all-projects"])
        .assert().success()
        .stdout(contains("aaaa1111").and(contains("bbbb2222")));
}
```

- [ ] **Step 2: Add `--all-projects` flag**

```rust
Search {
    query: String,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// Search across all projects on this machine (not just current cwd).
    #[arg(long)]
    all_projects: bool,
},
```

In match, branch on `all_projects`:

```rust
Commands::Search { query, limit, all_projects } => {
    if all_projects {
        let state_dir = tj_core::paths::state_dir()?;
        let hashes = tj_core::db::list_all_projects(&state_dir)?;
        for hash in hashes {
            let path = state_dir.join(format!("{hash}.sqlite"));
            let conn = match rusqlite::Connection::open(&path) { Ok(c) => c, Err(_) => continue };
            let mut stmt = match conn.prepare(
                "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT ?2"
            ) { Ok(s) => s, Err(_) => continue };
            if let Ok(rows) = stmt.query_map(rusqlite::params![&query, limit as i64], |r| r.get::<_, String>(0)) {
                for id in rows.flatten() {
                    println!("{hash}\t{id}");
                }
            }
        }
    } else {
        // existing single-project path stays here
    }
}
```

- [ ] **Step 3: GREEN, commit, close**

---

## Task 3: Pack token-budget truncation (full mode caps ~10KB)

**bd task:** `<P4.03>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn full_mode_truncates_when_exceeding_budget() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-big", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Big"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    // Add 100 evidence events with long text.
    for i in 0..100 {
        let ev = Event::new("tj-big", EventType::Evidence, Author::Agent, Source::Chat,
            format!("Evidence #{i}: {}", "lorem ipsum ".repeat(50)));
        db::upsert_task_from_event(&conn, &ev, "feedface").unwrap();
        db::index_event(&conn, &ev).unwrap();
    }

    let pack = assemble(&conn, "tj-big", PackMode::Full).unwrap();
    assert!(pack.text.len() <= 12 * 1024, "pack must stay under ~12KB; got {} bytes", pack.text.len());
}
```

- [ ] **Step 2: Add truncation step at end of `assemble`**

Right before the `INSERT OR REPLACE INTO task_pack_cache` write:

```rust
const FULL_BUDGET: usize = 10 * 1024;
const COMPACT_BUDGET: usize = 2 * 1024;

let budget = match mode { PackMode::Full => FULL_BUDGET, PackMode::Compact => COMPACT_BUDGET };
if text.len() > budget {
    // Truncate at the last full line within budget; append marker.
    let cutoff = text[..budget].rfind('\n').unwrap_or(budget);
    text.truncate(cutoff);
    text.push_str("\n\n_(truncated to fit pack budget)_\n");
}
```

- [ ] **Step 3: GREEN, commit, close**

---

## Task 4: Pack metadata `truncated` field

**bd task:** `<P4.04>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn truncated_metadata_flag_set_when_pack_truncated() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-tt", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Tt"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();
    for i in 0..200 {
        let ev = Event::new("tj-tt", EventType::Evidence, Author::Agent, Source::Chat,
            format!("Big evidence #{i}: {}", "x".repeat(200)));
        db::upsert_task_from_event(&conn, &ev, "feedface").unwrap();
        db::index_event(&conn, &ev).unwrap();
    }
    let pack = assemble(&conn, "tj-tt", PackMode::Full).unwrap();
    assert!(pack.metadata.truncated);
}
```

- [ ] **Step 2: Add `truncated: bool` to `PackMetadata`**

```rust
#[derive(Debug, Clone, Serialize)]
pub struct PackMetadata {
    pub generated_at: String,
    pub source_event_count: usize,
    pub cache_hit: bool,
    pub truncated: bool,
}
```

In `assemble`, track whether the truncation branch fired and pass through to metadata. Update the cache-hit branch to read/write the flag too (extend `task_pack_cache` schema or store inline in text — simpler: re-derive via `len > budget` check on read).

For simplicity: just compute `truncated = original_text.len() > budget` after building, add to metadata, also persist via cache (don't bother adding a column — recompute on read since cached `text` is already truncated form).

```rust
let truncated = text.len() > budget;
if truncated {
    let cutoff = text[..budget].rfind('\n').unwrap_or(budget);
    text.truncate(cutoff);
    text.push_str("\n\n_(truncated to fit pack budget)_\n");
}
// ... rest, then:
PackMetadata { generated_at, source_event_count: event_count, cache_hit: false, truncated }
```

For cache-hit path: detect via `text.contains("_(truncated to fit pack budget)_")` or store in a separate column. Simpler heuristic check is fine for v1.

- [ ] **Step 3: GREEN, commit, close**

---

## Task 5: Classifier telemetry — write to `metrics/<project_hash>.jsonl`

**bd task:** `<P4.05>`
**Files:**
- Create: `crates/tj-core/src/classifier/telemetry.rs`
- Modify: `crates/tj-core/src/classifier/mod.rs`
- Modify: `crates/tj-core/src/paths.rs`

- [ ] **Step 1: Failing test in `telemetry.rs`**

```rust
//! Append-only classifier telemetry: one JSONL line per classification call.

use serde::{Deserialize, Serialize};
use anyhow::Context;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryRecord {
    pub timestamp: String,
    pub project_hash: String,
    pub task_id_guess: Option<String>,
    pub event_type: String,
    pub confidence: f64,
    pub status: String,
    pub error: Option<String>,
}

pub fn append(metrics_path: impl AsRef<Path>, record: &TelemetryRecord) -> anyhow::Result<()> {
    if let Some(parent) = metrics_path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record).context("serialize telemetry")?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true).append(true).open(&metrics_path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_and_read_back_roundtrip() {
        let d = TempDir::new().unwrap();
        let path = d.path().join("metrics.jsonl");

        let r1 = TelemetryRecord {
            timestamp: "2026-04-30T00:00:00Z".into(),
            project_hash: "feedface".into(),
            task_id_guess: Some("tj-x".into()),
            event_type: "decision".into(),
            confidence: 0.92,
            status: "confirmed".into(),
            error: None,
        };
        let r2 = TelemetryRecord { confidence: 0.4, status: "suggested".into(), ..r1.clone() };
        append(&path, &r1).unwrap();
        append(&path, &r2).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 2);
    }
}
```

- [ ] **Step 2: Add `pub mod telemetry;` to classifier `mod.rs`**

- [ ] **Step 3: Add `metrics_dir()` to `paths.rs`**

```rust
pub fn metrics_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("metrics"))
}
```

- [ ] **Step 4: GREEN, commit, close**

---

## Task 6: CLI `task-journal stats` — show classifier accuracy

**bd task:** `<P4.06>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn stats_command_shows_classifier_counts() {
    let dir = assert_fs::TempDir::new().unwrap();

    // Pre-seed metrics/<hash>.jsonl with synthetic records.
    let metrics = dir.path().join("task-journal").join("metrics");
    std::fs::create_dir_all(&metrics).unwrap();
    let body = vec![
        r#"{"timestamp":"2026-04-30T00:00:00Z","project_hash":"feedface","task_id_guess":"tj-x","event_type":"decision","confidence":0.95,"status":"confirmed","error":null}"#,
        r#"{"timestamp":"2026-04-30T00:00:00Z","project_hash":"feedface","task_id_guess":"tj-x","event_type":"finding","confidence":0.65,"status":"suggested","error":null}"#,
    ].join("\n");
    std::fs::write(metrics.join("feedface.jsonl"), body).unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["stats"])
        .assert().success()
        .stdout(contains("classified: 2")
            .and(contains("confirmed: 1"))
            .and(contains("suggested: 1")));
}
```

- [ ] **Step 2: Add `Stats` subcommand**

```rust
/// Show local classifier and journal statistics.
Stats,
```

In match:

```rust
Commands::Stats => {
    let metrics_dir = tj_core::paths::metrics_dir()?;
    let mut total = 0usize; let mut confirmed = 0usize; let mut suggested = 0usize;
    let mut errors = 0usize;
    if metrics_dir.exists() {
        for entry in std::fs::read_dir(&metrics_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            let body = std::fs::read_to_string(&path)?;
            for line in body.lines().filter(|l| !l.trim().is_empty()) {
                total += 1;
                let v: serde_json::Value = match serde_json::from_str(line) { Ok(v)=>v, Err(_) => { errors += 1; continue }};
                match v.get("status").and_then(|s| s.as_str()) {
                    Some("confirmed") => confirmed += 1,
                    Some("suggested") => suggested += 1,
                    _ => {}
                }
            }
        }
    }
    println!("classified: {total}");
    println!("  confirmed: {confirmed}");
    println!("  suggested: {suggested}");
    println!("  parse errors: {errors}");
    if total > 0 {
        let ratio = confirmed as f64 / total as f64 * 100.0;
        println!("  confirmed ratio: {ratio:.1}%");
    }
}
```

- [ ] **Step 3: GREEN, commit, close**

---

## Task 7: Wire telemetry into `ingest-hook`

**bd task:** `<P4.07>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn ingest_hook_writes_telemetry_record() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Tel"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook", "--kind", "Stop", "--text", "decided to use Rust",
            "--mock-event-type", "decision",
            "--mock-task-id", &task_id,
            "--mock-confidence", "0.92",
        ])
        .assert().success();

    // metrics dir should now have at least one record.
    let metrics_dir = dir.path().join("task-journal").join("metrics");
    let mut total_lines = 0;
    if metrics_dir.exists() {
        for entry in std::fs::read_dir(&metrics_dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                total_lines += std::fs::read_to_string(&p).unwrap().lines().count();
            }
        }
    }
    assert!(total_lines >= 1, "expected at least one telemetry line, got {total_lines}");
}
```

- [ ] **Step 2: Append telemetry call inside `IngestHook` arm after writing event**

```rust
let metrics_path = tj_core::paths::metrics_dir()?.join(format!("{project_hash}.jsonl"));
let _ = tj_core::classifier::telemetry::append(&metrics_path, &tj_core::classifier::telemetry::TelemetryRecord {
    timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    project_hash: project_hash.clone(),
    task_id_guess: Some(task_id.clone()),
    event_type: serde_json::to_value(&etype)?.as_str().unwrap_or("?").to_string(),
    confidence,
    status: serde_json::to_value(&event.status)?.as_str().unwrap_or("?").to_string(),
    error: None,
});
```

- [ ] **Step 3: GREEN, commit, close**

---

## Task 8: Hook installer — append `|| true` to suppress failures

**bd task:** `<P4.08>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn install_hooks_command_uses_no_fail_pattern() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert().success();
    let s = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(s.contains("|| true"), "hook command must end with || true so a failed classifier doesn't break Claude Code: {s}");
}
```

- [ ] **Step 2: Update install-hooks to wrap command with `|| true`**

```rust
let cmd = "task-journal ingest-hook --kind=$CLAUDE_HOOK_NAME --text=\"$CLAUDE_HOOK_TEXT\" || true";
```

- [ ] **Step 3: GREEN, commit, close**

---

## Task 9: Gate `--mock-*` flags behind `test-helpers` feature

**bd task:** `<P4.09>`
**Files:**
- Modify: `crates/tj-cli/Cargo.toml`
- Modify: `crates/tj-cli/src/main.rs`

- [ ] **Step 1: Add feature**

In `crates/tj-cli/Cargo.toml`:

```toml
[features]
default = []
test-helpers = []
```

In `[dev-dependencies]` ensure tests build with the feature. Add to root `Cargo.toml` or just enable per-crate:

```toml
[package.metadata.cargo-test-features]
test-helpers = []
```

(Simpler: tests run with `--features test-helpers` via `.cargo/config.toml`.)

Create `.cargo/config.toml`:

```toml
[alias]
test-all = ["test", "--workspace", "--features", "test-helpers"]
```

- [ ] **Step 2: Gate the flags in `main.rs`**

```rust
#[cfg(feature = "test-helpers")]
mock_event_type: Option<String>,
#[cfg(feature = "test-helpers")]
mock_task_id: Option<String>,
#[cfg(feature = "test-helpers")]
mock_confidence: Option<f64>,
```

And gate the destructuring + branching in `IngestHook` arm with `#[cfg(feature = "test-helpers")]` similarly.

- [ ] **Step 3: Run tests with feature on**

```bash
cargo test --workspace --features tj-cli/test-helpers
```

- [ ] **Step 4: Commit, close**

---

## Task 10: README.md — install + usage

**bd task:** `<P4.10>`
**Files:**
- Create: `README.md`

- [ ] **Step 1: Write README**

```markdown
# Task Journal

Append-only journal for AI-coding tasks. Captures the *reasoning chain* — goals, hypotheses, decisions, rejections, evidence — and renders a compact resume pack on demand so an agent can pick up a 2-week-old task with full context.

## Quick start

```bash
# Install
cargo install --path crates/tj-cli
cargo install --path crates/tj-mcp

# Open a task
task-journal create "Add OAuth login"
# → tj-x9rz1f

# Record decisions / findings as you work
task-journal event tj-x9rz1f --type decision --text "Adopt PKCE flow"
task-journal event tj-x9rz1f --type rejection --text "Implicit grant: deprecated"

# Get a resume pack
task-journal pack tj-x9rz1f --mode full
```

## MCP integration with Claude Code

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "task-journal": { "command": "task-journal-mcp" }
  }
}
```

Then 5 tools become available: `task_pack`, `task_search`, `task_create`, `event_add`, `task_close`.

## Auto-capture via Claude Code hooks

```bash
export ANTHROPIC_API_KEY=sk-ant-...
task-journal install-hooks --scope user
```

Hooks send chat chunks to Claude Haiku for classification. Confidence ≥ 0.85 → confirmed event; < 0.85 → suggested event (rendered with `[?]` marker, you decide).

See `INSTALL.md` for the hook walkthrough.

## Architecture

`task-journal` is a Rust workspace:
- `tj-core` — event schema (JSONL, append-only), SQLite derived state, pack assembler
- `tj-cli` — `task-journal` binary
- `tj-mcp` — `task-journal-mcp` binary (MCP server)

Source of truth = JSONL event log. SQLite is rebuildable.

Design: see `.docs/plans/2026-04-29-task-journal-v1-design.md`.
```

- [ ] **Step 2: Commit, close**

---

## Task 11: INSTALL.md — hook walkthrough

**bd task:** `<P4.11>`
**Files:**
- Create: `INSTALL.md`

- [ ] **Step 1: Write INSTALL**

```markdown
# Installation

## Prerequisites

- Rust toolchain (1.83+) — `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- (For auto-capture) `ANTHROPIC_API_KEY` exported in your shell

## Build & install

```bash
git clone <this repo>
cd claude-memory
cargo build --release --workspace
cargo install --path crates/tj-cli
cargo install --path crates/tj-mcp
```

This installs `task-journal` and `task-journal-mcp` to `~/.cargo/bin/`.

## MCP server (Claude Code)

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "task-journal": { "command": "task-journal-mcp" }
  }
}
```

Restart Claude Code. The 5 tools (`task_pack`, `task_search`, `task_create`, `event_add`, `task_close`) become available to the agent.

## Auto-capture hooks

```bash
export ANTHROPIC_API_KEY=sk-ant-...
task-journal install-hooks --scope user        # writes to ~/.claude/settings.json
# OR for one project only:
cd /path/to/project
task-journal install-hooks --scope project     # writes to .claude/settings.json
```

The hook command is wrapped with `|| true` so a classifier failure (network, rate limit) **never** breaks Claude Code. Failures land in `<data-dir>/pending/<id>.json` and are replayed on next successful ingest.

## Verify

```bash
task-journal create "Test task"
# → tj-xxxxxx
task-journal event tj-xxxxxx --type decision --text "Adopt my plan"
task-journal pack tj-xxxxxx --mode full
# → Markdown with the decision visible
```

## Uninstall hooks

```bash
task-journal install-hooks --scope user --uninstall
```

## Where data lives

- Linux/WSL: `$XDG_DATA_HOME/task-journal` (default `~/.local/share/task-journal`)
- macOS: `~/Library/Application Support/task-journal`
- Windows: `%LOCALAPPDATA%\task-journal`

Inside:
- `events/<project_hash>.jsonl` — append-only event log (source of truth)
- `state/<project_hash>.sqlite` — derived state (rebuildable)
- `metrics/<project_hash>.jsonl` — classifier telemetry
- `pending/<id>.json` — failed classifications awaiting retry

To reset a project: `rm <data-dir>/state/<hash>.sqlite` (regenerated on next read).
To wipe all journal: `rm -rf <data-dir>`.
```

- [ ] **Step 2: Commit, close**

---

## Task 12: P4 verification gate + demo

**bd task:** `<P4.12>`
**Files:**
- Create: `.beads/hooks/p4-demo.sh`

- [ ] **Step 1: Write `p4-demo.sh`**

```bash
#!/bin/bash
set -e
cd /home/shahinyanm/www/claude-memory

DEMO=/tmp/tj-p4-demo
rm -rf "$DEMO"
mkdir -p "$DEMO"
export XDG_DATA_HOME="$DEMO"

echo "==================== P4 DEMO (polish + dogfood) ===================="

# Two projects via env-driven paths.
mkdir -p "$DEMO/proj-a" "$DEMO/proj-b"

(cd "$DEMO/proj-a" && /home/shahinyanm/www/claude-memory/target/debug/task-journal create "OAuth login")
(cd "$DEMO/proj-b" && /home/shahinyanm/www/claude-memory/target/debug/task-journal create "Build pack assembler")

echo
echo ">>> Cross-project search:"
./target/debug/task-journal search "OAuth" --all-projects
./target/debug/task-journal search "pack" --all-projects

echo
echo ">>> Stats:"
./target/debug/task-journal stats

echo
echo ">>> P4 mock-classifier ingest with telemetry:"
TASK_ID=$(./target/debug/task-journal create "Telemetry test")
./target/debug/task-journal ingest-hook --kind Stop --text "decided to use Rust" \
  --mock-event-type decision --mock-task-id "$TASK_ID" --mock-confidence 0.92 >/dev/null
./target/debug/task-journal stats
```

- [ ] **Step 2: Run gate**

```bash
cargo test --workspace --features tj-cli/test-helpers
.beads/hooks/p2-demo.sh
.beads/hooks/p3-mock-demo.sh
.beads/hooks/p4-demo.sh
```

- [ ] **Step 3: Close epic OR keep open if anything blocks dogfood**

```bash
bd close <P4.12-id> --reason "P4 done; ready for dogfood"
```

If everything green, epic auto-closes (last child closed).

---

# Beads task batch-create helper (P4)

```
01 db list_all_projects helper
02 CLI search --all-projects flag
03 Pack token-budget truncation full mode
04 Pack metadata truncated field
05 Classifier telemetry append-only writer
06 CLI stats subcommand
07 ingest-hook writes telemetry
08 Install-hooks adds || true to suppress failures
09 Gate --mock-* flags behind test-helpers feature
10 README install + usage
11 INSTALL hook walkthrough
12 P4 verification gate plus demo
```

---

# Self-Review

**Spec coverage** vs design doc Phase 4 ("Polish + dogfood"):

| Design item | Tasks |
|------|------|
| Cross-project search | 1, 2 |
| Token budget tuning | 3, 4 |
| Local telemetry | 5, 6, 7 |
| README + install | 10, 11 |
| Hook resilience | 8 |
| Test/prod separation | 9 |
| E2E demo | 12 |

**Placeholder scan**: Tasks 9 and 10 reference real-world hook variables and Cargo features that need verification during implementation (Task 9 may need adjustment based on how `cargo test` resolves the feature flag — fall back to `cargo test --workspace --all-features` if `--features tj-cli/test-helpers` confuses cargo).

**Type consistency**: `TelemetryRecord` introduced in Task 5 used in Tasks 6+7. CLI `--all-projects` flag introduced in Task 2 doesn't conflict with existing flags.

**Scope check**: P4 is the last v1 phase. After Task 12, the next move is **dogfood** — using the tool on a real Claude Code session for ~1 week and capturing real-world bugs as v1.1 patches. If dogfood reveals fundamental issues (classifier accuracy <0.6, hook env vars missing, etc), they become v1.1 patch tasks; if not, v1 is shipped.

---

**End of Phase 4 plan.** No P5: v1 ships after this gate.
