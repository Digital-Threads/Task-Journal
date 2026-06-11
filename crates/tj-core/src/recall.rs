//! Read-only proactive-recall engine. Given the current tool context,
//! return the most relevant prior confirmed `rejection`/`decision` events
//! so the agent doesn't re-walk a ruled-out path. Shared by the PostToolUse
//! push path (claude-memory-60m) and the MCP-output push path (7km).

use crate::event::EventType;

/// One recalled high-signal event that matched the current context.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallHit {
    pub task_id: String,
    pub event_type: EventType, // Rejection | Decision
    pub text: String,
    pub score: f64,
}

/// Max hits surfaced per call. Autonomously-chosen default, flagged for review.
pub const DEFAULT_MAX_HITS: usize = 2;
/// Min blended score to surface. Autonomously-chosen default, flagged for review.
pub const RELEVANCE_THRESHOLD: f64 = 1.0;

/// Common, low-signal words dropped from the OR-token query. Without this,
/// a shared stopword like "the" between an unrelated tool call and a prior
/// rejection scores a spurious hit. Kept deliberately small — just the
/// high-frequency glue words plus the noise tokens that leak in from the
/// synthesized tool-call JSON (`Bash: {"command": …}`).
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "you", "are", "was", "but", "not", "this",
    "that", "from", "have", "has", "had", "will", "your", "our", "out", "let",
    "lets", "command", "output", "input", "tool", "bash", "name", "response",
];

/// Build an FTS5 OR-of-tokens query from a free-text context string. A raw
/// multi-word context like "let's switch to axum" parses as an implicit AND
/// under FTS5, so it would never match a short rejection that only shares one
/// token. We instead OR the individual word tokens (punctuation stripped,
/// short tokens and stopwords dropped) so any shared *meaningful* keyword
/// scores a hit — the same recall intent as `run_rejected`'s MATCH, widened
/// for prose input. Returns `None` when no usable token survives (caller
/// then falls back to a raw LIKE on the query).
fn fts_or_query(query_text: &str) -> Option<String> {
    let tokens: Vec<String> = query_text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    Some(tokens.join(" OR "))
}

/// Search confirmed `rejection`/`decision` events for ones relevant to the
/// current context, blending an FTS5/LIKE text signal with artifact overlap.
///
/// Read-only: never mutates the JSONL log or any derived table. Returns at
/// most `max_hits` hits scoring >= [`RELEVANCE_THRESHOLD`], sorted by score
/// descending with `Rejection` winning ties over `Decision`.
pub fn relevant_recall(
    conn: &rusqlite::Connection,
    query_text: &str,
    max_hits: usize,
) -> anyhow::Result<Vec<RecallHit>> {
    use std::collections::HashMap;
    if query_text.trim().is_empty() {
        return Ok(Vec::new());
    }

    // score keyed by event_id; carry (task_id, type, text) for output.
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut meta: HashMap<String, (String, EventType, String)> = HashMap::new();

    // 1) Text signal: FTS5 OR-of-tokens MATCH, restricted to confirmed
    //    rejection/decision (mirrors run_rejected's join). The tokenizer
    //    strips all punctuation, so the resulting `a OR b OR c` query is
    //    always FTS-safe even when the raw context is noisy tool-call JSON
    //    (`Bash: {"command":"…"}`) full of `:`/`"`/`{}` that would otherwise
    //    trip the FTS5 parser. Falls back to a raw LIKE substring only when
    //    no usable token survives — the same fallback shape run_search uses.
    let fts_or = fts_or_query(query_text);
    let use_fts = fts_or.is_some();
    let sql = if use_fts {
        "SELECT ei.event_id, ei.task_id, ei.type, sf.text
         FROM events_index ei
         JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.status = 'confirmed'
           AND ei.type IN ('rejection','decision')
           AND search_fts MATCH ?1"
    } else {
        "SELECT ei.event_id, ei.task_id, ei.type, sf.text
         FROM events_index ei
         JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.status = 'confirmed'
           AND ei.type IN ('rejection','decision')
           AND sf.text LIKE ?1"
    };
    let bind = if let Some(or_query) = fts_or {
        or_query
    } else {
        crate::fts::like_pattern(query_text)
    };
    if let Ok(mut stmt) = conn.prepare(sql) {
        let rows = stmt.query_map(rusqlite::params![bind], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (eid, tid, ty, text) = row;
                let et = parse_type(&ty);
                *scores.entry(eid.clone()).or_insert(0.0) += 1.0; // text-match weight
                meta.entry(eid).or_insert((tid, et, text));
            }
        }
    }

    // 2) Artifact signal: overlap of artifacts::extract(query_text) against
    //    events_index.artifacts (mirrors find_related_tasks LIKE scan), same
    //    confirmed rejection/decision restriction. +weight per shared artifact.
    let arts = crate::artifacts::extract(query_text);
    for needle in arts
        .linked_issues
        .iter()
        .chain(arts.commit_hashes.iter())
        .chain(arts.files.iter())
    {
        let pattern = format!("%\"{}\"%", needle.replace('%', "\\%"));
        if let Ok(mut stmt) = conn.prepare(
            "SELECT ei.event_id, ei.task_id, ei.type, sf.text
             FROM events_index ei
             JOIN search_fts sf ON sf.event_id = ei.event_id
             WHERE ei.status = 'confirmed'
               AND ei.type IN ('rejection','decision')
               AND ei.artifacts LIKE ?1",
        ) {
            let rows = stmt.query_map(rusqlite::params![pattern], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            });
            if let Ok(rows) = rows {
                for row in rows.flatten() {
                    let (eid, tid, ty, text) = row;
                    let et = parse_type(&ty);
                    *scores.entry(eid.clone()).or_insert(0.0) += 0.5; // artifact weight
                    meta.entry(eid).or_insert((tid, et, text));
                }
            }
        }
    }

    // 3) Threshold + rank. Sort by score desc; tie → Rejection before Decision.
    let mut hits: Vec<RecallHit> = scores
        .into_iter()
        .filter(|(_, s)| *s >= RELEVANCE_THRESHOLD)
        .filter_map(|(eid, score)| {
            meta.remove(&eid).map(|(task_id, event_type, text)| RecallHit {
                task_id,
                event_type,
                text,
                score,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| rank(a.event_type).cmp(&rank(b.event_type)))
    });
    hits.truncate(max_hits);
    Ok(hits)
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

    // Open a temp db and ingest a slice of events through the same
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

    #[test]
    fn returns_matching_confirmed_rejection() {
        let rej = ev(
            "tj-1",
            EventType::Rejection,
            "Tried switching the server to axum but it broke rmcp stdio.",
            EventStatus::Confirmed,
        );
        let (_d, conn) = seeded(&[rej]);

        let hits = relevant_recall(&conn, "let's switch to axum", DEFAULT_MAX_HITS).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].event_type, EventType::Rejection);
        assert!(hits[0].text.contains("axum"));
    }

    #[test]
    fn ignores_suggested_and_wrong_type() {
        let suggested = ev(
            "tj-1",
            EventType::Rejection,
            "Rejected the axum migration tentatively.",
            EventStatus::Suggested,
        );
        let finding = ev(
            "tj-1",
            EventType::Finding,
            "The axum server starts fine in isolation.",
            EventStatus::Confirmed,
        );
        let (_d, conn) = seeded(&[suggested, finding]);

        let hits = relevant_recall(&conn, "axum", DEFAULT_MAX_HITS).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
    }

    #[test]
    fn caps_at_max_hits() {
        let events: Vec<Event> = (0..5)
            .map(|i| {
                ev(
                    "tj-1",
                    EventType::Rejection,
                    &format!("Rejected widget approach number {i} for the dashboard"),
                    EventStatus::Confirmed,
                )
            })
            .collect();
        let (_d, conn) = seeded(&events);

        let hits = relevant_recall(&conn, "dashboard widget", 2).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn rejection_wins_tie_over_decision() {
        let decision = ev(
            "tj-1",
            EventType::Decision,
            "Decided to use the postgres connector.",
            EventStatus::Confirmed,
        );
        let rejection = ev(
            "tj-2",
            EventType::Rejection,
            "Rejected the postgres connector for latency.",
            EventStatus::Confirmed,
        );
        let (_d, conn) = seeded(&[decision, rejection]);

        let hits = relevant_recall(&conn, "postgres connector", DEFAULT_MAX_HITS).unwrap();
        assert_eq!(hits.len(), 2);
        // Same text-match score (1.0 each) → rejection ranks first.
        assert_eq!(hits[0].event_type, EventType::Rejection);
        assert_eq!(hits[1].event_type, EventType::Decision);
    }

    #[test]
    fn below_threshold_returns_empty() {
        // No textual or artifact overlap → score stays 0 < threshold.
        let rej = ev(
            "tj-1",
            EventType::Rejection,
            "Rejected the kafka pipeline for cost reasons.",
            EventStatus::Confirmed,
        );
        let (_d, conn) = seeded(&[rej]);

        let hits = relevant_recall(&conn, "frontend styling refactor", DEFAULT_MAX_HITS).unwrap();
        assert!(hits.is_empty(), "got: {hits:?}");
    }

    #[test]
    fn empty_query_returns_empty() {
        let rej = ev(
            "tj-1",
            EventType::Rejection,
            "Rejected axum.",
            EventStatus::Confirmed,
        );
        let (_d, conn) = seeded(&[rej]);

        assert!(relevant_recall(&conn, "", DEFAULT_MAX_HITS).unwrap().is_empty());
        assert!(relevant_recall(&conn, "   ", DEFAULT_MAX_HITS)
            .unwrap()
            .is_empty());
    }
}
