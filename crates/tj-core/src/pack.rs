//! Pack assembler: turns events + derived state into compact resume Markdown.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackMode {
    Compact,
    Full,
}

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
    pub truncated: bool,
}

use anyhow::Context;
use rusqlite::Connection;

fn render_recent_events(conn: &Connection, task_id: &str, limit: usize) -> anyhow::Result<String> {
    let mut out = format!("## Recent events (last {limit})\n");
    let mut stmt = conn.prepare(
        "SELECT ei.timestamp, ei.type, ei.status, sf.text FROM events_index ei
         LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.task_id=?1 ORDER BY ei.timestamp DESC LIMIT ?2",
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
        let one_line = txt
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        let marker = if st == "suggested" { " [?]" } else { "" };
        out.push_str(&format!("- {ts} [{ty}]{marker} {one_line}\n"));
    }
    out.push('\n');
    Ok(out)
}

fn render_evidence(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Evidence\n");
    // v0.10.3: newest evidence first (ULID DESC). Matches the
    // decision-ordering fix so truncation prefers older rows.
    let mut stmt = conn.prepare(
        "SELECT text, strength FROM evidence WHERE task_id=?1 ORDER BY evidence_id DESC",
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
    if count == 0 {
        out.push_str("- (none)\n");
    }
    out.push('\n');
    Ok(out)
}

fn render_rejected(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Rejected\n");
    // v0.10.3: newest-first so end-of-pack truncation drops the
    // OLDEST rejections, not the latest decision the agent recorded.
    let mut id_stmt = conn.prepare(
        "SELECT event_id FROM events_index
         WHERE task_id=?1 AND type='rejection'
         ORDER BY timestamp DESC",
    )?;
    let mut text_stmt = conn.prepare("SELECT text FROM search_fts WHERE event_id=?1 LIMIT 1")?;
    let event_ids: Vec<String> = id_stmt
        .query_map(rusqlite::params![task_id], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let mut count = 0;
    for eid in event_ids {
        let text: String = text_stmt.query_row(rusqlite::params![eid], |r| r.get(0))?;
        out.push_str(&format!("- {text}\n"));
        count += 1;
    }
    if count == 0 {
        out.push_str("- (none)\n");
    }
    out.push('\n');
    Ok(out)
}

fn render_active_decisions(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Active decisions\n");
    // v0.10.3: newest decision first. `decision_id` is a ULID so DESC
    // gives reverse-chronological order. The summary/final-decision
    // event the agent records just before close is now the FIRST line
    // of this section, surviving end-of-pack truncation.
    let mut stmt = conn.prepare(
        "SELECT text, alternatives FROM decisions WHERE task_id=?1 AND status='active' ORDER BY decision_id DESC",
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (text, alternatives) = row?;
        out.push_str(&format!("- {text}\n"));
        // v0.12.0: structured alternatives render under the decision so the
        // pack shows "considered A/B/C, chose X" without reconstructing it
        // from the hypothesis+rejection chain.
        if let Some(block) = render_alternatives(alternatives.as_deref()) {
            out.push_str(&block);
        }
        count += 1;
    }
    if count == 0 {
        out.push_str("- (none)\n");
    }
    out.push('\n');
    Ok(out)
}

/// Render a decision's `meta.alternatives` JSON as indented bullet lines.
/// Returns `None` when absent or malformed (the decision still renders
/// without the block — a bad alternatives payload never hides the choice).
/// Each entry is `{option, chosen?, rationale?}`; the chosen option is
/// marked so the final choice is unambiguous.
fn render_alternatives(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let arr = parsed.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut block = String::from("  - considered:\n");
    for entry in arr {
        let option = entry.get("option").and_then(|v| v.as_str())?;
        let chosen = entry
            .get("chosen")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let marker = if chosen { "✓ chose" } else { "✗" };
        let rationale = entry.get("rationale").and_then(|v| v.as_str());
        match rationale {
            Some(r) => block.push_str(&format!("    - {marker} {option} — {r}\n")),
            None => block.push_str(&format!("    - {marker} {option}\n")),
        }
    }
    Some(block)
}

fn render_lifecycle(conn: &Connection, task_id: &str) -> anyhow::Result<String> {
    let mut out = String::from("## Lifecycle\n");
    let mut stmt = conn.prepare(
        "SELECT timestamp, type FROM events_index
         WHERE task_id=?1 AND type IN ('open','close','reopen','supersede','redirect')
         ORDER BY timestamp ASC",
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
    if count == 0 {
        out.push_str("- (none)\n");
    }
    out.push('\n');
    Ok(out)
}

/// Truncate `text` to at most `budget` bytes, cutting at a UTF-8 char
/// boundary and preferring the last newline within the kept prefix, then
/// append `marker`. Char-boundary-safe: a raw `text[..budget]` byte slice
/// panics when `budget` lands inside a multibyte char (Cyrillic/CJK/emoji).
/// Render a compact one-level roll-up of a task's direct children, or None
/// when it has no children. Each child: status, id, title. Bounded.
fn render_subtasks(conn: &Connection, task_id: &str) -> anyhow::Result<Option<String>> {
    let kids = crate::db::children_of(conn, task_id)?;
    if kids.is_empty() {
        return Ok(None);
    }
    let mut s = format!("\n## Subtasks ({})\n", kids.len());
    for k in &kids {
        s.push_str(&format!("- [{}] {} — {}\n", k.status, k.task_id, k.title));
    }
    Ok(Some(s))
}

fn truncate_to_budget(text: &mut String, budget: usize, marker: &str) {
    if text.len() <= budget {
        return;
    }
    let mut end = budget;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let cutoff = text[..end].rfind('\n').unwrap_or(end);
    text.truncate(cutoff);
    text.push_str(marker);
}

pub fn assemble(conn: &Connection, task_id: &str, mode: PackMode) -> anyhow::Result<TaskPack> {
    let mode_str = match mode {
        PackMode::Compact => "compact",
        PackMode::Full => "full",
    };

    // Read-through cache: if we have a stored pack with the same mode, return it.
    let cached: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT text, generated_at, source_event_count FROM task_pack_cache
         WHERE task_id=?1 AND mode=?2",
            rusqlite::params![task_id, mode_str],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    if let Some((cached_text, cached_at, cached_count)) = cached {
        // Detect truncation by re-checking for the marker.
        let was_truncated = cached_text.contains("_(truncated to fit pack budget)_");
        return Ok(TaskPack {
            task_id: task_id.to_string(),
            mode,
            schema_version: crate::SCHEMA_VERSION.into(),
            text: cached_text,
            metadata: PackMetadata {
                generated_at: cached_at,
                source_event_count: cached_count as usize,
                cache_hit: true,
                truncated: was_truncated,
            },
        });
    }

    let (title, status, goal, outcome, outcome_tag, external): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT title, status, goal, outcome, outcome_tag, external FROM tasks WHERE task_id=?1",
            rusqlite::params![task_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .with_context(|| format!("task not found: {task_id}"))?;

    let event_count: usize = conn.query_row(
        "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
        rusqlite::params![task_id],
        |r| r.get::<_, i64>(0).map(|n| n as usize),
    )?;

    let mut text = format!("# {title}  [status: {status}]\n\n");

    // v0.4.0 task-as-goal block. Goal renders even when empty so the
    // shape is consistent and the absence is visible. Outcome only
    // when closed (avoids "(open)" noise on every active task).
    // External only when populated.
    let goal_str = goal.as_deref().unwrap_or("(not set)");
    text.push_str(&format!("**Goal**: {goal_str}\n"));
    if status == "closed" {
        let outcome_str = outcome.as_deref().unwrap_or("(not recorded)");
        let tag = outcome_tag
            .as_deref()
            .map(|t| format!(" [{t}]"))
            .unwrap_or_default();
        text.push_str(&format!("**Outcome**{tag}: {outcome_str}\n"));
    }
    if let Some(ext) = external.as_deref().filter(|s| !s.is_empty()) {
        // Split out `linked:tj-xxx` entries so the user sees the
        // task-graph dimension separately from PRs / commit hashes /
        // beads-ids. Other refs stay in External; linked entries get
        // their own block annotated with the live status of each
        // pointer.
        let (linked, other): (Vec<_>, Vec<_>) = ext
            .split(',')
            .map(|s| s.trim())
            .partition(|s| s.starts_with("linked:") || s.starts_with("linked: "));
        if !other.is_empty() {
            text.push_str(&format!(
                "**External**: {}\n",
                other
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        if !linked.is_empty() {
            text.push_str("**Linked**:\n");
            for entry in linked {
                let id = entry.trim_start_matches("linked:").trim();
                let st: Option<String> = conn
                    .query_row(
                        "SELECT status FROM tasks WHERE task_id = ?1",
                        rusqlite::params![id],
                        |r| r.get(0),
                    )
                    .ok();
                match st {
                    Some(s) => text.push_str(&format!("- {id} [{s}]\n")),
                    None => text.push_str(&format!("- {id} [unknown]\n")),
                }
            }
        }
    }

    // v0.5.0 Phase B: artifacts auto-extracted from event text. Render
    // only categories that have entries — empty groups are noise on a
    // 30-event task. Order is stable so packs diff cleanly.
    let arts = crate::db::task_artifacts(conn, task_id)?;
    if !arts.is_empty() {
        text.push_str("**Artifacts**:\n");
        if !arts.commit_hashes.is_empty() {
            text.push_str(&format!("- commits: {}\n", arts.commit_hashes.join(", ")));
        }
        if !arts.pr_urls.is_empty() {
            text.push_str(&format!("- PRs: {}\n", arts.pr_urls.join(", ")));
        }
        if !arts.linked_issues.is_empty() {
            text.push_str(&format!("- issues: {}\n", arts.linked_issues.join(", ")));
        }
        if !arts.files.is_empty() {
            text.push_str(&format!("- files: {}\n", arts.files.join(", ")));
        }
        if !arts.branch_names.is_empty() {
            text.push_str(&format!("- branches: {}\n", arts.branch_names.join(", ")));
        }
    }
    text.push('\n');

    if matches!(mode, PackMode::Full) {
        text.push_str(&render_lifecycle(conn, task_id)?);
    }
    text.push_str(&render_active_decisions(conn, task_id)?);
    if matches!(mode, PackMode::Full) {
        text.push_str(&render_rejected(conn, task_id)?);
        text.push_str(&render_evidence(conn, task_id)?);
    }
    let recent_limit = match mode {
        PackMode::Compact => 3,
        PackMode::Full => 10,
    };
    text.push_str(&render_recent_events(conn, task_id, recent_limit)?);

    let report = crate::completeness::assess(conn, task_id, crate::completeness::pending_count())?;
    if let Some(section) = crate::completeness::render_section(&report) {
        text.push_str(&section);
    }

    // One-level roll-up of direct children (parents only). Appended before
    // truncation so it shares the pack budget. Task 5 busts the parent cache
    // when a child changes, so the next assemble regenerates fresh.
    if let Some(subtasks) = render_subtasks(conn, task_id)? {
        text.push_str(&subtasks);
    }

    // Token-budget truncation: cap pack size so it always fits an LLM context window.
    // v0.10.3: full bumped 10K → 24K → 32K. Real tasks accumulate 50-100 events
    // and the prior cap clipped final-summary decisions even after the
    // ORDER BY DESC reshuffle. 32K still fits comfortably inside any
    // modern LLM context budget.
    const FULL_BUDGET: usize = 32 * 1024;
    const COMPACT_BUDGET: usize = 2 * 1024;
    const TRUNC_MARKER: &str = "\n\n_(truncated to fit pack budget)_\n";
    let budget = match mode {
        PackMode::Full => FULL_BUDGET,
        PackMode::Compact => COMPACT_BUDGET,
    };
    let truncated = text.len() > budget;
    if truncated {
        truncate_to_budget(&mut text, budget, TRUNC_MARKER);
    }

    let generated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    // Write-through cache.
    conn.execute(
        "INSERT OR REPLACE INTO task_pack_cache(task_id, mode, text, generated_at, source_event_count)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![task_id, mode_str, text, generated_at, event_count as i64],
    )?;

    Ok(TaskPack {
        task_id: task_id.to_string(),
        mode,
        schema_version: crate::SCHEMA_VERSION.into(),
        text,
        metadata: PackMetadata {
            generated_at,
            source_event_count: event_count,
            cache_hit: false,
            truncated,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_mode_round_trips_via_serde() {
        let s = serde_json::to_string(&PackMode::Compact).unwrap();
        assert_eq!(s, "\"Compact\"");
    }

    #[test]
    fn parent_pack_contains_subtasks_section() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();

        // Parent + one child, each with an open event.
        let mut p = crate::event::Event::new(
            "p",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "Parent".into(),
        );
        p.meta = serde_json::json!({"title": "Parent"});
        crate::db::upsert_task_from_event(&conn, &p, "ph").unwrap();
        crate::db::index_event(&conn, &p).unwrap();

        let mut c = crate::event::Event::new(
            "c",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "Child".into(),
        );
        c.meta = serde_json::json!({"title": "Child", "parent_id": "p"});
        crate::db::upsert_task_from_event(&conn, &c, "ph").unwrap();
        crate::db::index_event(&conn, &c).unwrap();

        let parent_pack = assemble(&conn, "p", PackMode::Compact).unwrap();
        assert!(parent_pack.text.contains("Subtasks"));
        assert!(parent_pack.text.contains("Child"));
        assert!(parent_pack.text.contains("c")); // child id

        let child_pack = assemble(&conn, "c", PackMode::Compact).unwrap();
        assert!(!child_pack.text.contains("Subtasks"));
    }

    #[test]
    fn pack_shows_completeness_section_when_gaps() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();
        // Task with no goal → NoGoal gap.
        let e = crate::event::Event::new(
            "g1",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "T".into(),
        );
        crate::db::upsert_task_from_event(&conn, &e, "ph").unwrap();
        crate::db::index_event(&conn, &e).unwrap();

        let pack = assemble(&conn, "g1", PackMode::Compact).unwrap();
        assert!(pack.text.contains("Completeness"));
        assert!(pack.text.contains("no goal recorded"));
    }

    #[test]
    fn pack_no_completeness_section_when_complete() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();
        let mut e = crate::event::Event::new(
            "g2",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "T".into(),
        );
        e.meta = serde_json::json!({"title": "T"});
        crate::db::upsert_task_from_event(&conn, &e, "ph").unwrap();
        crate::db::index_event(&conn, &e).unwrap();
        // Give it a goal so NoGoal doesn't fire; open + no decisions → complete.
        conn.execute("UPDATE tasks SET goal='g' WHERE task_id='g2'", [])
            .unwrap();
        // pending_count() resolves `<data_dir>/pending`. Point the data dir at
        // the isolated tempdir (no pending/ child) so the PendingLeak rule
        // stays silent regardless of the real dev environment.
        std::env::set_var("TASK_JOURNAL_DATA_DIR", d.path());

        let pack = assemble(&conn, "g2", PackMode::Compact).unwrap();
        std::env::remove_var("TASK_JOURNAL_DATA_DIR");
        assert!(!pack.text.contains("## Completeness"));
    }

    #[test]
    fn cache_is_invalidated_on_new_event() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-inv",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Inv"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let _ = assemble(&conn, "tj-inv", PackMode::Compact).unwrap();
        let p2 = assemble(&conn, "tj-inv", PackMode::Compact).unwrap();
        assert!(p2.metadata.cache_hit);

        let dec = Event::new(
            "tj-inv",
            EventType::Decision,
            Author::Agent,
            Source::Chat,
            "D".into(),
        );
        db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        db::index_event(&conn, &dec).unwrap();

        let p3 = assemble(&conn, "tj-inv", PackMode::Compact).unwrap();
        assert!(
            !p3.metadata.cache_hit,
            "new event must invalidate the cache"
        );
    }

    #[test]
    fn pack_cache_hits_after_incremental_ingest_with_no_new_events() {
        // Reproduces the MCP hot loop: client calls task_pack(X), the server
        // runs ingest_new_events (which now reads only the JSONL tail), then
        // calls assemble(X). After B2 the second call must hit the cache —
        // before B2, full rebuild_state replayed every event through index_
        // event() which DELETEd the cache row, so we always missed.
        use crate::db;
        use crate::event::*;
        use std::io::Write;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let jsonl = d.path().join("events.jsonl");
        let project = "cafef00dcafef00d";

        let mut open_e = Event::new(
            "tj-cmcp",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Cached"});
        let dec = Event::new(
            "tj-cmcp",
            EventType::Decision,
            Author::Agent,
            Source::Chat,
            "Adopt Rust".into(),
        );

        let mut f = std::fs::File::create(&jsonl).unwrap();
        writeln!(f, "{}", serde_json::to_string(&open_e).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&dec).unwrap()).unwrap();
        drop(f);

        let conn = db::open(d.path().join("s.sqlite")).unwrap();

        // First MCP call: ingest, then pack.
        db::ingest_new_events(&conn, &jsonl, project).unwrap();
        let first = assemble(&conn, "tj-cmcp", PackMode::Compact).unwrap();
        assert!(
            !first.metadata.cache_hit,
            "first assemble must populate cache"
        );

        // Second MCP call: ingest again (zero new events in JSONL), then pack.
        let n_new = db::ingest_new_events(&conn, &jsonl, project).unwrap();
        assert_eq!(n_new, 0, "no new events should be ingested");
        let second = assemble(&conn, "tj-cmcp", PackMode::Compact).unwrap();
        assert!(
            second.metadata.cache_hit,
            "repeat assemble after a no-op ingest must hit the cache"
        );
        assert_eq!(first.text, second.text);
    }

    #[test]
    fn pack_cache_returns_cached_text_on_second_call() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-c",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Cache"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let p1 = assemble(&conn, "tj-c", PackMode::Compact).unwrap();
        assert!(!p1.metadata.cache_hit);
        let p2 = assemble(&conn, "tj-c", PackMode::Compact).unwrap();
        assert!(p2.metadata.cache_hit, "second call should hit cache");
        assert_eq!(p1.text, p2.text);
    }

    #[test]
    fn compact_mode_omits_optional_sections() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-cm",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Compact"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();
        let dec = Event::new(
            "tj-cm",
            EventType::Decision,
            Author::Agent,
            Source::Chat,
            "D1".into(),
        );
        db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        db::index_event(&conn, &dec).unwrap();

        let pack = assemble(&conn, "tj-cm", PackMode::Compact).unwrap();
        assert!(pack.text.contains("# Compact"));
        assert!(pack.text.contains("Active decisions"));
        assert!(pack.text.contains("Recent events"));
        assert!(
            !pack.text.contains("Lifecycle"),
            "compact should omit Lifecycle: {}",
            pack.text
        );
        assert!(
            !pack.text.contains("Rejected"),
            "compact should omit Rejected: {}",
            pack.text
        );
        assert!(
            !pack.text.contains("Evidence"),
            "compact should omit Evidence: {}",
            pack.text
        );
    }

    #[test]
    fn full_mode_truncates_when_exceeding_budget() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-big",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Big"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();
        for i in 0..100 {
            let ev = Event::new(
                "tj-big",
                EventType::Evidence,
                Author::Agent,
                Source::Chat,
                format!("Evidence #{i}: {}", "lorem ipsum ".repeat(50)),
            );
            db::upsert_task_from_event(&conn, &ev, "feedface").unwrap();
            db::index_event(&conn, &ev).unwrap();
        }
        let pack = assemble(&conn, "tj-big", PackMode::Full).unwrap();
        // v0.10.3: FULL_BUDGET bumped 10K → 24K → 32K + truncation slack.
        assert!(
            pack.text.len() <= 34 * 1024,
            "pack must stay under ~34KB; got {} bytes",
            pack.text.len()
        );
        assert!(pack.metadata.truncated, "metadata.truncated must be true");
        assert!(pack.text.contains("truncated to fit pack budget"));
    }

    #[test]
    fn truncate_to_budget_handles_multibyte_boundary() {
        // 1 ASCII byte shifts every 'я' (2 bytes) start to an ODD offset, so an
        // EVEN budget lands INSIDE a char — a raw text[..budget] slice would panic.
        let marker = "\n[cut]";
        let mut s = String::from("x");
        s.push_str(&"я".repeat(2000)); // total = 1 + 4000 = 4001 bytes
        let budget = 100usize; // even → mid-char given the odd char starts
        assert!(
            !s.is_char_boundary(budget),
            "precondition: budget must be mid-char"
        );
        truncate_to_budget(&mut s, budget, marker); // must NOT panic
        assert!(s.ends_with(marker));
        assert!(s.len() <= budget + marker.len());
        assert!(
            std::str::from_utf8(s.as_bytes()).is_ok(),
            "result must be valid UTF-8"
        );
    }

    #[test]
    fn truncate_to_budget_noop_under_budget() {
        let mut s = String::from("маленький текст");
        let before = s.clone();
        truncate_to_budget(&mut s, 10_000, "\n[cut]");
        assert_eq!(s, before);
    }

    #[test]
    fn corrected_events_appear_with_correction_event_type() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-co",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Corr"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let bad = Event::new(
            "tj-co",
            EventType::Finding,
            Author::Classifier,
            Source::Hook,
            "Migration done (wrong)".into(),
        );
        db::upsert_task_from_event(&conn, &bad, "feedface").unwrap();
        db::index_event(&conn, &bad).unwrap();

        let mut corr = Event::new(
            "tj-co",
            EventType::Correction,
            Author::User,
            Source::Cli,
            "Migration NOT done; finding was wrong".into(),
        );
        corr.corrects = Some(bad.event_id.clone());
        db::upsert_task_from_event(&conn, &corr, "feedface").unwrap();
        db::index_event(&conn, &corr).unwrap();

        let pack = assemble(&conn, "tj-co", PackMode::Full).unwrap();
        assert!(pack.text.contains("[correction]"));
        assert!(pack.text.contains("Migration NOT done"));
    }

    #[test]
    fn suggested_events_get_question_mark_marker_in_pack() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-q",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Q"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let mut suggested = Event::new(
            "tj-q",
            EventType::Decision,
            Author::Classifier,
            Source::Hook,
            "Adopt Rust".into(),
        );
        suggested.status = EventStatus::Suggested;
        db::upsert_task_from_event(&conn, &suggested, "feedface").unwrap();
        db::index_event(&conn, &suggested).unwrap();

        let pack = assemble(&conn, "tj-q", PackMode::Full).unwrap();
        let recent_pos = pack.text.find("## Recent events").unwrap();
        let recent_section = &pack.text[recent_pos..];
        assert!(
            recent_section.contains("[?]"),
            "suggested event must show [?] marker in Recent events:\n{recent_section}"
        );
    }

    #[test]
    fn pack_renders_recent_events_full_mode() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-re",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Recent"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();
        for i in 0..6 {
            let e = Event::new(
                "tj-re",
                EventType::Hypothesis,
                Author::Agent,
                Source::Chat,
                format!("hypothesis {i}"),
            );
            db::upsert_task_from_event(&conn, &e, "feedface").unwrap();
            db::index_event(&conn, &e).unwrap();
        }

        let pack = assemble(&conn, "tj-re", PackMode::Full).unwrap();
        assert!(pack.text.contains("## Recent events"));
        let count = pack.text.matches("[hypothesis]").count();
        assert!(
            count >= 5,
            "expected >=5 hypotheses, got {count} in {}",
            pack.text
        );
    }

    #[test]
    fn pack_renders_evidence_section() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-ev",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Ev"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let mut ev = Event::new(
            "tj-ev",
            EventType::Evidence,
            Author::Agent,
            Source::Chat,
            "Hook startup at 12ms vs 380ms node".into(),
        );
        ev.evidence_strength = Some(EvidenceStrength::Strong);
        db::upsert_task_from_event(&conn, &ev, "feedface").unwrap();
        db::index_event(&conn, &ev).unwrap();

        let pack = assemble(&conn, "tj-ev", PackMode::Full).unwrap();
        assert!(pack.text.contains("## Evidence"));
        assert!(pack.text.contains("12ms"));
        assert!(pack.text.contains("(strong)"));
    }

    #[test]
    fn pack_renders_rejected_options() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-r",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Rej"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let rej = Event::new(
            "tj-r",
            EventType::Rejection,
            Author::Agent,
            Source::Chat,
            "TypeScript: loses single-binary distribution".into(),
        );
        db::upsert_task_from_event(&conn, &rej, "feedface").unwrap();
        db::index_event(&conn, &rej).unwrap();

        let pack = assemble(&conn, "tj-r", PackMode::Full).unwrap();
        assert!(pack.text.contains("## Rejected"));
        assert!(pack.text.contains("TypeScript"));
    }

    #[test]
    fn pack_renders_active_decisions() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-ad",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Decisions test"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let dec = Event::new(
            "tj-ad",
            EventType::Decision,
            Author::Agent,
            Source::Chat,
            "Adopt Rust".into(),
        );
        db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        db::index_event(&conn, &dec).unwrap();

        let pack = assemble(&conn, "tj-ad", PackMode::Full).unwrap();
        assert!(
            pack.text.contains("## Active decisions"),
            "missing section: {}",
            pack.text
        );
        assert!(
            pack.text.contains("Adopt Rust"),
            "decision text missing: {}",
            pack.text
        );
    }

    #[test]
    fn pack_renders_decision_alternatives() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        let mut open_e = Event::new(
            "tj-alt",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Alt test"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let mut dec = Event::new(
            "tj-alt",
            EventType::Decision,
            Author::Agent,
            Source::Chat,
            "Use SQLite for storage".into(),
        );
        dec.meta = serde_json::json!({
            "alternatives": [
                {"option": "SQLite", "chosen": true, "rationale": "embedded, zero-ops"},
                {"option": "Postgres", "chosen": false, "rationale": "too heavy for a local tool"}
            ]
        });
        db::upsert_task_from_event(&conn, &dec, "feedface").unwrap();
        db::index_event(&conn, &dec).unwrap();

        let pack = assemble(&conn, "tj-alt", PackMode::Full).unwrap();
        // The decision itself still renders.
        assert!(
            pack.text.contains("Use SQLite for storage"),
            "decision text missing: {}",
            pack.text
        );
        // Considered alternatives surface, both chosen and rejected, with rationale.
        assert!(
            pack.text.contains("considered"),
            "alternatives header missing: {}",
            pack.text
        );
        assert!(
            pack.text.contains("SQLite") && pack.text.contains("Postgres"),
            "both options missing: {}",
            pack.text
        );
        assert!(
            pack.text.contains("too heavy for a local tool"),
            "rejected rationale missing: {}",
            pack.text
        );
    }

    #[test]
    fn assemble_includes_lifecycle_history() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();

        let mut open_e = Event::new(
            "tj-l",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Lifecycle"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let close_e = Event::new(
            "tj-l",
            EventType::Close,
            Author::User,
            Source::Cli,
            "done".into(),
        );
        db::upsert_task_from_event(&conn, &close_e, "feedface").unwrap();
        db::index_event(&conn, &close_e).unwrap();

        let pack = assemble(&conn, "tj-l", PackMode::Full).unwrap();
        assert!(pack.text.contains("## Lifecycle"));
        assert!(pack.text.contains("opened"));
        assert!(pack.text.contains("closed"));
    }

    #[test]
    fn assemble_header_only_compact() {
        use crate::db;
        use crate::event::*;
        use tempfile::TempDir;

        let d = TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();

        let mut open_e = Event::new(
            "tj-h",
            EventType::Open,
            Author::User,
            Source::Cli,
            "x".into(),
        );
        open_e.meta = serde_json::json!({"title": "Header test"});
        db::upsert_task_from_event(&conn, &open_e, "feedface").unwrap();
        db::index_event(&conn, &open_e).unwrap();

        let pack = assemble(&conn, "tj-h", PackMode::Compact).unwrap();
        assert!(
            pack.text.contains("# Header test"),
            "header missing: {}",
            pack.text
        );
        assert!(
            pack.text.contains("status: open"),
            "status missing: {}",
            pack.text
        );
        assert_eq!(pack.metadata.source_event_count, 1);
        assert!(!pack.metadata.cache_hit);
    }
}
