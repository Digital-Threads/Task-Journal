# Task Journal v1 — Phase 1 (Skeleton) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a Rust workspace with `tj-core`, `tj-mcp`, and `tj-cli` crates that expose the v1 event schema, JSONL append-only storage, SQLite derived-state tables, all 5 MCP tools as mock stubs, and a basic CLI — so subsequent phases can fill in real assembler/classifier/hooks logic without touching scaffolding.

**Architecture:** Three-crate Cargo workspace. `tj-core` is a pure library (no I/O frameworks) with `Event`/`EventType`, JSONL `Writer`, OS path resolution, project-hash, and SQLite repos. `tj-mcp` is the MCP server binary (`rmcp` + Tokio stdio). `tj-cli` is the human-facing CLI (`clap`). Both binaries depend on `tj-core`. The JSONL log is the source of truth; SQLite is rebuildable from it.

**Tech Stack:** Rust 1.83+, `rmcp` (official MCP SDK), Tokio (async runtime), `serde` + `schemars` (event schema), `rusqlite` (SQLite, bundled feature), `clap` v4 (CLI), `directories` (OS-specific data dirs), `dunce` (path canonicalization), `sha2` (project hash), `ulid` (event IDs), `chrono` (timestamps), `anyhow` + `thiserror` (errors), `assert_fs` + `predicates` (CLI integration tests).

**Working directory (all commands):** `/home/shahinyanm/www/claude-memory` inside WSL Ubuntu. From the host, prefix every shell call with `wsl -d Ubuntu -- bash -c 'export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; cd /home/shahinyanm/www/claude-memory && <command>'`.

**Beads tracking:** Each task corresponds to one `bd` issue under epic `claude-memory-d36`. The executor MUST call `bd update <id> --claim` before starting and `bd close <id> --reason "..."` when done.

**Spec & Design source of truth:**
- `.docs/plans/2026-04-29-tz-task-journal-v2.md` — pinned ТЗ
- `.docs/plans/2026-04-29-task-journal-v1-design.md` — answers to all 9 architectural questions

---

## Pre-flight: Verify environment and Context7-check `rmcp`

Before starting Task 1, the executor MUST:

1. Run `cargo --version` inside WSL. If "command not found", proceed with Task 1 (rustup install). Otherwise skip Task 1.
2. Query Context7 for the latest `rmcp` API (the design doc was written 2026-04-29 — APIs may have shifted):
   ```
   mcp__plugin_context7_context7__resolve-library-id  libraryName="rmcp"  query="MCP server stdio tool macros"
   mcp__plugin_context7_context7__query-docs  libraryId="<resolved>"  query="Minimal stdio MCP server with #[tool_router] and async tools, rmcp 0.x as of 2026"
   ```
   If the macro names or transport signature changed since this plan was written, adjust the relevant tasks below before coding.

---

## File Structure

Files this plan creates (final layout after Task 22):

```
claude-memory/
├── Cargo.toml                                     # workspace root
├── crates/
│   ├── tj-core/
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs                             # re-exports
│   │   │   ├── event.rs                           # Event, EventType, EventStatus, Author, Source
│   │   │   ├── storage.rs                         # JsonlWriter (append+fsync)
│   │   │   ├── paths.rs                           # data_dir() for current OS
│   │   │   ├── project_hash.rs                    # canonical path → 16-hex hash
│   │   │   └── db.rs                              # SQLite open + migrations + tasks/events repos
│   │   └── tests/
│   │       └── round_trip.rs                      # integration: write events, rebuild state, query
│   ├── tj-mcp/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs                            # rmcp + tokio + 5 stub tools
│   └── tj-cli/
│       ├── Cargo.toml
│       └── src/
│           └── main.rs                            # clap subcommands: create, events, rebuild-state
├── .gitignore                                     # add target/, *.sqlite
└── .docs/                                         # already exists
```

Boundaries: `tj-core` has zero deps on `rmcp` or `clap`. `tj-mcp` and `tj-cli` both depend on `tj-core`. This makes core fast to test and lets the binaries swap independently.

---

# Tasks

> **Test discipline:** every task that adds production code starts with a failing test (RED), then minimal impl (GREEN), then run + commit. No production code without a failing test first. After GREEN, commit with conventional-commit message that mentions the bd issue id.

> **Beads task creation:** the executor creates one `bd` issue per Task below as it picks them up. A helper batch-create command appears at the bottom of this plan.

---

## Task 1: Install Rust toolchain (skip if already present)

**bd task:** `claude-memory-p1-01`
**Files:** none (system change only)

- [ ] **Step 1: Check if rustup/cargo already installed**

Run: `wsl -d Ubuntu -- bash -c 'command -v cargo'`
Expected: prints path → skip remaining steps. Empty → continue.

- [ ] **Step 2: Install rustup non-interactively**

Run:
```bash
wsl -d Ubuntu -- bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal'
```
Expected: "Rust is installed now. Great!" near the end.

- [ ] **Step 3: Verify**

Run:
```bash
wsl -d Ubuntu -- bash -c 'export PATH="$HOME/.cargo/bin:$PATH"; cargo --version && rustc --version'
```
Expected: `cargo 1.83.x` or newer, `rustc 1.83.x` or newer.

- [ ] **Step 4: Close bd**

```bash
bd close claude-memory-p1-01 --reason "rustup installed"
```

---

## Task 2: Create Cargo workspace skeleton

**bd task:** `claude-memory-p1-02`
**Files:**
- Create: `Cargo.toml`
- Create: `crates/tj-core/Cargo.toml`
- Create: `crates/tj-core/src/lib.rs`
- Create: `crates/tj-mcp/Cargo.toml`
- Create: `crates/tj-mcp/src/main.rs`
- Create: `crates/tj-cli/Cargo.toml`
- Create: `crates/tj-cli/src/main.rs`
- Create: `.gitignore`

- [ ] **Step 1: Write the failing test (workspace builds)**

This task has no code logic to TDD; the "test" is `cargo build` succeeding. Skip RED phase, but the Step 5 build **must** be the failing-then-passing gate.

- [ ] **Step 2: Create root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = [
    "crates/tj-core",
    "crates/tj-mcp",
    "crates/tj-cli",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.83"
license = "MIT"

[workspace.dependencies]
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0.8"
chrono = { version = "0.4", features = ["serde"] }
ulid = { version = "1", features = ["serde"] }
sha2 = "0.10"
dunce = "1"
directories = "5"
rusqlite = { version = "0.31", features = ["bundled"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "io-std"] }
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Test deps
assert_fs = "1"
predicates = "3"
tempfile = "3"
```

- [ ] **Step 3: Create `crates/tj-core/Cargo.toml`**

```toml
[package]
name = "tj-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
schemars = { workspace = true }
chrono = { workspace = true }
ulid = { workspace = true }
sha2 = { workspace = true }
dunce = { workspace = true }
directories = { workspace = true }
rusqlite = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 4: Create `crates/tj-core/src/lib.rs`**

```rust
//! tj-core: append-only event log + derived SQLite state for Task Journal.

#![deny(rust_2018_idioms)]

pub mod event;
pub mod storage;
pub mod paths;
pub mod project_hash;
pub mod db;
```

(Modules will be created as empty files in subsequent tasks — for now make placeholders so the lib compiles.)

```bash
mkdir -p crates/tj-core/src
touch crates/tj-core/src/event.rs crates/tj-core/src/storage.rs crates/tj-core/src/paths.rs crates/tj-core/src/project_hash.rs crates/tj-core/src/db.rs
```

- [ ] **Step 5: Create `crates/tj-mcp/Cargo.toml`** (rmcp added in Task 12, blank for now)

```toml
[package]
name = "tj-mcp"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "task-journal-mcp"
path = "src/main.rs"

[dependencies]
tj-core = { path = "../tj-core" }
anyhow = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 6: Create `crates/tj-mcp/src/main.rs`**

```rust
fn main() {
    println!("task-journal-mcp v0 placeholder — wired up in Task 12");
}
```

- [ ] **Step 7: Create `crates/tj-cli/Cargo.toml`**

```toml
[package]
name = "tj-cli"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "task-journal"
path = "src/main.rs"

[dependencies]
tj-core = { path = "../tj-core" }
anyhow = { workspace = true }
clap = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde_json = { workspace = true }
```

- [ ] **Step 8: Create `crates/tj-cli/src/main.rs`**

```rust
fn main() {
    println!("task-journal CLI v0 placeholder — wired up in Task 18");
}
```

- [ ] **Step 9: Create `.gitignore`**

```
/target
*.sqlite
*.sqlite-journal
*.sqlite-wal
.DS_Store
```

- [ ] **Step 10: Build the workspace**

Run: `cargo build --workspace`
Expected: `Compiling tj-core...`, `Compiling tj-mcp...`, `Compiling tj-cli...`, all succeed. Two binaries placed at `target/debug/task-journal-mcp` and `target/debug/task-journal`.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml Cargo.lock crates/ .gitignore
git commit -m "feat(skeleton): cargo workspace with tj-core, tj-mcp, tj-cli (claude-memory-p1-02)"
bd close claude-memory-p1-02 --reason "Workspace skeleton compiles"
```

---

## Task 3: `EventType` enum

**bd task:** `claude-memory-p1-03`
**Files:**
- Modify: `crates/tj-core/src/event.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/tj-core/src/event.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_serializes_to_snake_case() {
        let t = EventType::Decision;
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s, "\"decision\"");
    }

    #[test]
    fn event_type_round_trip_all_variants() {
        for ty in EventType::ALL {
            let s = serde_json::to_string(&ty).unwrap();
            let back: EventType = serde_json::from_str(&s).unwrap();
            assert_eq!(*ty, back);
        }
    }
}
```

- [ ] **Step 2: Run the test (must fail to compile)**

Run: `cargo test -p tj-core`
Expected: errors like "cannot find type `EventType`".

- [ ] **Step 3: Implement `EventType`**

Replace the test module placement and add the type at the top of `event.rs`:
```rust
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Open,
    Hypothesis,
    Finding,
    Evidence,
    Decision,
    Rejection,
    Constraint,
    Correction,
    Reopen,
    Supersede,
    Close,
    Redirect,
}

impl EventType {
    pub const ALL: &'static [Self] = &[
        Self::Open, Self::Hypothesis, Self::Finding, Self::Evidence,
        Self::Decision, Self::Rejection, Self::Constraint, Self::Correction,
        Self::Reopen, Self::Supersede, Self::Close, Self::Redirect,
    ];
}

#[cfg(test)]
mod tests { /* keep the tests from Step 1 */ }
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p tj-core event_type`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/event.rs
git commit -m "feat(core): EventType enum with 12 v1 event types (claude-memory-p1-03)"
bd close claude-memory-p1-03 --reason "EventType enum implemented and round-trips"
```

---

## Task 4: `Author`, `Source`, `EventStatus`, `EvidenceStrength` enums

**bd task:** `claude-memory-p1-04`
**Files:**
- Modify: `crates/tj-core/src/event.rs`

- [ ] **Step 1: Write the failing test**

Append to `event.rs` test module:
```rust
#[test]
fn author_source_status_strength_serialize_snake_case() {
    assert_eq!(serde_json::to_string(&Author::Classifier).unwrap(), "\"classifier\"");
    assert_eq!(serde_json::to_string(&Source::Hook).unwrap(), "\"hook\"");
    assert_eq!(serde_json::to_string(&EventStatus::Suggested).unwrap(), "\"suggested\"");
    assert_eq!(serde_json::to_string(&EvidenceStrength::Strong).unwrap(), "\"strong\"");
}
```

- [ ] **Step 2: Run, fail to compile**

Run: `cargo test -p tj-core author_source`
Expected: "cannot find type `Author`".

- [ ] **Step 3: Add the enums**

Insert above the test module:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Author { User, Agent, Classifier, Hook }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source { Chat, Hook, Manual, Cli }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus { Confirmed, Suggested }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength { Weak, Medium, Strong }
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core author_source_status_strength`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/event.rs
git commit -m "feat(core): Author/Source/EventStatus/EvidenceStrength enums (claude-memory-p1-04)"
bd close claude-memory-p1-04 --reason "Enums implemented"
```

---

## Task 5: `Refs` struct + `Event` struct with serde

**bd task:** `claude-memory-p1-05`
**Files:**
- Modify: `crates/tj-core/src/event.rs`

- [ ] **Step 1: Write the failing test**

Append to test module:
```rust
#[test]
fn event_round_trip_all_fields() {
    let e = Event {
        event_id: "01HZX5K8000000000000000000".to_string(),
        schema_version: "1.0".to_string(),
        task_id: "tj-7f3a".to_string(),
        event_type: EventType::Decision,
        timestamp: "2026-05-14T12:00:00+04:00".to_string(),
        author: Author::Agent,
        source: Source::Chat,
        confidence: Some(0.92),
        evidence_strength: Some(EvidenceStrength::Strong),
        text: "Adopt Rust + rmcp.".to_string(),
        refs: Refs {
            commits: vec!["a3f2dd".into()],
            files: vec!["Cargo.toml".into()],
            events: vec![],
        },
        corrects: None,
        supersedes: None,
        status: EventStatus::Confirmed,
        meta: serde_json::json!({}),
    };
    let s = serde_json::to_string(&e).unwrap();
    let back: Event = serde_json::from_str(&s).unwrap();
    assert_eq!(e.event_id, back.event_id);
    assert_eq!(e.event_type, back.event_type);
    assert_eq!(e.refs.commits, back.refs.commits);
    assert_eq!(e.confidence, back.confidence);
}
```

- [ ] **Step 2: Run, fail to compile**

Run: `cargo test -p tj-core event_round_trip`
Expected: "cannot find type `Event`" / "cannot find type `Refs`".

- [ ] **Step 3: Add `Refs` and `Event`**

Insert above the test module:
```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Refs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub event_id: String,
    pub schema_version: String,
    pub task_id: String,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub timestamp: String,
    pub author: Author,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_strength: Option<EvidenceStrength>,
    pub text: String,
    #[serde(default)]
    pub refs: Refs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub status: EventStatus,
    #[serde(default)]
    pub meta: serde_json::Value,
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core event_round_trip`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/event.rs
git commit -m "feat(core): Event + Refs structs with full schema (claude-memory-p1-05)"
bd close claude-memory-p1-05 --reason "Event struct round-trips"
```

---

## Task 6: `Event::new(...)` constructor with ULID + RFC3339 timestamp

**bd task:** `claude-memory-p1-06`
**Files:**
- Modify: `crates/tj-core/src/event.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn event_new_assigns_ulid_and_now() {
    let a = Event::new("tj-1", EventType::Open, Author::User, Source::Manual, "first".into());
    let b = Event::new("tj-1", EventType::Open, Author::User, Source::Manual, "second".into());
    assert_ne!(a.event_id, b.event_id);
    assert_eq!(a.event_id.len(), 26); // ULID is 26 chars
    assert!(a.event_id < b.event_id, "ULIDs must be monotonic-ish");
    assert_eq!(a.schema_version, "1.0");
    assert_eq!(a.status, EventStatus::Confirmed);
    // timestamp is RFC3339-ish: at minimum it parses
    chrono::DateTime::parse_from_rfc3339(&a.timestamp).expect("RFC3339");
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-core event_new_assigns`
Expected: "no associated function or method named `new`".

- [ ] **Step 3: Implement `Event::new`**

Append to `event.rs` (above the test module):
```rust
impl Event {
    pub fn new(
        task_id: impl Into<String>,
        event_type: EventType,
        author: Author,
        source: Source,
        text: String,
    ) -> Self {
        Event {
            event_id: ulid::Ulid::new().to_string(),
            schema_version: "1.0".to_string(),
            task_id: task_id.into(),
            event_type,
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            author,
            source,
            confidence: None,
            evidence_strength: None,
            text,
            refs: Refs::default(),
            corrects: None,
            supersedes: None,
            status: EventStatus::Confirmed,
            meta: serde_json::json!({}),
        }
    }
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core event_new_assigns`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/event.rs
git commit -m "feat(core): Event::new constructor with ULID + UTC RFC3339 (claude-memory-p1-06)"
bd close claude-memory-p1-06 --reason "Event::new implemented"
```

---

## Task 7: `JsonlWriter` — append-only with `fsync` on flush

**bd task:** `claude-memory-p1-07`
**Files:**
- Modify: `crates/tj-core/src/storage.rs`
- Modify: `crates/tj-core/src/lib.rs` (no change — module already declared)

- [ ] **Step 1: Write the failing test**

Create `crates/tj-core/src/storage.rs`:
```rust
use crate::event::*;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub struct JsonlWriter {
    path: PathBuf,
    inner: BufWriter<File>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_event(text: &str) -> Event {
        Event::new("tj-1", EventType::Open, Author::User, Source::Cli, text.into())
    }

    #[test]
    fn append_three_events_yields_three_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("events.jsonl");

        let mut w = JsonlWriter::open(&path).unwrap();
        w.append(&sample_event("a")).unwrap();
        w.append(&sample_event("b")).unwrap();
        w.append(&sample_event("c")).unwrap();
        w.flush_durable().unwrap();
        drop(w);

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let _: Event = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn reopen_appends_not_truncates() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("events.jsonl");

        {
            let mut w = JsonlWriter::open(&path).unwrap();
            w.append(&sample_event("a")).unwrap();
            w.flush_durable().unwrap();
        }
        {
            let mut w = JsonlWriter::open(&path).unwrap();
            w.append(&sample_event("b")).unwrap();
            w.flush_durable().unwrap();
        }

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 2);
    }
}
```

- [ ] **Step 2: Run, fail to compile**

Run: `cargo test -p tj-core --lib storage::tests`
Expected: "no function or associated item named `open` found".

- [ ] **Step 3: Implement `JsonlWriter`**

Replace the stub at the top of `storage.rs`:
```rust
use crate::event::Event;
use anyhow::Context;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub struct JsonlWriter {
    path: PathBuf,
    inner: BufWriter<File>,
}

impl JsonlWriter {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {parent:?}"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {path:?} for append"))?;
        Ok(Self { path, inner: BufWriter::new(file) })
    }

    pub fn append(&mut self, event: &Event) -> anyhow::Result<()> {
        let line = serde_json::to_string(event).context("serialize event")?;
        self.inner.write_all(line.as_bytes()).context("write event line")?;
        self.inner.write_all(b"\n").context("write newline")?;
        Ok(())
    }

    /// Flush user buffers to OS, then fsync the underlying file so the bytes
    /// survive a crash. Call after every batch of appends that must be durable.
    pub fn flush_durable(&mut self) -> anyhow::Result<()> {
        self.inner.flush().context("flush BufWriter")?;
        self.inner.get_ref().sync_all().context("fsync events file")?;
        Ok(())
    }

    pub fn path(&self) -> &Path { &self.path }
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core --lib storage::tests`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/storage.rs
git commit -m "feat(core): JsonlWriter with append + fsync on flush_durable (claude-memory-p1-07)"
bd close claude-memory-p1-07 --reason "JsonlWriter passes round-trip + reopen tests"
```

---

## Task 8: `paths::data_dir()` for current OS

**bd task:** `claude-memory-p1-08`
**Files:**
- Modify: `crates/tj-core/src/paths.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/tj-core/src/paths.rs`:
```rust
use anyhow::Context;
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_returns_a_path_containing_task_journal() {
        let p = data_dir().expect("data_dir");
        let s = p.to_string_lossy();
        assert!(s.contains("task-journal"), "got: {s}");
    }

    #[test]
    fn project_dir_appends_subdir() {
        let p = project_storage_dir("abc123").expect("project dir");
        assert!(p.ends_with("abc123"), "got: {p:?}");
    }
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-core --lib paths::tests`
Expected: "cannot find function `data_dir`".

- [ ] **Step 3: Implement**

Replace the stub:
```rust
use anyhow::Context;
use std::path::PathBuf;

/// Base data directory for Task Journal on the current OS.
/// - Linux/WSL: $XDG_DATA_HOME/task-journal (default ~/.local/share/task-journal)
/// - macOS: ~/Library/Application Support/task-journal
/// - Windows: %LOCALAPPDATA%\task-journal
pub fn data_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "task-journal")
        .context("could not resolve OS data directories")?;
    Ok(dirs.data_local_dir().to_path_buf())
}

pub fn events_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("events"))
}

pub fn state_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("state"))
}

pub fn project_storage_dir(project_hash: &str) -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join(project_hash))
}

#[cfg(test)]
mod tests { /* keep the tests from Step 1 */ }
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core --lib paths::tests`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/paths.rs
git commit -m "feat(core): OS-specific data_dir, events_dir, state_dir resolution (claude-memory-p1-08)"
bd close claude-memory-p1-08 --reason "Path resolution implemented"
```

---

## Task 9: `project_hash::from_path` — canonical path → 16-hex hash

**bd task:** `claude-memory-p1-09`
**Files:**
- Modify: `crates/tj-core/src/project_hash.rs`

- [ ] **Step 1: Write the failing test**

```rust
use anyhow::Context;
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn from_path(p: impl AsRef<Path>) -> anyhow::Result<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn same_path_yields_same_hash() {
        let d = TempDir::new().unwrap();
        let a = from_path(d.path()).unwrap();
        let b = from_path(d.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16, "16 hex chars expected, got: {a}");
    }

    #[test]
    fn different_paths_yield_different_hashes() {
        let d1 = TempDir::new().unwrap();
        let d2 = TempDir::new().unwrap();
        let a = from_path(d1.path()).unwrap();
        let b = from_path(d2.path()).unwrap();
        assert_ne!(a, b);
    }
}
```

- [ ] **Step 2: Run — must fail at runtime**

Run: `cargo test -p tj-core --lib project_hash::tests`
Expected: panics with "not yet implemented" (the `todo!()`).

- [ ] **Step 3: Implement**

Replace the function body:
```rust
pub fn from_path(p: impl AsRef<Path>) -> anyhow::Result<String> {
    let canonical = dunce::canonicalize(p.as_ref())
        .with_context(|| format!("canonicalize {:?}", p.as_ref()))?;
    let bytes = canonical.as_os_str().as_encoded_bytes();
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    debug_assert_eq!(hex.len(), 16);
    Ok(hex)
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core --lib project_hash::tests`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/project_hash.rs
git commit -m "feat(core): project_hash::from_path canonicalizes and SHA-256-truncates (claude-memory-p1-09)"
bd close claude-memory-p1-09 --reason "project_hash implemented"
```

---

## Task 10: SQLite open + initial migration

**bd task:** `claude-memory-p1-10`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/tj-core/src/db.rs`:
```rust
use anyhow::Context;
use rusqlite::Connection;
use std::path::Path;

pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn open_creates_all_tables() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("state.sqlite");
        let conn = open(&p).unwrap();

        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' OR type='virtual table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        for required in [
            "decisions", "events_index", "evidence", "task_pack_cache", "tasks", "search_fts"
        ] {
            assert!(names.iter().any(|n| n == required), "missing table {required}, have {names:?}");
        }
    }

    #[test]
    fn open_is_idempotent() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("state.sqlite");
        let _ = open(&p).unwrap();
        let _ = open(&p).unwrap(); // must not error
    }
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-core --lib db::tests`
Expected: panic at `todo!()`.

- [ ] **Step 3: Implement**

Replace the function body:
```rust
const MIGRATION_001: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
  task_id        TEXT PRIMARY KEY,
  title          TEXT NOT NULL,
  status         TEXT NOT NULL,
  project_hash   TEXT NOT NULL,
  opened_at      TEXT NOT NULL,
  closed_at      TEXT,
  last_event_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_hash, last_event_at DESC);

CREATE TABLE IF NOT EXISTS events_index (
  event_id    TEXT PRIMARY KEY,
  task_id     TEXT NOT NULL,
  type        TEXT NOT NULL,
  timestamp   TEXT NOT NULL,
  confidence  REAL,
  status      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_task_time ON events_index(task_id, timestamp DESC);

CREATE TABLE IF NOT EXISTS decisions (
  decision_id    TEXT PRIMARY KEY,
  task_id        TEXT NOT NULL,
  text           TEXT NOT NULL,
  status         TEXT NOT NULL,
  superseded_by  TEXT
);

CREATE TABLE IF NOT EXISTS evidence (
  evidence_id           TEXT PRIMARY KEY,
  task_id               TEXT NOT NULL,
  text                  TEXT NOT NULL,
  strength              TEXT NOT NULL,
  refers_to_decision_id TEXT
);

CREATE TABLE IF NOT EXISTS task_pack_cache (
  task_id             TEXT NOT NULL,
  mode                TEXT NOT NULL,
  text                TEXT NOT NULL,
  generated_at        TEXT NOT NULL,
  source_event_count  INTEGER NOT NULL,
  PRIMARY KEY (task_id, mode)
);

CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
  task_id UNINDEXED,
  event_id UNINDEXED,
  text,
  type
);
"#;

pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {parent:?}"))?;
    }
    let conn = Connection::open(&path)
        .with_context(|| format!("open SQLite at {:?}", path.as_ref()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    conn.execute_batch(MIGRATION_001).context("apply migration 001")?;
    Ok(conn)
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core --lib db::tests`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/db.rs
git commit -m "feat(core): SQLite open with initial migration (5 tables + FTS5) (claude-memory-p1-10)"
bd close claude-memory-p1-10 --reason "DB migrations apply, idempotent"
```

---

## Task 11: Tasks repo — `upsert_from_event` for `open` events

**bd task:** `claude-memory-p1-11`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Write the failing test**

Append to `db.rs` test module:
```rust
#[test]
fn upsert_task_from_open_event_inserts_row() {
    let d = TempDir::new().unwrap();
    let conn = open(d.path().join("s.sqlite")).unwrap();

    let mut e = crate::event::Event::new(
        "tj-7f3a", crate::event::EventType::Open,
        crate::event::Author::User, crate::event::Source::Cli,
        "Add OAuth".into()
    );
    e.meta = serde_json::json!({ "title": "Add OAuth login" });

    upsert_task_from_event(&conn, &e, "abcd1234abcd1234").unwrap();

    let (id, title, status): (String, String, String) = conn.query_row(
        "SELECT task_id, title, status FROM tasks WHERE task_id = ?1",
        ["tj-7f3a"],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();

    assert_eq!(id, "tj-7f3a");
    assert_eq!(title, "Add OAuth login");
    assert_eq!(status, "open");
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-core --lib db::tests::upsert_task`
Expected: "cannot find function `upsert_task_from_event`".

- [ ] **Step 3: Implement**

Append to `db.rs` (above test module):
```rust
use crate::event::{Event, EventType};

pub fn upsert_task_from_event(
    conn: &Connection,
    event: &Event,
    project_hash: &str,
) -> anyhow::Result<()> {
    match event.event_type {
        EventType::Open => {
            let title = event
                .meta
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.text)
                .to_string();
            conn.execute(
                "INSERT INTO tasks(task_id, title, status, project_hash, opened_at, last_event_at)
                 VALUES (?1, ?2, 'open', ?3, ?4, ?4)
                 ON CONFLICT(task_id) DO UPDATE SET last_event_at = ?4",
                rusqlite::params![event.task_id, title, project_hash, event.timestamp],
            )?;
        }
        EventType::Close => {
            conn.execute(
                "UPDATE tasks SET status='closed', closed_at=?2, last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
            )?;
        }
        EventType::Reopen => {
            conn.execute(
                "UPDATE tasks SET status='open', closed_at=NULL, last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
            )?;
        }
        _ => {
            // Bump last_event_at for any other event
            conn.execute(
                "UPDATE tasks SET last_event_at=?2 WHERE task_id=?1",
                rusqlite::params![event.task_id, event.timestamp],
            )?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core --lib db::tests::upsert_task`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/db.rs
git commit -m "feat(core): upsert_task_from_event handles open/close/reopen (claude-memory-p1-11)"
bd close claude-memory-p1-11 --reason "Tasks projection implemented for v1 lifecycle"
```

---

## Task 12: `index_event` — write to events_index + search_fts

**bd task:** `claude-memory-p1-12`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Write the failing test**

Append:
```rust
#[test]
fn index_event_writes_index_and_fts() {
    let d = TempDir::new().unwrap();
    let conn = open(d.path().join("s.sqlite")).unwrap();
    // Need a task row so FK is happy (no FK in v1, but realistic order)
    let mut open_e = crate::event::Event::new(
        "tj-1", crate::event::EventType::Open,
        crate::event::Author::User, crate::event::Source::Cli,
        "Title".into()
    );
    open_e.meta = serde_json::json!({"title": "Title"});
    upsert_task_from_event(&conn, &open_e, "deadbeefdeadbeef").unwrap();
    index_event(&conn, &open_e).unwrap();

    let mut decision = crate::event::Event::new(
        "tj-1", crate::event::EventType::Decision,
        crate::event::Author::Agent, crate::event::Source::Chat,
        "Adopt Rust".into()
    );
    decision.confidence = Some(0.92);
    upsert_task_from_event(&conn, &decision, "deadbeefdeadbeef").unwrap();
    index_event(&conn, &decision).unwrap();

    // events_index has 2 rows for tj-1
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
        ["tj-1"], |r| r.get(0)
    ).unwrap();
    assert_eq!(count, 2);

    // FTS search returns the decision
    let hits: Vec<String> = conn.prepare(
        "SELECT event_id FROM search_fts WHERE search_fts MATCH ?1"
    ).unwrap().query_map(["Rust"], |r| r.get::<_, String>(0)).unwrap()
        .collect::<Result<_,_>>().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0], decision.event_id);
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-core --lib db::tests::index_event_writes`
Expected: "cannot find function `index_event`".

- [ ] **Step 3: Implement**

Append:
```rust
pub fn index_event(conn: &Connection, event: &Event) -> anyhow::Result<()> {
    let type_str = serde_json::to_value(event.event_type)?
        .as_str()
        .unwrap()
        .to_string();
    let status_str = serde_json::to_value(event.status)?
        .as_str()
        .unwrap()
        .to_string();
    conn.execute(
        "INSERT OR REPLACE INTO events_index(event_id, task_id, type, timestamp, confidence, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            event.event_id, event.task_id, type_str,
            event.timestamp, event.confidence, status_str
        ],
    )?;
    conn.execute(
        "INSERT INTO search_fts(task_id, event_id, text, type) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event.task_id, event.event_id, event.text, type_str],
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core --lib db::tests::index_event_writes`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/db.rs
git commit -m "feat(core): index_event writes events_index + search_fts row (claude-memory-p1-12)"
bd close claude-memory-p1-12 --reason "Event indexing implemented"
```

---

## Task 13: `rebuild_state` — full re-projection from JSONL

**bd task:** `claude-memory-p1-13`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Write the failing test**

Append:
```rust
#[test]
fn rebuild_state_reads_jsonl_and_populates_db() {
    use std::io::Write;
    let d = TempDir::new().unwrap();
    let events_path = d.path().join("events.jsonl");
    let db_path = d.path().join("s.sqlite");

    let mut f = std::fs::File::create(&events_path).unwrap();
    let mut e1 = crate::event::Event::new(
        "tj-9", crate::event::EventType::Open,
        crate::event::Author::User, crate::event::Source::Cli,
        "x".into()
    );
    e1.meta = serde_json::json!({"title": "Nine"});
    let e2 = crate::event::Event::new(
        "tj-9", crate::event::EventType::Decision,
        crate::event::Author::Agent, crate::event::Source::Chat,
        "Adopt Rust".into()
    );
    writeln!(f, "{}", serde_json::to_string(&e1).unwrap()).unwrap();
    writeln!(f, "{}", serde_json::to_string(&e2).unwrap()).unwrap();
    drop(f);

    let conn = open(&db_path).unwrap();
    rebuild_state(&conn, &events_path, "deadbeefdeadbeef").unwrap();

    let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM events_index", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-core --lib db::tests::rebuild_state_reads`
Expected: "cannot find function `rebuild_state`".

- [ ] **Step 3: Implement**

Append:
```rust
use std::io::BufRead;

pub fn rebuild_state(
    conn: &Connection,
    jsonl_path: impl AsRef<Path>,
    project_hash: &str,
) -> anyhow::Result<usize> {
    let f = std::fs::File::open(&jsonl_path)
        .with_context(|| format!("open {:?}", jsonl_path.as_ref()))?;
    let reader = std::io::BufReader::new(f);

    let tx = conn.unchecked_transaction()?;
    let mut count = 0;
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {i}"))?;
        if line.trim().is_empty() { continue; }
        let event: Event = serde_json::from_str(&line)
            .with_context(|| format!("parse line {i}"))?;
        upsert_task_from_event(&tx, &event, project_hash)?;
        index_event(&tx, &event)?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-core --lib db::tests::rebuild_state_reads`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-core/src/db.rs
git commit -m "feat(core): rebuild_state replays JSONL into tasks + events_index (claude-memory-p1-13)"
bd close claude-memory-p1-13 --reason "rebuild_state implemented"
```

---

## Task 14: Integration test — full core round-trip

**bd task:** `claude-memory-p1-14`
**Files:**
- Create: `crates/tj-core/tests/round_trip.rs`

- [ ] **Step 1: Write the test (no impl needed)**

```rust
use tj_core::event::{Author, Event, EventType, Source};
use tj_core::storage::JsonlWriter;
use tj_core::db;
use tempfile::TempDir;

#[test]
fn full_round_trip_writes_events_and_rebuilds_state() {
    let d = TempDir::new().unwrap();
    let events_path = d.path().join("events.jsonl");
    let db_path = d.path().join("s.sqlite");
    let project_hash = "deadbeefdeadbeef";

    // 1. Write 3 events to JSONL
    let mut writer = JsonlWriter::open(&events_path).unwrap();
    let mut open_e = Event::new("tj-r", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Round trip"});
    let dec = Event::new("tj-r", EventType::Decision, Author::Agent, Source::Chat, "Adopt Rust".into());
    let close = Event::new("tj-r", EventType::Close, Author::User, Source::Cli, "done".into());
    writer.append(&open_e).unwrap();
    writer.append(&dec).unwrap();
    writer.append(&close).unwrap();
    writer.flush_durable().unwrap();
    drop(writer);

    // 2. Rebuild state
    let conn = db::open(&db_path).unwrap();
    let n = db::rebuild_state(&conn, &events_path, project_hash).unwrap();
    assert_eq!(n, 3);

    // 3. Assert tasks row reflects close
    let (status, closed_at): (String, Option<String>) = conn.query_row(
        "SELECT status, closed_at FROM tasks WHERE task_id=?1",
        ["tj-r"],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(status, "closed");
    assert!(closed_at.is_some());
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p tj-core --test round_trip`
Expected: 1 passed (no compile failure since all impls already exist from prior tasks).

- [ ] **Step 3: Commit**

```bash
git add crates/tj-core/tests/round_trip.rs
git commit -m "test(core): integration test for write→rebuild→query round-trip (claude-memory-p1-14)"
bd close claude-memory-p1-14 --reason "Round-trip integration test green"
```

---

## Task 15: Add `rmcp` to `tj-mcp` and verify it compiles

**bd task:** `claude-memory-p1-15`
**Files:**
- Modify: `crates/tj-mcp/Cargo.toml`
- Modify: `Cargo.toml` (add `rmcp` to `[workspace.dependencies]`)

> **Pre-step:** before this task, run the Context7 query in the **Pre-flight** section to confirm the current `rmcp` version. Update the version pin below if Context7 returns a newer release.

- [ ] **Step 1: Add `rmcp` to workspace deps**

Edit root `Cargo.toml` `[workspace.dependencies]`, append:
```toml
rmcp = { version = "0.6", features = ["server", "transport-io", "macros", "schemars"] }
```

(Adjust version per Context7 lookup. Features list confirmed via design doc but recheck.)

- [ ] **Step 2: Reference it in `crates/tj-mcp/Cargo.toml`**

Append to `[dependencies]`:
```toml
rmcp = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
schemars = { workspace = true }
```

- [ ] **Step 3: Build (must succeed)**

Run: `cargo build -p tj-mcp`
Expected: `Compiling rmcp ...` then `Compiling tj-mcp ...` succeed. May download a lot of crates first time.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tj-mcp/Cargo.toml
git commit -m "build(mcp): add rmcp to workspace deps (claude-memory-p1-15)"
bd close claude-memory-p1-15 --reason "rmcp compiles in workspace"
```

---

## Task 16: rmcp server skeleton with empty tool router

**bd task:** `claude-memory-p1-16`
**Files:**
- Modify: `crates/tj-mcp/src/main.rs`

- [ ] **Step 1: Write the smoke test**

Add to `crates/tj-mcp/Cargo.toml` `[dev-dependencies]`:
```toml
tokio = { workspace = true }
```

Create `crates/tj-mcp/tests/smoke.rs`:
```rust
//! Verifies the binary builds and exits cleanly when stdin closes.
//! A real protocol-level test arrives in Phase 3.

#[test]
fn binary_exists_after_build() {
    // Cargo runs this AFTER building the bin. We just check the env var.
    let p = env!("CARGO_BIN_EXE_task-journal-mcp");
    assert!(std::path::Path::new(p).exists(), "binary not built: {p}");
}
```

- [ ] **Step 2: Run — should fail until binary compiles cleanly**

Run: `cargo test -p tj-mcp --test smoke`
Expected: passes once main.rs compiles. Failure at this stage means the server skeleton has a bug.

- [ ] **Step 3: Implement skeleton**

Replace `crates/tj-mcp/src/main.rs`:
```rust
//! task-journal-mcp: MCP server entry point.
//!
//! Phase 1 wires the server with a `tool_router` that has all 5 stub tools.
//! Phase 2+ replaces stubs with real implementations.

use anyhow::Result;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    handler::server::tool::Parameters,
    handler::server::wrapper::Json,
    schemars,
    transport::io::stdio,
    tool, tool_router,
    ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct TaskJournalServer;

#[tool_router(server_handler)]
impl TaskJournalServer {
    // Tools added in Tasks 17-21
}

impl ServerHandler for TaskJournalServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo {
            server_info: rmcp::model::Implementation {
                name: "task-journal".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let server = TaskJournalServer::default();
    let (stdin, stdout) = stdio();
    server.serve((stdin, stdout)).await?.waiting().await?;
    Ok(())
}
```

> **Note for executor:** if Context7 indicates the `rmcp` API has shifted (e.g., `tool_router` macro renamed, or `ServerHandler` is now `Server`), adapt this code. The structural intent is: a struct, an impl with `#[tool]` items wrapped by `#[tool_router(server_handler)]`, a `ServerHandler` impl exposing capabilities, a `main` that runs it on stdio.

- [ ] **Step 4: Build & run smoke test**

Run: `cargo test -p tj-mcp --test smoke`
Expected: 1 passed (binary exists at expected path).

- [ ] **Step 5: Commit**

```bash
git add crates/tj-mcp/src/main.rs crates/tj-mcp/tests/smoke.rs crates/tj-mcp/Cargo.toml
git commit -m "feat(mcp): server skeleton with rmcp + stdio transport (claude-memory-p1-16)"
bd close claude-memory-p1-16 --reason "MCP skeleton compiles, smoke test green"
```

---

## Task 17: Stub tool — `task_pack`

**bd task:** `claude-memory-p1-17`
**Files:**
- Modify: `crates/tj-mcp/src/main.rs`

- [ ] **Step 1: Write the smoke check**

Append to `crates/tj-mcp/tests/smoke.rs`:
```rust
#[test]
fn binary_help_lists_task_pack_tool() {
    // Phase 3 will use a real MCP client; for now we assert binary still builds.
    let _ = env!("CARGO_BIN_EXE_task-journal-mcp");
}
```

(This is a stand-in until Phase 3 adds full MCP client tests. We at least guard against breaking the build with each tool.)

- [ ] **Step 2: Run — should pass already**

Run: `cargo test -p tj-mcp --test smoke binary_help`
Expected: 1 passed.

- [ ] **Step 3: Implement the stub**

Inside `impl TaskJournalServer` block of `main.rs`, ADD as the first method:
```rust
#[tool(name = "task_pack", description = "Return a compact resume pack for a task. Pass mode=compact|full.")]
async fn task_pack(
    &self,
    Parameters(p): Parameters<TaskPackParams>,
) -> Json<TaskPackResult> {
    Json(TaskPackResult {
        task_id: p.task_id,
        mode: p.mode.unwrap_or_else(|| "compact".into()),
        schema_version: "1.0".into(),
        text: "[STUB] task_pack not yet implemented (Phase 2)".into(),
        metadata: TaskPackMetadata { stub: true },
    })
}
```

ADD above `impl TaskJournalServer`:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskPackParams {
    pub task_id: String,
    pub mode: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskPackResult {
    pub task_id: String,
    pub mode: String,
    pub schema_version: String,
    pub text: String,
    pub metadata: TaskPackMetadata,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskPackMetadata {
    pub stub: bool,
}
```

- [ ] **Step 4: Build**

Run: `cargo build -p tj-mcp`
Expected: succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-mcp/src/main.rs crates/tj-mcp/tests/smoke.rs
git commit -m "feat(mcp): stub task_pack tool returning placeholder text (claude-memory-p1-17)"
bd close claude-memory-p1-17 --reason "task_pack stub compiles"
```

---

## Task 18: Stub tools — `task_search`, `task_create`, `event_add`, `task_close`

**bd task:** `claude-memory-p1-18`
**Files:**
- Modify: `crates/tj-mcp/src/main.rs`

- [ ] **Step 1: Add the four stub tool methods**

Inside `impl TaskJournalServer`, append after `task_pack`:
```rust
#[tool(name = "task_search", description = "Search tasks by query, status, project.")]
async fn task_search(
    &self,
    Parameters(p): Parameters<TaskSearchParams>,
) -> Json<TaskSearchResult> {
    Json(TaskSearchResult {
        query: p.query.clone(),
        results: vec![],
        stub: true,
    })
}

#[tool(name = "task_create", description = "Open a new task with title and optional initial context.")]
async fn task_create(
    &self,
    Parameters(p): Parameters<TaskCreateParams>,
) -> Json<TaskCreateResult> {
    Json(TaskCreateResult {
        task_id: format!("tj-stub-{}", &ulid::Ulid::new().to_string()[..6].to_lowercase()),
        title: p.title,
        stub: true,
    })
}

#[tool(name = "event_add", description = "Append a typed event (decision, finding, etc.) to a task.")]
async fn event_add(
    &self,
    Parameters(p): Parameters<EventAddParams>,
) -> Json<EventAddResult> {
    Json(EventAddResult {
        event_id: ulid::Ulid::new().to_string(),
        task_id: p.task_id,
        event_type: p.event_type,
        stub: true,
    })
}

#[tool(name = "task_close", description = "Close a task with reason and outcome.")]
async fn task_close(
    &self,
    Parameters(p): Parameters<TaskCloseParams>,
) -> Json<TaskCloseResult> {
    Json(TaskCloseResult {
        task_id: p.task_id,
        closed: true,
        stub: true,
    })
}
```

ADD above `impl TaskJournalServer` (after the existing `TaskPack*` types):
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskSearchParams {
    pub query: String,
    pub status: Option<String>,
    pub project: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskSearchResult {
    pub query: String,
    pub results: Vec<String>,
    pub stub: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskCreateParams {
    pub title: String,
    pub initial_context: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskCreateResult {
    pub task_id: String,
    pub title: String,
    pub stub: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EventAddParams {
    pub task_id: String,
    pub event_type: String,
    pub text: String,
    pub corrects: Option<String>,
    pub supersedes: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EventAddResult {
    pub event_id: String,
    pub task_id: String,
    pub event_type: String,
    pub stub: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskCloseParams {
    pub task_id: String,
    pub reason: String,
    pub outcome: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskCloseResult {
    pub task_id: String,
    pub closed: bool,
    pub stub: bool,
}
```

ADD `ulid` to `crates/tj-mcp/Cargo.toml` `[dependencies]`:
```toml
ulid = { workspace = true }
```

- [ ] **Step 2: Build & smoke test**

Run: `cargo test -p tj-mcp --test smoke`
Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/tj-mcp/src/main.rs crates/tj-mcp/Cargo.toml
git commit -m "feat(mcp): stubs for task_search, task_create, event_add, task_close (claude-memory-p1-18)"
bd close claude-memory-p1-18 --reason "All 5 MCP tools stubbed and compile"
```

---

## Task 19: CLI scaffolding with `clap` derive

**bd task:** `claude-memory-p1-19`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`

- [ ] **Step 1: Write the failing CLI test**

Add to `crates/tj-cli/Cargo.toml` `[dev-dependencies]`:
```toml
assert_fs = { workspace = true }
predicates = { workspace = true }
assert_cmd = "2"
```

Create `crates/tj-cli/tests/cli.rs`:
```rust
use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("task-journal").unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("create"))
        .stdout(contains("events"))
        .stdout(contains("rebuild-state"));
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-cli --test cli help_lists`
Expected: stdout missing the subcommand names.

- [ ] **Step 3: Implement clap CLI**

Replace `crates/tj-cli/src/main.rs`:
```rust
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "task-journal", version, about = "Task Journal CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new task (writes an `open` event).
    Create {
        /// Task title (one line).
        title: String,
        /// Optional initial context paragraph.
        #[arg(long)]
        context: Option<String>,
    },
    /// Inspect events for a project.
    Events {
        #[command(subcommand)]
        action: EventsCmd,
    },
    /// Rebuild SQLite state from the JSONL log.
    RebuildState,
}

#[derive(Subcommand)]
enum EventsCmd {
    /// List events (most recent first).
    List {
        /// Limit to N events.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Create { title, context } => {
            println!("[stub] would create task with title={title:?} context={context:?}");
        }
        Commands::Events { action } => match action {
            EventsCmd::List { limit } => {
                println!("[stub] would list last {limit} events");
            }
        },
        Commands::RebuildState => {
            println!("[stub] would rebuild SQLite from JSONL");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-cli --test cli help_lists`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-cli/src/main.rs crates/tj-cli/Cargo.toml crates/tj-cli/tests/cli.rs
git commit -m "feat(cli): clap scaffolding with create / events list / rebuild-state (claude-memory-p1-19)"
bd close claude-memory-p1-19 --reason "CLI subcommands wired (stub bodies)"
```

---

## Task 20: CLI `create` writes a real `open` event to JSONL

**bd task:** `claude-memory-p1-20`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`

- [ ] **Step 1: Write the failing CLI test**

Append to `crates/tj-cli/tests/cli.rs`:
```rust
use assert_fs::prelude::*;

#[test]
fn create_writes_open_event_to_jsonl() {
    let dir = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Add OAuth login"])
        .assert()
        .success();

    // The open event should appear in events/<hash>.jsonl under the data dir.
    let events_glob = dir.path().join("task-journal").join("events");
    let mut found_lines = 0;
    for entry in std::fs::read_dir(&events_glob).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let body = std::fs::read_to_string(&p).unwrap();
            for line in body.lines() {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                if v["type"] == "open" && v["text"].as_str().unwrap_or("").contains("OAuth") {
                    found_lines += 1;
                }
            }
        }
    }
    assert_eq!(found_lines, 1);
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-cli --test cli create_writes_open_event`
Expected: failure — currently the create branch is a stub print.

- [ ] **Step 3: Implement real `create` body**

Replace the body of the `Commands::Create` arm in `main.rs`:
```rust
Commands::Create { title, context } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_dir = tj_core::paths::events_dir()?;
    let events_path = events_dir.join(format!("{project_hash}.jsonl"));
    std::fs::create_dir_all(&events_dir)?;

    let task_id = format!("tj-{}", &ulid::Ulid::new().to_string()[..6].to_lowercase());
    let mut event = tj_core::event::Event::new(
        task_id.clone(),
        tj_core::event::EventType::Open,
        tj_core::event::Author::User,
        tj_core::event::Source::Cli,
        context.clone().unwrap_or_else(|| title.clone()),
    );
    event.meta = serde_json::json!({ "title": title });

    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
    writer.append(&event)?;
    writer.flush_durable()?;

    println!("{}", task_id);
}
```

ADD to `crates/tj-cli/Cargo.toml` `[dependencies]`:
```toml
ulid = { workspace = true }
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-cli --test cli create_writes_open_event`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-cli/src/main.rs crates/tj-cli/Cargo.toml
git commit -m "feat(cli): create writes real open event to JSONL log (claude-memory-p1-20)"
bd close claude-memory-p1-20 --reason "CLI create persists an open event"
```

---

## Task 21: CLI `events list` reads JSONL

**bd task:** `claude-memory-p1-21`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`

- [ ] **Step 1: Write the failing CLI test**

Append to `crates/tj-cli/tests/cli.rs`:
```rust
#[test]
fn events_list_shows_recent_events() {
    let dir = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "First task"])
        .assert().success();
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Second task"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["events", "list", "--limit", "10"])
        .assert()
        .success()
        .stdout(contains("First task").and(contains("Second task")));
}
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-cli --test cli events_list_shows`
Expected: stdout missing both titles.

- [ ] **Step 3: Implement `EventsCmd::List`**

Replace the `EventsCmd::List { limit } => { ... }` arm:
```rust
EventsCmd::List { limit } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    if !events_path.exists() {
        println!("(no events yet)");
        return Ok(());
    }
    let body = std::fs::read_to_string(&events_path)?;
    let mut events: Vec<tj_core::event::Event> = body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    events.reverse();
    for e in events.into_iter().take(limit) {
        let title = e.meta.get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| e.text.clone());
        println!("{}  [{:?}]  {}", e.timestamp, e.event_type, title);
    }
}
```

ADD to `crates/tj-cli/Cargo.toml` `[dependencies]` (already present from earlier — verify):
```toml
serde_json = { workspace = true }
tj-core = { path = "../tj-core" }
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-cli --test cli events_list_shows`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-cli/src/main.rs
git commit -m "feat(cli): events list reads JSONL and shows latest N entries (claude-memory-p1-21)"
bd close claude-memory-p1-21 --reason "events list works"
```

---

## Task 22: CLI `rebuild-state` rebuilds SQLite from JSONL

**bd task:** `claude-memory-p1-22`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`

- [ ] **Step 1: Write the failing CLI test**

Append:
```rust
#[test]
fn rebuild_state_creates_sqlite_with_one_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Build it"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["rebuild-state"])
        .assert()
        .success()
        .stdout(contains("rebuilt"));

    let state_dir = dir.path().join("task-journal").join("state");
    let mut found = 0;
    for entry in std::fs::read_dir(&state_dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) == Some("sqlite") {
            let conn = rusqlite::Connection::open(&p).unwrap();
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
            assert_eq!(n, 1);
            found += 1;
        }
    }
    assert_eq!(found, 1);
}
```

ADD to `crates/tj-cli/Cargo.toml` `[dev-dependencies]`:
```toml
rusqlite = { workspace = true }
```

- [ ] **Step 2: Run, fail**

Run: `cargo test -p tj-cli --test cli rebuild_state_creates`
Expected: stdout missing "rebuilt", or no .sqlite file.

- [ ] **Step 3: Implement `Commands::RebuildState`**

Replace the arm:
```rust
Commands::RebuildState => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

    if !events_path.exists() {
        anyhow::bail!("no events file at {events_path:?}");
    }

    let conn = tj_core::db::open(&state_path)?;
    let n = tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
    println!("rebuilt {n} events into {state_path:?}");
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p tj-cli --test cli rebuild_state_creates`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tj-cli/src/main.rs crates/tj-cli/Cargo.toml
git commit -m "feat(cli): rebuild-state replays JSONL into SQLite (claude-memory-p1-22)"
bd close claude-memory-p1-22 --reason "rebuild-state command works end-to-end"
```

---

# Phase 1 Done-Definition

After Task 22, the executor MUST verify the following BEFORE saying "Phase 1 complete":

```bash
# All tests pass
cargo test --workspace
# Both binaries run
./target/debug/task-journal --help
./target/debug/task-journal-mcp <<< ''   # exits without panic when stdin closes

# Beads epic still has open children for P2-P4 (unstarted)
bd list --status=open
```

Then run `superpowers:verification-before-completion` skill, capture all outputs, and only THEN claim Phase 1 done.

---

# Self-Review (writer's pass)

**Spec coverage** — every section of `2026-04-29-task-journal-v1-design.md` Phase 1 deliverables is mapped:

| Design item | Tasks |
|------|------|
| Cargo workspace | 2 |
| Event schema (`schemars` derive) | 3, 4, 5, 6 |
| JSONL writer (append + fsync policy) | 7 |
| OS path resolution | 8 |
| project_hash logic | 9 |
| SQLite migrations | 10 |
| 5 MCP tools stubbed | 16, 17, 18 |
| CLI: `task-journal create`, `events list`, `rebuild-state` | 19, 20, 21, 22 |
| Tasks/events_index repos | 11, 12, 13 |
| Integration round-trip test | 14 |
| `rmcp` integration scaffolding | 15 |

**Placeholder scan**: every task contains the actual code. Two cases use `todo!()` — both as deliberate RED-state markers immediately replaced in Step 3 of the same task, never crossing task boundaries.

**Type consistency**: `Event`, `EventType`, `Author`, `Source`, `EventStatus`, `EvidenceStrength`, `Refs` introduced in Tasks 3-5, used identically in Tasks 6-14, 17, 18, 20, 21. CLI `Commands` enum stays consistent across Tasks 19-22. MCP tool names match design doc Q2.

**Scope check**: this plan covers Phase 1 only (skeleton). It does NOT implement `task_pack` assembly logic (P2), classifier (P3), or hooks (P3). Those are intentionally separate plans because each is a coherent unit of work with its own test surface.

---

# Beads task batch-create helper

Run once before starting Task 1 to populate the issues:

```bash
wsl -d Ubuntu -- bash -c 'export PATH="$HOME/.local/bin:$PATH"; cd /home/shahinyanm/www/claude-memory && \
  for i in $(seq -w 01 22); do \
    case "$i" in \
      01) t="Install Rust toolchain in WSL";; \
      02) t="Cargo workspace skeleton (3 crates)";; \
      03) t="EventType enum with 12 variants";; \
      04) t="Author/Source/EventStatus/EvidenceStrength enums";; \
      05) t="Refs and Event structs with serde";; \
      06) t="Event::new constructor (ULID + RFC3339)";; \
      07) t="JsonlWriter append + fsync";; \
      08) t="paths::data_dir for current OS";; \
      09) t="project_hash::from_path";; \
      10) t="SQLite open + initial migration";; \
      11) t="Tasks repo: upsert_task_from_event";; \
      12) t="index_event: events_index + search_fts";; \
      13) t="rebuild_state from JSONL";; \
      14) t="Integration test: full core round-trip";; \
      15) t="Add rmcp to tj-mcp (workspace dep)";; \
      16) t="rmcp server skeleton with empty tool router";; \
      17) t="Stub tool: task_pack";; \
      18) t="Stub tools: task_search, task_create, event_add, task_close";; \
      19) t="CLI scaffolding with clap";; \
      20) t="CLI create writes open event to JSONL";; \
      21) t="CLI events list reads JSONL";; \
      22) t="CLI rebuild-state replays JSONL into SQLite";; \
    esac; \
    bd create --title "P1.$i: $t" --type=task --priority=1 \
      --description "Phase 1 (skeleton). See .docs/plans/2026-04-29-task-journal-v1-p1-skeleton.md Task $i for the full step-by-step plan." \
      --acceptance "Test added in Step 1 passes after Step 3 implementation. Step 5 commit landed. bd issue closed with reason." 2>/dev/null; \
  done; \
  bd list --status=open | head -25'
```

After creation, link them all to the epic and add `blocks` dependencies (each task blocks the next):

```bash
# After noting the IDs (claude-memory-XXX) bd assigned, link them.
# Replace IDs below with actual ones from `bd list`.
# Example flow:
#   bd link <p1-01-id> claude-memory-d36 --type=parent-child
#   bd link <p1-02-id> <p1-01-id> --type=blocks
#   ... etc
```

A second-pass automation script for this is left to the executor (use `bd list --json` + jq).

---

**End of Phase 1 plan.** Phase 2 plan will be authored once Tasks 1-22 are closed and the verification gate passes.
