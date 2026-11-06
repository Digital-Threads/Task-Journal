//! Read-only proactive recall. After a tool call, find a prior confirmed
//! `rejection`/`decision` of the session's own task — or of a task in the
//! same module — that the call clearly touches, so the agent doesn't re-walk
//! a ruled-out path. Shared by the PostToolUse push path (claude-memory-60m)
//! and the MCP-output push path (7km).
//!
//! It stays quiet unless the match is strong: recall that fires on one
//! shared word, across unrelated tasks, or on machine text the classifier
//! once filed as reasoning, buried the agent in noise (dozens of hits per
//! session, almost none about the work at hand).

use crate::event::EventType;
use std::collections::HashSet;

/// One recalled high-signal event that matched the current context.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallHit {
    pub event_id: String,
    pub task_id: String,
    pub event_type: EventType, // Rejection | Decision
    pub text: String,
    pub score: f64,
}

/// Max hits surfaced per call.
pub const DEFAULT_MAX_HITS: usize = 1;
/// Shared meaningful words that make a match strong when no file, issue or
/// commit is shared.
pub const MIN_SHARED_WORDS: usize = 3;

/// Common, low-signal words dropped from the OR-token query. Without this,
/// a shared stopword like "the" between an unrelated tool call and a prior
/// rejection scores a spurious hit. Kept deliberately small — just the
/// high-frequency glue words plus the noise tokens that leak in from the
/// synthesized tool-call JSON (`Bash: {"command": …}`).
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "you", "are", "was", "but", "not", "this", "that", "from", "have",
    "has", "had", "will", "your", "our", "out", "let", "lets", "command", "output", "input",
    "tool", "bash", "name", "response",
];

/// The meaningful words of `text`: alphanumeric runs of 3+ chars, lowercased,
/// stopwords dropped. Shared by recall and module suggestions.
pub(crate) fn keywords(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// Prior confirmed rejections and decisions that `query_text` (what a tool
/// call did) clearly touches, for a session working on `task_id`.
///
/// Only what the agent or the user recorded — classifier output is not
/// trusted here — on `task_id` itself or on a task sharing a module with it,
/// never machine text, never a corrected or bookkeeping event. A match is
/// strong when the call and the event share a file, an issue or a commit, or
/// at least [`MIN_SHARED_WORDS`] meaningful words. Returns at most `max_hits`,
/// best first, a rejection ahead of a decision on a tie. Read-only.
pub fn recall_for_task(
    conn: &rusqlite::Connection,
    task_id: &str,
    query_text: &str,
    max_hits: usize,
) -> anyhow::Result<Vec<RecallHit>> {
    // A path is a file, not words: `/home/u/proj/crates/tj-core/src/db.rs`
    // alone would share "crates", "core" and "src" with any note on the crate.
    let (paths, prose): (Vec<&str>, Vec<&str>) = query_text
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| "\"'`,;:()[]{}<>|".contains(c)))
        .partition(|t| t.contains('/'));
    let words: HashSet<String> = keywords(&prose.join(" ")).into_iter().collect();
    let arts = crate::artifacts::extract(query_text);
    let needles: Vec<String> = arts
        .linked_issues
        .iter()
        .chain(arts.commit_hashes.iter())
        .map(|n| format!("\"{n}\""))
        .collect();
    if words.is_empty() && needles.is_empty() && paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT ei.event_id, ei.task_id, ei.type, sf.text, COALESCE(ei.artifacts, '')
         FROM events_index ei
         JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.status = 'confirmed'
           AND ei.type IN ('rejection','decision')
           AND ei.bookkeeping = 0
           AND ei.corrected_by IS NULL
           AND ei.author IN ('user','agent')
           AND (ei.task_id = ?1 OR ei.task_id IN (
                 SELECT other.task_id FROM task_modules mine
                 JOIN task_modules other ON other.module_id = mine.module_id
                 WHERE mine.task_id = ?1))",
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;

    let mut hits = Vec::new();
    for row in rows {
        let (event_id, task_id, ty, text, artifacts) = row?;
        if crate::noise::machine_text(&text).is_some() {
            continue;
        }

        let shared_arts = needles
            .iter()
            .filter(|n| artifacts.contains(n.as_str()))
            .count()
            + shared_files(&paths, &artifacts);
        let shared_words = keywords(&text)
            .into_iter()
            .collect::<HashSet<_>>()
            .intersection(&words)
            .count();
        if shared_arts == 0 && shared_words < MIN_SHARED_WORDS {
            continue;
        }

        hits.push(RecallHit {
            event_id,
            task_id,
            event_type: parse_type(&ty),
            text,
            score: (shared_arts * 2 + shared_words) as f64,
        });
    }

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| rank(a.event_type).cmp(&rank(b.event_type)))
    });
    hits.truncate(max_hits);

    Ok(hits)
}

/// Files of an event (its `artifacts` JSON) that one of `paths` names. An
/// event keeps `src/pack.rs` while a tool call names the absolute path, so a
/// match is the same path or one ending in the other at a `/`.
fn shared_files(paths: &[&str], artifacts: &str) -> usize {
    if paths.is_empty() || artifacts.is_empty() {
        return 0;
    }
    let Ok(arts) = serde_json::from_str::<crate::artifacts::Artifacts>(artifacts) else {
        return 0;
    };
    let same =
        |a: &str, b: &str| a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"));

    arts.files
        .iter()
        .filter(|f| paths.iter().any(|p| same(p, f)))
        .count()
}

fn parse_type(s: &str) -> EventType {
    match s {
        "rejection" => EventType::Rejection,
        _ => EventType::Decision,
    }
}

// Rejection ranks before Decision on a tie.
fn rank(t: EventType) -> u8 {
    match t {
        EventType::Rejection => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::event::{Author, Event, EventStatus, EventType, Source};

    const TASK: &str = "tj-1";
    const AXUM_CALL: &str = r#"{"command":"switch the server to axum"}"#;

    // Open a temp db and index a slice of events through the same
    // `index_event` path db.rs tests use (db::open + index_event).
    fn seeded(events: &[Event]) -> (tempfile::TempDir, rusqlite::Connection) {
        let d = tempfile::TempDir::new().unwrap();
        let conn = db::open(d.path().join("s.sqlite")).unwrap();
        for e in events {
            db::index_event(&conn, e).unwrap();
        }
        (d, conn)
    }

    fn ev(task: &str, ty: EventType, text: &str, status: EventStatus) -> Event {
        let mut e = Event::new(task, ty, Author::Agent, Source::Chat, text.into());
        e.status = status;
        e
    }

    fn rejection(task: &str, text: &str) -> Event {
        ev(task, EventType::Rejection, text, EventStatus::Confirmed)
    }

    #[test]
    fn returns_a_strong_match_on_the_task() {
        let (_d, conn) = seeded(&[rejection(
            TASK,
            "Tried to switch the server to axum but it broke rmcp stdio.",
        )]);

        let hits = recall_for_task(&conn, TASK, AXUM_CALL, DEFAULT_MAX_HITS).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].event_type, EventType::Rejection);
        assert!(hits[0].text.contains("axum"));
    }

    /// One shared word used to be enough: "server" or "test" matched
    /// rejections from any task in the project.
    #[test]
    fn one_or_two_shared_words_are_not_a_match() {
        let (_d, conn) = seeded(&[rejection(
            TASK,
            "Rejected the axum migration for the server: rmcp stdio broke.",
        )]);

        let hits = recall_for_task(&conn, TASK, r#"{"command":"restart the server"}"#, 2).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
        let hits = recall_for_task(&conn, TASK, r#"{"command":"axum server up"}"#, 2).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
    }

    #[test]
    fn a_shared_file_is_a_match() {
        let (_d, conn) = seeded(&[rejection(
            TASK,
            "Rejected caching in src/pack.rs: it was never reused.",
        )]);

        let hits = recall_for_task(&conn, TASK, "/home/u/proj/src/pack.rs x", 2).unwrap();
        assert_eq!(hits.len(), 1, "got: {hits:?}");
    }

    /// The words of a path are not words of the work: another file of the
    /// same crate shares "crates", "core" and "src", and that is no match.
    #[test]
    fn a_path_counts_as_a_file_not_as_words() {
        let (_d, conn) = seeded(&[rejection(
            TASK,
            "Rejected caching in crates/tj-core/src/pack.rs: it was never reused.",
        )]);

        let other =
            recall_for_task(&conn, TASK, "/home/u/proj/crates/tj-core/src/db.rs", 2).unwrap();
        assert!(other.is_empty(), "got: {other:?}");
        let same =
            recall_for_task(&conn, TASK, "/home/u/proj/crates/tj-core/src/pack.rs", 2).unwrap();
        assert_eq!(same.len(), 1, "got: {same:?}");
    }

    #[test]
    fn other_tasks_are_out_unless_they_share_a_module() {
        let text = "Tried to switch the server to axum but it broke rmcp stdio.";
        let (_d, conn) = seeded(&[rejection("tj-other", text)]);

        let hits = recall_for_task(&conn, TASK, AXUM_CALL, 2).unwrap();
        assert!(hits.is_empty(), "another task, no module: {hits:?}");

        for task in [TASK, "tj-other"] {
            conn.execute(
                "INSERT INTO task_modules(project_hash, task_id, module_id) VALUES ('p', ?1, 'mcp')",
                rusqlite::params![task],
            )
            .unwrap();
        }
        let hits = recall_for_task(&conn, TASK, AXUM_CALL, 2).unwrap();
        assert_eq!(hits.len(), 1, "same module: {hits:?}");
    }

    /// The classifier filed whole answers, tool calls and summaries as
    /// confirmed rejections; recall trusts only what a person or the agent
    /// recorded, and never machine text.
    #[test]
    fn classifier_output_and_machine_text_are_never_recalled() {
        let mut by_classifier =
            rejection(TASK, "Tried to switch the server to axum, it broke stdio.");
        by_classifier.author = Author::Classifier;
        let payload = rejection(TASK, r#"Bash: {"command":"switch the server to axum"}"#);
        let summary = rejection(
            TASK,
            "This session is being continued from a previous conversation: switch the server to axum.",
        );
        let (_d, conn) = seeded(&[by_classifier, payload, summary]);

        let hits = recall_for_task(&conn, TASK, AXUM_CALL, 3).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
    }

    #[test]
    fn ignores_suggested_and_wrong_type() {
        let suggested = ev(
            TASK,
            EventType::Rejection,
            "Rejected switching the server to axum tentatively.",
            EventStatus::Suggested,
        );
        let finding = ev(
            TASK,
            EventType::Finding,
            "The axum server starts fine in isolation after the switch.",
            EventStatus::Confirmed,
        );
        let (_d, conn) = seeded(&[suggested, finding]);

        let hits = recall_for_task(&conn, TASK, AXUM_CALL, 2).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
    }

    #[test]
    fn rejection_wins_a_tie_and_hits_are_capped() {
        let decision = ev(
            TASK,
            EventType::Decision,
            "Decided to switch the server to axum.",
            EventStatus::Confirmed,
        );
        let rej = rejection(TASK, "Rejected: switch the server to axum.");
        let (_d, conn) = seeded(&[decision, rej]);

        let hits = recall_for_task(&conn, TASK, AXUM_CALL, 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].event_type, EventType::Rejection);

        let hits = recall_for_task(&conn, TASK, AXUM_CALL, 1).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn empty_query_returns_empty() {
        let (_d, conn) = seeded(&[rejection(TASK, "Rejected axum.")]);

        assert!(recall_for_task(&conn, TASK, "", 2).unwrap().is_empty());
        assert!(recall_for_task(&conn, TASK, "   ", 2).unwrap().is_empty());
    }

    #[test]
    fn compaction_markers_are_never_recalled() {
        // An old marker (recognised by its text) and a new one (by meta.kind).
        let old = ev(
            TASK,
            EventType::Decision,
            "Conversation compacted at 2026-01-01T00:00:00Z; preceding events should be treated as a single reasoning unit.",
            EventStatus::Confirmed,
        );
        let mut new = ev(
            TASK,
            EventType::Decision,
            "Compaction boundary: preceding reasoning unit closed.",
            EventStatus::Confirmed,
        );
        new.meta = serde_json::json!({ "kind": "compaction_marker" });
        let (_d, conn) = seeded(&[old, new]);

        let hits = recall_for_task(&conn, TASK, "preceding reasoning unit", 2).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
    }

    #[test]
    fn corrected_events_are_never_recalled() {
        let rej = rejection(
            TASK,
            "Rejected switching the server to axum in src/server.rs.",
        );
        let mut corr = ev(
            TASK,
            EventType::Correction,
            "That was wrong: the stdio issue was unrelated.",
            EventStatus::Confirmed,
        );
        corr.corrects = Some(rej.event_id.clone());
        let (_d, conn) = seeded(&[rej, corr]);

        // Both the word and the file path must skip the corrected event.
        let hits = recall_for_task(&conn, TASK, "switch server axum src/server.rs", 2).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
    }
}
