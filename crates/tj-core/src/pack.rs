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
    let mut stmt = conn
        .prepare("SELECT text, strength FROM evidence WHERE task_id=?1 ORDER BY evidence_id ASC")?;
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
    let mut id_stmt = conn.prepare(
        "SELECT event_id FROM events_index
         WHERE task_id=?1 AND type='rejection'
         ORDER BY timestamp ASC",
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
    let mut stmt = conn.prepare(
        "SELECT text FROM decisions WHERE task_id=?1 AND status='active' ORDER BY decision_id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| r.get::<_, String>(0))?;
    let mut count = 0;
    for row in rows {
        out.push_str(&format!("- {}\n", row?));
        count += 1;
    }
    if count == 0 {
        out.push_str("- (none)\n");
    }
    out.push('\n');
    Ok(out)
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
            schema_version: "1.0".into(),
            text: cached_text,
            metadata: PackMetadata {
                generated_at: cached_at,
                source_event_count: cached_count as usize,
                cache_hit: true,
                truncated: was_truncated,
            },
        });
    }

    let (title, status): (String, String) = conn
        .query_row(
            "SELECT title, status FROM tasks WHERE task_id=?1",
            rusqlite::params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .with_context(|| format!("task not found: {task_id}"))?;

    let event_count: usize = conn.query_row(
        "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
        rusqlite::params![task_id],
        |r| r.get::<_, i64>(0).map(|n| n as usize),
    )?;

    let mut text = format!("# {title}  [status: {status}]\n\n");
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

    // Token-budget truncation: cap pack size so it always fits an LLM context window.
    const FULL_BUDGET: usize = 10 * 1024;
    const COMPACT_BUDGET: usize = 2 * 1024;
    const TRUNC_MARKER: &str = "\n\n_(truncated to fit pack budget)_\n";
    let budget = match mode {
        PackMode::Full => FULL_BUDGET,
        PackMode::Compact => COMPACT_BUDGET,
    };
    let truncated = text.len() > budget;
    if truncated {
        let cutoff = text[..budget].rfind('\n').unwrap_or(budget);
        text.truncate(cutoff);
        text.push_str(TRUNC_MARKER);
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
        schema_version: "1.0".into(),
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
        assert!(
            pack.text.len() <= 12 * 1024,
            "pack must stay under ~12KB; got {} bytes",
            pack.text.len()
        );
        assert!(pack.metadata.truncated, "metadata.truncated must be true");
        assert!(pack.text.contains("truncated to fit pack budget"));
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
