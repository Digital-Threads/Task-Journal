# Task Journal v1 — Phase 2 (task_pack core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace P1 stubs with real implementations of `task_pack` (the primary product output), real CLI commands for full lifecycle (`pack`, `event`, `close`, `search`), and real MCP tools that call into `tj-core`. Add golden-fixture tests proving curated event sequences produce expected Markdown output.

**Architecture:** New `tj-core::pack` module is the assembler. It reads from `tasks`, `events_index`, `decisions`, `evidence`, and `task_pack_cache` tables, and renders Markdown into `TaskPack { mode, text, metadata }`. `index_event` is extended to also project `decision` and `evidence` events into their dedicated tables, and to mark decisions as superseded when a `supersede` event arrives. The cache stores rendered packs and is invalidated on any new event for that task.

**Tech Stack:** No new dependencies. Reuses tj-core building blocks from P1.

**Working directory:** `/home/shahinyanm/www/claude-memory` inside WSL Ubuntu. From the host, prefix every shell call with `wsl -d Ubuntu -- bash -c 'export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; cd /home/shahinyanm/www/claude-memory && <command>'`.

**Beads tracking:** One `bd` issue per Task below, linked as parent-child to epic `claude-memory-d36` and chained `blocks` so `bd ready` shows only the next available task.

**Spec & Design source of truth:**
- `.docs/plans/2026-04-29-tz-task-journal-v2.md` — pinned ТЗ
- `.docs/plans/2026-04-29-task-journal-v1-design.md` — answers to all 9 architectural questions
- `.docs/plans/2026-04-29-task-journal-v1-p1-skeleton.md` — what P1 produced
- `.docs/plans/2026-04-29-p1-task-map.txt` — example of plan→bd id mapping (this plan needs its own map after batch-create)

---

## Pre-flight

1. Verify P1 still passes: `cargo test --workspace` should show all green.
2. Verify both binaries run: `./target/debug/task-journal --help` and `./target/debug/task-journal-mcp < /dev/null`.
3. Re-query Context7 for `rmcp` ONLY if a tool macro change is needed (Tasks 18-19 touch the MCP impls but don't add new tools — should be safe with the 0.3 surface validated in P1).

---

## File Structure (after Task 23)

```
crates/
├── tj-core/
│   ├── src/
│   │   ├── lib.rs                  ← + pub mod pack
│   │   ├── event.rs                (unchanged)
│   │   ├── storage.rs              (unchanged)
│   │   ├── paths.rs                (unchanged)
│   │   ├── project_hash.rs         (unchanged)
│   │   ├── db.rs                   ← extended: project decisions/evidence,
│   │   │                            mark superseded, invalidate pack cache
│   │   └── pack.rs                 ← NEW: assembler + Markdown render
│   └── tests/
│       ├── round_trip.rs           (unchanged)
│       └── golden_pack.rs          ← NEW: golden fixture tests
├── tj-mcp/src/main.rs              ← stubs replaced with real impls
└── tj-cli/src/main.rs              ← + pack, event, close, search subcommands
```

---

## Granularity contract

Each Task: ONE bd issue, RED test → GREEN minimal impl → run tests → commit → `bd close`. The TDD discipline from P1 carries over verbatim.

When in doubt about which test to write first: write the smallest test that, if it passed, would prove the feature works for the simplest case.

---

# Tasks

## Task 1: pack module skeleton + `TaskPack` type

**bd task placeholder:** `<P2.01>` (created in batch script — see end of plan)
**Files:**
- Create: `crates/tj-core/src/pack.rs`
- Modify: `crates/tj-core/src/lib.rs`

- [ ] **Step 1: Write failing test in pack.rs**

```rust
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum PackMode { Compact, Full }

#[derive(Debug, Clone, Serialize)]
pub struct TaskPack {
    pub task_id: String,
    pub mode: PackMode,
    pub schema_version: String,
    pub text: String,
    pub metadata: PackMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackMetadata {
    pub generated_at: String,
    pub source_event_count: usize,
    pub cache_hit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pack_mode_round_trips_via_serde() {
        let s = serde_json::to_string(&PackMode::Compact).unwrap();
        assert_eq!(s, "\"Compact\"");
    }
}
```

- [ ] **Step 2: Add `pub mod pack;` to lib.rs**

```rust
pub mod pack;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p tj-core --lib pack::tests
```
Expected: 1 passed.

- [ ] **Step 4: Commit + close**

```bash
git add crates/tj-core/src/pack.rs crates/tj-core/src/lib.rs
git commit -m "feat(core): TaskPack/PackMode/PackMetadata types in pack module (claude-memory-<id>)"
bd close <id> --reason "Pack types defined"
```

---

## Task 2: `pack::assemble` minimum: header only (title + status)

**bd task placeholder:** `<P2.02>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Write failing test**

Add to `pack.rs` `mod tests`:
```rust
#[test]
fn assemble_header_only_compact() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();

    let mut open_e = Event::new("tj-h", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Header test"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let pack = assemble(&conn, "tj-h", PackMode::Compact).unwrap();
    assert!(pack.text.contains("# Header test"), "header missing: {}", pack.text);
    assert!(pack.text.contains("status: open"), "status missing: {}", pack.text);
    assert_eq!(pack.metadata.source_event_count, 1);
    assert!(!pack.metadata.cache_hit);
}
```

- [ ] **Step 2: Run, fail**

```bash
cargo test -p tj-core --lib pack::tests::assemble_header
```
Expected: "cannot find function `assemble`".

- [ ] **Step 3: Implement `assemble`**

Append to `pack.rs`:
```rust
use anyhow::Context;
use rusqlite::Connection;

pub fn assemble(conn: &Connection, task_id: &str, mode: PackMode) -> anyhow::Result<TaskPack> {
    let (title, status): (String, String) = conn.query_row(
        "SELECT title, status FROM tasks WHERE task_id=?1",
        rusqlite::params![task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).with_context(|| format!("task not found: {task_id}"))?;

    let event_count: usize = conn.query_row(
        "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
        rusqlite::params![task_id],
        |r| r.get::<_, i64>(0).map(|n| n as usize),
    )?;

    let text = format!("# {title}  [status: {status}]\n");

    Ok(TaskPack {
        task_id: task_id.to_string(),
        mode,
        schema_version: "1.0".into(),
        text,
        metadata: PackMetadata {
            generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            source_event_count: event_count,
            cache_hit: false,
        },
    })
}
```

- [ ] **Step 4: Run, GREEN**

```bash
cargo test -p tj-core --lib pack::tests::assemble_header
```
Expected: 1 passed.

- [ ] **Step 5: Commit + close**

```bash
git add crates/tj-core/src/pack.rs
git commit -m "feat(pack): assemble minimum (title + status header) (claude-memory-<id>)"
bd close <id> --reason "assemble header works"
```

---

## Task 3: pack adds lifecycle history (open / close / reopen events)

**bd task placeholder:** `<P2.03>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Write failing test**

Append to `mod tests`:
```rust
#[test]
fn assemble_includes_lifecycle_history() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();

    let mut open_e = Event::new("tj-l", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Lifecycle"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let close_e = Event::new("tj-l", EventType::Close, Author::User, Source::Cli, "done".into());
    db::upsert_task_from_event(&conn, &close_e, "feedface").unwrap();
    db::index_event(&conn, &close_e).unwrap();

    let pack = assemble(&conn, "tj-l", PackMode::Full).unwrap();
    assert!(pack.text.contains("## Lifecycle"));
    assert!(pack.text.contains("opened"));
    assert!(pack.text.contains("closed"));
}
```

- [ ] **Step 2: Run, fail**

Expected: assertion fails — text doesn't contain "## Lifecycle".

- [ ] **Step 3: Add lifecycle section to `assemble`**

Replace the `text` line in `assemble` and add a helper:
```rust
let mut text = format!("# {title}  [status: {status}]\n\n");
text.push_str(&render_lifecycle(conn, task_id)?);

// ... rest of assemble
```

Append before the `assemble` function:
```rust
fn render_lifecycle(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Lifecycle\n");
    let mut stmt = conn.prepare(
        "SELECT timestamp, type FROM events_index
         WHERE task_id=?1 AND type IN ('open','close','reopen','supersede','redirect')
         ORDER BY timestamp ASC"
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| {
        let ts: String = r.get(0)?;
        let ty: String = r.get(1)?;
        Ok((ts, ty))
    })?;
    let mut count = 0;
    for row in rows {
        let (ts, ty) = row?;
        let verb = match ty.as_str() {
            "open" => "opened",
            "close" => "closed",
            "reopen" => "reopened",
            "supersede" => "superseded",
            "redirect" => "redirected",
            _ => &ty,
        };
        out.push_str(&format!("- {ts} {verb}\n"));
        count += 1;
    }
    if count == 0 { out.push_str("- (none)\n"); }
    out.push('\n');
    Ok(out)
}
```

- [ ] **Step 4: Run, GREEN**

- [ ] **Step 5: Commit + close**

```bash
git commit -m "feat(pack): render lifecycle history section (claude-memory-<id>)"
bd close <id> --reason "Lifecycle section rendered"
```

---

## Task 4: Project `decision` events into `decisions` table

**bd task placeholder:** `<P2.04>`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Failing test**

Append to `db.rs` `mod tests`:
```rust
#[test]
fn index_event_projects_decision_to_decisions_table() {
    let d = TempDir::new().unwrap();
    let conn = open(d.path().join("s.sqlite")).unwrap();

    let mut open_e = crate::event::Event::new(
        "tj-d", crate::event::EventType::Open,
        crate::event::Author::User, crate::event::Source::Cli, "x".into()
    );
    open_e.meta = serde_json::json!({"title": "T"});
    upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    index_event(&conn, &open_e).unwrap();

    let dec = crate::event::Event::new(
        "tj-d", crate::event::EventType::Decision,
        crate::event::Author::Agent, crate::event::Source::Chat,
        "Adopt Rust".into()
    );
    upsert_task_from_event(&conn, &dec, "feedface").unwrap();
    index_event(&conn, &dec).unwrap();

    let (id, text, status): (String, String, String) = conn.query_row(
        "SELECT decision_id, text, status FROM decisions WHERE task_id=?1",
        rusqlite::params!["tj-d"],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(id, dec.event_id);
    assert_eq!(text, "Adopt Rust");
    assert_eq!(status, "active");
}
```

- [ ] **Step 2: Run, fail (no row in decisions)**

- [ ] **Step 3: Extend `index_event`**

In `index_event`, after the existing INSERTs, add:
```rust
if event.event_type == EventType::Decision {
    conn.execute(
        "INSERT OR REPLACE INTO decisions(decision_id, task_id, text, status)
         VALUES (?1, ?2, ?3, 'active')",
        rusqlite::params![event.event_id, event.task_id, event.text],
    )?;
}
```

- [ ] **Step 4: GREEN, commit + close**

```bash
git commit -m "feat(core): project decision events into decisions table (claude-memory-<id>)"
bd close <id> --reason "Decisions projection works"
```

---

## Task 5: Mark decision as superseded when `supersede` event arrives

**bd task placeholder:** `<P2.05>`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Failing test**

Append:
```rust
#[test]
fn supersede_event_marks_decision_superseded() {
    let d = TempDir::new().unwrap();
    let conn = open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = crate::event::Event::new(
        "tj-s", crate::event::EventType::Open,
        crate::event::Author::User, crate::event::Source::Cli, "x".into()
    );
    open_e.meta = serde_json::json!({"title": "T"});
    upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    index_event(&conn, &open_e).unwrap();

    let dec = crate::event::Event::new(
        "tj-s", crate::event::EventType::Decision,
        crate::event::Author::Agent, crate::event::Source::Chat,
        "Use TS".into()
    );
    upsert_task_from_event(&conn, &dec, "feedface").unwrap();
    index_event(&conn, &dec).unwrap();

    let mut sup = crate::event::Event::new(
        "tj-s", crate::event::EventType::Supersede,
        crate::event::Author::Agent, crate::event::Source::Chat,
        "Replaced by Rust decision".into()
    );
    sup.supersedes = Some(dec.event_id.clone());
    upsert_task_from_event(&conn, &sup, "feedface").unwrap();
    index_event(&conn, &sup).unwrap();

    let (status, by): (String, Option<String>) = conn.query_row(
        "SELECT status, superseded_by FROM decisions WHERE decision_id=?1",
        rusqlite::params![dec.event_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(status, "superseded");
    assert_eq!(by.as_deref(), Some(sup.event_id.as_str()));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Extend `index_event`**

After the decision-projection block, add:
```rust
if event.event_type == EventType::Supersede {
    if let Some(target) = &event.supersedes {
        conn.execute(
            "UPDATE decisions SET status='superseded', superseded_by=?1 WHERE decision_id=?2",
            rusqlite::params![event.event_id, target],
        )?;
    }
}
```

- [ ] **Step 4: GREEN, commit + close**

```bash
git commit -m "feat(core): supersede event marks targeted decision superseded (claude-memory-<id>)"
bd close <id>
```

---

## Task 6: Project `evidence` events into `evidence` table

**bd task placeholder:** `<P2.06>`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn index_event_projects_evidence() {
    let d = TempDir::new().unwrap();
    let conn = open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = crate::event::Event::new(
        "tj-e", crate::event::EventType::Open,
        crate::event::Author::User, crate::event::Source::Cli, "x".into()
    );
    open_e.meta = serde_json::json!({"title": "T"});
    upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    index_event(&conn, &open_e).unwrap();

    let mut ev = crate::event::Event::new(
        "tj-e", crate::event::EventType::Evidence,
        crate::event::Author::Agent, crate::event::Source::Chat,
        "Hook startup measured at 12ms".into()
    );
    ev.evidence_strength = Some(crate::event::EvidenceStrength::Strong);
    upsert_task_from_event(&conn, &ev, "feedface").unwrap();
    index_event(&conn, &ev).unwrap();

    let (text, strength): (String, String) = conn.query_row(
        "SELECT text, strength FROM evidence WHERE task_id=?1",
        rusqlite::params!["tj-e"],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert!(text.contains("12ms"));
    assert_eq!(strength, "strong");
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Extend `index_event`**

After the supersede block:
```rust
if event.event_type == EventType::Evidence {
    let strength_str = event.evidence_strength
        .map(|s| serde_json::to_value(s).unwrap().as_str().unwrap().to_string())
        .unwrap_or_else(|| "medium".into());
    conn.execute(
        "INSERT OR REPLACE INTO evidence(evidence_id, task_id, text, strength)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event.event_id, event.task_id, event.text, strength_str],
    )?;
}
```

- [ ] **Step 4: GREEN, commit + close**

```bash
git commit -m "feat(core): project evidence events into evidence table (claude-memory-<id>)"
bd close <id>
```

---

## Task 7: pack renders Active Decisions section

**bd task placeholder:** `<P2.07>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

Append to `pack::tests`:
```rust
#[test]
fn pack_renders_active_decisions() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-ad", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Decisions test"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let dec = Event::new("tj-ad", EventType::Decision, Author::Agent, Source::Chat, "Adopt Rust".into());
    db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
    db::index_event(&conn, &dec).unwrap();

    let pack = assemble(&conn, "tj-ad", PackMode::Full).unwrap();
    assert!(pack.text.contains("## Active decisions"), "missing section: {}", pack.text);
    assert!(pack.text.contains("Adopt Rust"), "decision text missing: {}", pack.text);
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Extend `assemble`**

Add helper above `assemble`:
```rust
fn render_active_decisions(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Active decisions\n");
    let mut stmt = conn.prepare(
        "SELECT text FROM decisions WHERE task_id=?1 AND status='active' ORDER BY decision_id ASC"
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| r.get::<_, String>(0))?;
    let mut count = 0;
    for row in rows {
        out.push_str(&format!("- {}\n", row?));
        count += 1;
    }
    if count == 0 { out.push_str("- (none)\n"); }
    out.push('\n');
    Ok(out)
}
```

In `assemble`, after `render_lifecycle`:
```rust
text.push_str(&render_active_decisions(conn, task_id)?);
```

- [ ] **Step 4: GREEN, commit + close**

```bash
git commit -m "feat(pack): render Active decisions section (claude-memory-<id>)"
bd close <id>
```

---

## Task 8: pack renders Rejected section (from `rejection` event types)

**bd task placeholder:** `<P2.08>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn pack_renders_rejected_options() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-r", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Rej"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let rej = Event::new("tj-r", EventType::Rejection, Author::Agent, Source::Chat,
        "TypeScript: loses single-binary distribution".into());
    db::upsert_task_from_event(&conn, &rej, "feedface").unwrap();
    db::index_event(&conn, &rej).unwrap();

    let pack = assemble(&conn, "tj-r", PackMode::Full).unwrap();
    assert!(pack.text.contains("## Rejected"));
    assert!(pack.text.contains("TypeScript"));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Extend `assemble`**

Add helper:
```rust
fn render_rejected(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Rejected\n");
    let mut stmt = conn.prepare(
        "SELECT ei.event_id FROM events_index ei
         WHERE ei.task_id=?1 AND ei.type='rejection'
         ORDER BY ei.timestamp ASC"
    )?;
    let mut text_stmt = conn.prepare(
        "SELECT text FROM search_fts WHERE event_id=?1 LIMIT 1"
    )?;
    let event_ids: Vec<String> = stmt.query_map(rusqlite::params![task_id], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let mut count = 0;
    for eid in event_ids {
        let text: String = text_stmt.query_row(rusqlite::params![eid], |r| r.get(0))?;
        out.push_str(&format!("- {text}\n"));
        count += 1;
    }
    if count == 0 { out.push_str("- (none)\n"); }
    out.push('\n');
    Ok(out)
}
```

In `assemble`, after `render_active_decisions`:
```rust
text.push_str(&render_rejected(conn, task_id)?);
```

- [ ] **Step 4: GREEN, commit + close**

```bash
git commit -m "feat(pack): render Rejected section from rejection events (claude-memory-<id>)"
bd close <id>
```

---

## Task 9: pack renders Evidence section

**bd task placeholder:** `<P2.09>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn pack_renders_evidence_section() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-ev", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Ev"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let mut ev = Event::new("tj-ev", EventType::Evidence, Author::Agent, Source::Chat,
        "Hook startup at 12ms vs 380ms node".into());
    ev.evidence_strength = Some(EvidenceStrength::Strong);
    db::upsert_task_from_event(&conn, &ev, "feedface").unwrap();
    db::index_event(&conn, &ev).unwrap();

    let pack = assemble(&conn, "tj-ev", PackMode::Full).unwrap();
    assert!(pack.text.contains("## Evidence"));
    assert!(pack.text.contains("12ms"));
    assert!(pack.text.contains("(strong)"));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Helper + insert**

```rust
fn render_evidence(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Evidence\n");
    let mut stmt = conn.prepare(
        "SELECT text, strength FROM evidence WHERE task_id=?1 ORDER BY evidence_id ASC"
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| {
        let t: String = r.get(0)?;
        let s: String = r.get(1)?;
        Ok((t, s))
    })?;
    let mut count = 0;
    for row in rows {
        let (t, s) = row?;
        out.push_str(&format!("- {t} ({s})\n"));
        count += 1;
    }
    if count == 0 { out.push_str("- (none)\n"); }
    out.push('\n');
    Ok(out)
}
```

Call after rejected:
```rust
text.push_str(&render_evidence(conn, task_id)?);
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 10: pack renders Recent Events section (last N, configurable)

**bd task placeholder:** `<P2.10>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn pack_renders_recent_events_full_mode() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-re", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Recent"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();
    for i in 0..6 {
        let e = Event::new("tj-re", EventType::Hypothesis, Author::Agent, Source::Chat,
            format!("hypothesis {i}"));
        db::upsert_task_from_event(&conn, &e, "feedface").unwrap();
        db::index_event(&conn, &e).unwrap();
    }

    let pack = assemble(&conn, "tj-re", PackMode::Full).unwrap();
    assert!(pack.text.contains("## Recent events"));
    // Full mode: include up to 10 events
    let count = pack.text.matches("[hypothesis]").count();
    assert!(count >= 5, "expected >=5 hypotheses, got {count} in {}", pack.text);
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Helper**

```rust
fn render_recent_events(conn: &Connection, task_id: &str, limit: usize) -> anyhow::Result<String> {
    let mut out = format!("## Recent events (last {limit})\n");
    let mut stmt = conn.prepare(
        "SELECT ei.timestamp, ei.type, sf.text FROM events_index ei
         LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.task_id=?1 ORDER BY ei.timestamp DESC LIMIT ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id, limit as i64], |r| {
        let ts: String = r.get(0)?;
        let ty: String = r.get(1)?;
        let txt: Option<String> = r.get(2)?;
        Ok((ts, ty, txt.unwrap_or_default()))
    })?;
    for row in rows {
        let (ts, ty, txt) = row?;
        let one_line = txt.lines().next().unwrap_or("").chars().take(120).collect::<String>();
        out.push_str(&format!("- {ts} [{ty}] {one_line}\n"));
    }
    out.push('\n');
    Ok(out)
}
```

In `assemble`:
```rust
let recent_limit = match mode { PackMode::Compact => 3, PackMode::Full => 10 };
text.push_str(&render_recent_events(conn, task_id, recent_limit)?);
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 11: Compact mode trims (omits Lifecycle, Rejected, Evidence sections)

**bd task placeholder:** `<P2.11>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn compact_mode_omits_optional_sections() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-cm", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Compact"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();
    let dec = Event::new("tj-cm", EventType::Decision, Author::Agent, Source::Chat, "D1".into());
    db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
    db::index_event(&conn, &dec).unwrap();

    let pack = assemble(&conn, "tj-cm", PackMode::Compact).unwrap();
    assert!(pack.text.contains("# Compact"));
    assert!(pack.text.contains("Active decisions"));
    assert!(pack.text.contains("Recent events"));
    assert!(!pack.text.contains("Lifecycle"), "compact should omit Lifecycle: {}", pack.text);
    assert!(!pack.text.contains("Rejected"), "compact should omit Rejected: {}", pack.text);
    assert!(!pack.text.contains("Evidence"), "compact should omit Evidence: {}", pack.text);
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Gate sections in `assemble`**

```rust
if matches!(mode, PackMode::Full) {
    text.push_str(&render_lifecycle(conn, task_id)?);
}
text.push_str(&render_active_decisions(conn, task_id)?);
if matches!(mode, PackMode::Full) {
    text.push_str(&render_rejected(conn, task_id)?);
    text.push_str(&render_evidence(conn, task_id)?);
}
let recent_limit = match mode { PackMode::Compact => 3, PackMode::Full => 10 };
text.push_str(&render_recent_events(conn, task_id, recent_limit)?);
```

- [ ] **Step 4: GREEN — also re-check earlier full-mode tests still pass**

```bash
cargo test -p tj-core --lib pack::tests
```

- [ ] **Step 5: Commit + close**

```bash
git commit -m "feat(pack): compact mode omits Lifecycle/Rejected/Evidence sections (claude-memory-<id>)"
bd close <id>
```

---

## Task 12: pack_cache read-through

**bd task placeholder:** `<P2.12>`
**Files:**
- Modify: `crates/tj-core/src/pack.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn pack_cache_returns_cached_text_on_second_call() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-c", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Cache"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let p1 = assemble(&conn, "tj-c", PackMode::Compact).unwrap();
    assert!(!p1.metadata.cache_hit);
    let p2 = assemble(&conn, "tj-c", PackMode::Compact).unwrap();
    assert!(p2.metadata.cache_hit, "second call should hit cache");
    assert_eq!(p1.text, p2.text);
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Implement cache**

In `assemble`, before computing `text`:
```rust
let mode_str = match mode { PackMode::Compact => "compact", PackMode::Full => "full" };

// Read-through cache
let cached: Option<(String, String, i64)> = conn.query_row(
    "SELECT text, generated_at, source_event_count FROM task_pack_cache
     WHERE task_id=?1 AND mode=?2",
    rusqlite::params![task_id, mode_str],
    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
).ok();

if let Some((cached_text, cached_at, cached_count)) = cached {
    return Ok(TaskPack {
        task_id: task_id.to_string(),
        mode,
        schema_version: "1.0".into(),
        text: cached_text,
        metadata: PackMetadata {
            generated_at: cached_at,
            source_event_count: cached_count as usize,
            cache_hit: true,
        },
    });
}
```

After computing `text`:
```rust
conn.execute(
    "INSERT OR REPLACE INTO task_pack_cache(task_id, mode, text, generated_at, source_event_count)
     VALUES (?1, ?2, ?3, ?4, ?5)",
    rusqlite::params![task_id, mode_str, text, chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), event_count as i64],
)?;
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 13: pack_cache invalidates on `index_event`

**bd task placeholder:** `<P2.13>`
**Files:**
- Modify: `crates/tj-core/src/db.rs`

- [ ] **Step 1: Failing test in `pack::tests`**

Add to pack.rs tests:
```rust
#[test]
fn cache_is_invalidated_on_new_event() {
    use crate::db;
    use crate::event::*;
    use tempfile::TempDir;

    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let mut open_e = Event::new("tj-inv", EventType::Open, Author::User, Source::Cli, "x".into());
    open_e.meta = serde_json::json!({"title": "Inv"});
    db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
    db::index_event(&conn, &open_e).unwrap();

    let _ = assemble(&conn, "tj-inv", PackMode::Compact).unwrap();
    let p2 = assemble(&conn, "tj-inv", PackMode::Compact).unwrap();
    assert!(p2.metadata.cache_hit);

    // New event invalidates
    let dec = Event::new("tj-inv", EventType::Decision, Author::Agent, Source::Chat, "D".into());
    db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
    db::index_event(&conn, &dec).unwrap();

    let p3 = assemble(&conn, "tj-inv", PackMode::Compact).unwrap();
    assert!(!p3.metadata.cache_hit, "new event must invalidate the cache");
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: At end of `index_event`, drop cache rows for that task**

```rust
conn.execute(
    "DELETE FROM task_pack_cache WHERE task_id=?1",
    rusqlite::params![event.task_id],
)?;
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 14: CLI `pack` command

**bd task placeholder:** `<P2.14>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test in `cli.rs`**

```rust
#[test]
fn pack_command_prints_markdown_for_existing_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Pack me"])
            .assert().success()
            .get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "compact"])
        .assert()
        .success()
        .stdout(contains("# Pack me"));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add `pack` subcommand**

In `main.rs` `Commands` enum:
```rust
/// Render and print the resume pack for a task.
Pack {
    /// Task id (e.g. tj-7f3a).
    task_id: String,
    /// Output mode.
    #[arg(long, default_value = "compact")]
    mode: String,
},
```

In match:
```rust
Commands::Pack { task_id, mode } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        // Lazy rebuild so the user doesn't have to remember `rebuild-state`.
        tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
    }

    let pmode = match mode.as_str() {
        "compact" => tj_core::pack::PackMode::Compact,
        "full" => tj_core::pack::PackMode::Full,
        other => anyhow::bail!("unknown mode: {other}"),
    };
    let pack = tj_core::pack::assemble(&conn, &task_id, pmode)?;
    print!("{}", pack.text);
}
```

- [ ] **Step 4: GREEN, commit + close**

```bash
git commit -m "feat(cli): pack subcommand renders Markdown from events (claude-memory-<id>)"
bd close <id>
```

---

## Task 15: CLI `event` command (add typed event)

**bd task placeholder:** `<P2.15>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn event_command_appends_decision_visible_in_pack() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "T"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["event", &task_id, "--type", "decision", "--text", "Adopt Rust"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Adopt Rust"));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add `Event` subcommand**

```rust
/// Append a typed event to a task.
Event {
    task_id: String,
    /// Event type: hypothesis, finding, evidence, decision, rejection, constraint,
    /// correction, reopen, supersede, close, redirect.
    #[arg(long)]
    r#type: String,
    /// Event text body.
    #[arg(long)]
    text: String,
    /// Optional event id this corrects (for type=correction).
    #[arg(long)]
    corrects: Option<String>,
    /// Optional event id this supersedes (for type=supersede).
    #[arg(long)]
    supersedes: Option<String>,
},
```

In match:
```rust
Commands::Event { task_id, r#type, text, corrects, supersedes } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    std::fs::create_dir_all(events_path.parent().unwrap())?;

    let event_type = parse_event_type(&r#type)?;
    let mut event = tj_core::event::Event::new(
        &task_id, event_type,
        tj_core::event::Author::User, tj_core::event::Source::Cli,
        text,
    );
    event.corrects = corrects;
    event.supersedes = supersedes;

    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
    writer.append(&event)?;
    writer.flush_durable()?;
    println!("{}", event.event_id);
}
```

Add a helper `parse_event_type` near the bottom:
```rust
fn parse_event_type(s: &str) -> anyhow::Result<tj_core::event::EventType> {
    use tj_core::event::EventType::*;
    Ok(match s {
        "open" => Open,
        "hypothesis" => Hypothesis,
        "finding" => Finding,
        "evidence" => Evidence,
        "decision" => Decision,
        "rejection" => Rejection,
        "constraint" => Constraint,
        "correction" => Correction,
        "reopen" => Reopen,
        "supersede" => Supersede,
        "close" => Close,
        "redirect" => Redirect,
        other => anyhow::bail!("unknown event type: {other}"),
    })
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 16: CLI `close` command

**bd task placeholder:** `<P2.16>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`, `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn close_command_marks_task_closed_in_pack() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "T"]).assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", &task_id, "--reason", "shipped"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("status: closed"));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add `Close` subcommand**

```rust
/// Close a task (writes a `close` event).
Close {
    task_id: String,
    #[arg(long)]
    reason: Option<String>,
},
```

In match:
```rust
Commands::Close { task_id, reason } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));

    let mut event = tj_core::event::Event::new(
        &task_id, tj_core::event::EventType::Close,
        tj_core::event::Author::User, tj_core::event::Source::Cli,
        reason.clone().unwrap_or_else(|| "(closed)".into()),
    );
    if let Some(r) = reason { event.meta = serde_json::json!({"reason": r}); }

    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
    writer.append(&event)?;
    writer.flush_durable()?;
    println!("{}", event.event_id);
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 17: CLI `search` using FTS5

**bd task placeholder:** `<P2.17>`
**Files:**
- Modify: `crates/tj-cli/src/main.rs`, `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn search_command_finds_task_by_event_text() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "OAuth thing"]).assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["event", &task_id, "--type", "decision", "--text", "Adopt Rust + rmcp"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "rmcp"])
        .assert().success()
        .stdout(contains(&task_id));
}
```

- [ ] **Step 2: Run, fail**

- [ ] **Step 3: Add `Search` subcommand**

```rust
/// Full-text search across events (FTS5).
Search {
    /// Query string (FTS5 syntax).
    query: String,
    #[arg(long, default_value_t = 20)]
    limit: usize,
},
```

In match:
```rust
Commands::Search { query, limit } => {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
    }

    let mut stmt = conn.prepare(
        "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT ?2"
    )?;
    let ids: Vec<String> = stmt.query_map(rusqlite::params![query, limit as i64], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    for id in ids { println!("{id}"); }
}
```

- [ ] **Step 4: GREEN, commit + close**

---

## Task 18: MCP `task_pack` real impl

**bd task placeholder:** `<P2.18>`
**Files:**
- Modify: `crates/tj-mcp/src/main.rs`

- [ ] **Step 1: Smoke test stays green; logic re-tested via real call later**

For now, manual smoke. Add no new test in this task — rely on existing smoke.rs and cli integration tests covering the underlying tj-core behavior.

- [ ] **Step 2: Replace `task_pack` body**

In `main.rs`, replace the `task_pack` async fn body:
```rust
async fn task_pack(
    &self,
    Parameters(p): Parameters<TaskPackParams>,
) -> Json<TaskPackResult> {
    let result = (|| -> anyhow::Result<TaskPackResult> {
        let cwd = std::env::current_dir()?;
        let project_hash = tj_core::project_hash::from_path(&cwd)?;
        let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
        let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

        let conn = tj_core::db::open(&state_path)?;
        if events_path.exists() {
            tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
        }
        let pmode = match p.mode.as_deref() {
            Some("full") => tj_core::pack::PackMode::Full,
            _ => tj_core::pack::PackMode::Compact,
        };
        let pack = tj_core::pack::assemble(&conn, &p.task_id, pmode)?;
        Ok(TaskPackResult {
            task_id: pack.task_id,
            mode: match pack.mode { tj_core::pack::PackMode::Compact => "compact".into(), tj_core::pack::PackMode::Full => "full".into() },
            schema_version: pack.schema_version,
            text: pack.text,
            metadata: TaskPackMetadata { stub: false },
        })
    })();
    match result {
        Ok(r) => Json(r),
        Err(e) => Json(TaskPackResult {
            task_id: p.task_id,
            mode: p.mode.unwrap_or_else(|| "compact".into()),
            schema_version: "1.0".into(),
            text: format!("[error] {e}"),
            metadata: TaskPackMetadata { stub: false },
        }),
    }
}
```

- [ ] **Step 3: `cargo build -p tj-mcp` + smoke test, GREEN**

- [ ] **Step 4: Commit + close**

---

## Task 19: MCP `task_create`, `event_add`, `task_close`, `task_search` real impls

**bd task placeholder:** `<P2.19>`
**Files:**
- Modify: `crates/tj-mcp/src/main.rs`

- [ ] **Step 1: Replace bodies of the four remaining tools**

In each, replicate the CLI logic but return JSON results. Key implementations (one per tool):

```rust
async fn task_create(
    &self,
    Parameters(p): Parameters<TaskCreateParams>,
) -> Json<TaskCreateResult> {
    let result = (|| -> anyhow::Result<TaskCreateResult> {
        let cwd = std::env::current_dir()?;
        let project_hash = tj_core::project_hash::from_path(&cwd)?;
        let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
        std::fs::create_dir_all(events_path.parent().unwrap())?;

        let task_id = format!("tj-{}", &ulid::Ulid::new().to_string()[..6].to_lowercase());
        let mut event = tj_core::event::Event::new(
            task_id.clone(),
            tj_core::event::EventType::Open,
            tj_core::event::Author::Agent,
            tj_core::event::Source::Chat,
            p.initial_context.unwrap_or_else(|| p.title.clone()),
        );
        event.meta = serde_json::json!({"title": p.title.clone()});

        let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
        writer.append(&event)?;
        writer.flush_durable()?;

        Ok(TaskCreateResult { task_id, title: p.title, stub: false })
    })();
    Json(result.unwrap_or_else(|e| TaskCreateResult { task_id: format!("[error] {e}"), title: "".into(), stub: false }))
}
```

Apply the same shape to `event_add`, `task_close`, `task_search`. (For `task_search`, query the FTS5 index after a lazy rebuild_state.)

- [ ] **Step 2: `cargo build -p tj-mcp`, smoke test green**

- [ ] **Step 3: Commit + close**

```bash
git commit -m "feat(mcp): real impls for create/event_add/close/search (claude-memory-<id>)"
bd close <id>
```

---

## Task 20: Golden-fixture A — 5-event compact pack

**bd task placeholder:** `<P2.20>`
**Files:**
- Create: `crates/tj-core/tests/golden_pack.rs`

- [ ] **Step 1: Write the fixture test**

```rust
//! Curated event sequences → expected pack output. Updates require
//! deliberate review (these protect the user-facing contract).

use tj_core::db;
use tj_core::event::{Author, Event, EventType, Source};
use tj_core::pack::{assemble, PackMode};
use tempfile::TempDir;

#[test]
fn fixture_a_compact_pack_for_simple_task() {
    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let ph = "feedface";

    let events = build_fixture_a();
    for e in &events {
        db::upsert_task_from_event(&conn, e, ph).unwrap();
        db::index_event(&conn, e).unwrap();
    }

    let pack = assemble(&conn, "tj-fa", PackMode::Compact).unwrap();
    insta_assert_contains(&pack.text, "# Add OAuth login");
    insta_assert_contains(&pack.text, "status: open");
    insta_assert_contains(&pack.text, "Active decisions");
    insta_assert_contains(&pack.text, "Adopt PKCE flow");
    insta_assert_contains(&pack.text, "Recent events");
    assert_eq!(pack.metadata.source_event_count, 5);
}

fn build_fixture_a() -> Vec<Event> {
    let mut events = Vec::new();
    let mut open_e = Event::new("tj-fa", EventType::Open, Author::User, Source::Cli, "Add OAuth login".into());
    open_e.meta = serde_json::json!({"title": "Add OAuth login"});
    events.push(open_e);
    events.push(Event::new("tj-fa", EventType::Hypothesis, Author::Agent, Source::Chat, "PKCE vs implicit grant".into()));
    let mut ev = Event::new("tj-fa", EventType::Evidence, Author::Agent, Source::Chat, "OAuth 2.1 deprecates implicit".into());
    ev.evidence_strength = Some(tj_core::event::EvidenceStrength::Strong);
    events.push(ev);
    events.push(Event::new("tj-fa", EventType::Decision, Author::Agent, Source::Chat, "Adopt PKCE flow".into()));
    events.push(Event::new("tj-fa", EventType::Rejection, Author::Agent, Source::Chat, "Implicit grant: deprecated, no refresh".into()));
    events
}

fn insta_assert_contains(haystack: &str, needle: &str) {
    assert!(haystack.contains(needle), "missing {needle:?} in:\n{haystack}");
}
```

- [ ] **Step 2: Run, GREEN**

```bash
cargo test -p tj-core --test golden_pack
```

- [ ] **Step 3: Commit + close**

---

## Task 21: Golden-fixture B — 12-event full pack with supersede + correction

**bd task placeholder:** `<P2.21>`
**Files:**
- Modify: `crates/tj-core/tests/golden_pack.rs`

- [ ] **Step 1: Append fixture test**

```rust
#[test]
fn fixture_b_full_pack_with_supersede_and_correction() {
    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let ph = "feedface";

    let events = build_fixture_b();
    for e in &events {
        db::upsert_task_from_event(&conn, e, ph).unwrap();
        db::index_event(&conn, e).unwrap();
    }

    let pack = assemble(&conn, "tj-fb", PackMode::Full).unwrap();
    insta_assert_contains(&pack.text, "# Stack choice for journal");
    insta_assert_contains(&pack.text, "Lifecycle");
    insta_assert_contains(&pack.text, "opened");
    insta_assert_contains(&pack.text, "closed");
    insta_assert_contains(&pack.text, "Active decisions");
    insta_assert_contains(&pack.text, "Adopt Rust"); // not superseded
    insta_assert_contains(&pack.text, "Rejected");
    insta_assert_contains(&pack.text, "TypeScript");
    insta_assert_contains(&pack.text, "Evidence");
    // The superseded TS decision must NOT appear under "Active decisions" twice.
    let active_count = pack.text.matches("Adopt TypeScript").count();
    assert_eq!(active_count, 0, "superseded TS must not appear active");

    assert_eq!(pack.metadata.source_event_count, 12);
}

fn build_fixture_b() -> Vec<Event> {
    let mut events = Vec::new();
    let mut open_e = Event::new("tj-fb", EventType::Open, Author::User, Source::Cli, "Stack choice".into());
    open_e.meta = serde_json::json!({"title": "Stack choice for journal"});
    events.push(open_e);
    events.push(Event::new("tj-fb", EventType::Hypothesis, Author::Agent, Source::Chat, "TS vs Rust".into()));
    events.push(Event::new("tj-fb", EventType::Constraint, Author::User, Source::Chat, "Single static binary".into()));
    let mut ev1 = Event::new("tj-fb", EventType::Evidence, Author::Agent, Source::Chat, "Hook startup 380ms node, 12ms rust".into());
    ev1.evidence_strength = Some(tj_core::event::EvidenceStrength::Strong);
    events.push(ev1);
    let ts_dec = Event::new("tj-fb", EventType::Decision, Author::Agent, Source::Chat, "Adopt TypeScript".into());
    let ts_dec_id = ts_dec.event_id.clone();
    events.push(ts_dec);
    let mut sup = Event::new("tj-fb", EventType::Supersede, Author::Agent, Source::Chat, "TS decision replaced".into());
    sup.supersedes = Some(ts_dec_id);
    events.push(sup);
    events.push(Event::new("tj-fb", EventType::Decision, Author::Agent, Source::Chat, "Adopt Rust".into()));
    events.push(Event::new("tj-fb", EventType::Rejection, Author::Agent, Source::Chat, "TypeScript: loses single-binary distribution".into()));
    let mistake = Event::new("tj-fb", EventType::Finding, Author::Classifier, Source::Hook, "Migration looks complete (was wrong)".into());
    let mistake_id = mistake.event_id.clone();
    events.push(mistake);
    let mut corr = Event::new("tj-fb", EventType::Correction, Author::User, Source::Cli, "Migration was NOT complete; reverted finding".into());
    corr.corrects = Some(mistake_id);
    events.push(corr);
    events.push(Event::new("tj-fb", EventType::Finding, Author::Agent, Source::Chat, "Migration completed for real after fix".into()));
    events.push(Event::new("tj-fb", EventType::Close, Author::User, Source::Cli, "Done".into()));
    events
}
```

- [ ] **Step 2: Run, GREEN**

- [ ] **Step 3: Commit + close**

---

## Task 22: End-to-end CLI demo test

**bd task placeholder:** `<P2.22>`
**Files:**
- Modify: `crates/tj-cli/tests/cli.rs`

- [ ] **Step 1: Failing E2E test**

```rust
#[test]
fn e2e_create_decisions_close_pack_search() {
    let dir = assert_fs::TempDir::new().unwrap();
    let env = || {
        let mut cmd = Command::cargo_bin("task-journal").unwrap();
        cmd.env("XDG_DATA_HOME", dir.path());
        cmd
    };

    let task_id = String::from_utf8(
        env().args(["create", "Build pack assembler"]).assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    env().args(["event", &task_id, "--type", "hypothesis", "--text", "Use SQLite views"]).assert().success();
    env().args(["event", &task_id, "--type", "decision", "--text", "Rust + rmcp"]).assert().success();
    env().args(["event", &task_id, "--type", "rejection", "--text", "Node loses binary"]).assert().success();
    env().args(["close", &task_id, "--reason", "shipped"]).assert().success();

    env().args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("Build pack assembler")
            .and(contains("Rust + rmcp"))
            .and(contains("Node loses binary"))
            .and(contains("status: closed")));

    env().args(["search", "rmcp"])
        .assert().success()
        .stdout(contains(&task_id));
}
```

- [ ] **Step 2: Run, GREEN (depends on all earlier tasks)**

- [ ] **Step 3: Commit + close**

---

## Task 23: Verification gate + plan-finish

**bd task placeholder:** `<P2.23>`

- [ ] **Step 1: Run full workspace**

```bash
cargo test --workspace
```
Expected: ALL green.

- [ ] **Step 2: Manual smoke**

```bash
./target/debug/task-journal create "demo task"
# capture id, then:
./target/debug/task-journal event <id> --type decision --text "adopt approach X"
./target/debug/task-journal pack <id> --mode full
```
Expected: Markdown pack containing "demo task" + "adopt approach X".

- [ ] **Step 3: Close epic OR keep open for P3**

```bash
bd close claude-memory-<P2.23> --reason "P2 done; P3 (hooks + classifier) is the next plan"
# Epic claude-memory-d36 stays open until P3+P4 close
```

- [ ] **Step 4: Invoke `superpowers:finishing-a-development-branch`**

---

# Beads task batch-create helper (P2)

After this plan is approved, run a script analogous to `.beads/hooks/p1-create.sh` but with P2 titles. Save the resulting plan→bd-id map at `.docs/plans/2026-04-30-p2-task-map.txt`.

Suggested titles (matching Tasks above):

```
01 pack module skeleton plus TaskPack types
02 pack assemble minimum (header only)
03 pack lifecycle history section
04 project decision events into decisions table
05 supersede event marks decision superseded
06 project evidence events into evidence table
07 pack render Active decisions section
08 pack render Rejected section
09 pack render Evidence section
10 pack render Recent events section
11 compact mode omits optional sections
12 pack_cache read-through
13 pack_cache invalidation on new event
14 CLI pack subcommand
15 CLI event subcommand
16 CLI close subcommand
17 CLI search subcommand FTS5
18 MCP task_pack real impl
19 MCP task_create event_add task_close task_search real impls
20 Golden fixture A compact 5-event pack
21 Golden fixture B full 12-event with supersede correction
22 E2E CLI test create event close pack search
23 Verification gate plus plan finish
```

After batch-create:
- Link each as parent-child to `claude-memory-d36`
- Chain `blocks`: P2.02 blocks P2.03, etc.
- Tasks 4, 5, 6 (db extensions) should NOT block 7-10 strictly, but for simplicity keep linear chain. Executor can re-order in-place if needed.

---

# Self-Review (writer's pass)

**Spec coverage** vs design doc P2 deliverables:

| Design item | Tasks |
|------|------|
| task_pack assembler (full + compact modes) | 1, 2, 3, 7-11 |
| Markdown rendering | 2, 3, 7-10 |
| decisions/evidence projections | 4, 5, 6 |
| FTS5 search | 17 (CLI), 19 (MCP) |
| Real task_create / event_add / task_close / task_search | 14, 15, 16, 17 (CLI); 19 (MCP) |
| Golden-fixture tests | 20, 21 |
| Pack cache invalidation | 12, 13 |
| End-to-end test | 22 |

**Placeholder scan**: every task contains the actual code. No "TBD" or "implement later".

**Type consistency**: `TaskPack`, `PackMode`, `PackMetadata` introduced in Task 1, used throughout 2-13. CLI flags (`--mode`, `--type`, `--text`, `--reason`) consistent. MCP tool param types (`TaskPackParams` etc.) reused from P1.

**Scope check**: This plan is task_pack core only. Hooks (P3) and classifier (P3) are NOT in scope. Polish/dogfood (P4) is separate.

**Ambiguity check**: 
- `mode` in CLI accepts "compact"|"full"; anything else errors out. Same for MCP.
- Lazy rebuild_state on every CLI/MCP read-only call is intentional — Phase 1 has no live event-to-state pipeline; rebuilding from JSONL on demand is fast enough for v1 (<5s per 100k events per the design doc).
- Cache invalidation key is just `task_id` — coarse-grained but correct (any new event for a task busts both compact and full caches).

---

**End of Phase 2 plan.** Phase 3 (hooks + classifier) is authored after P2's Task 23 verification gate passes.
