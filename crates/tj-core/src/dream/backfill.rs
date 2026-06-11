//! Pass A per-session backfill: dedup-guard + provenance stamping.

use crate::dream::backend::BackfillEvent;
use crate::event::{Author, Event, EventStatus, Source};
use std::collections::HashSet;

/// Similarity at or above which a proposed event is considered a
/// duplicate of an existing one. Tuned conservative (high) so the guard
/// only drops near-identical restatements, not genuinely new events.
pub const DUP_THRESHOLD: f64 = 0.8;

fn tokens(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Jaccard similarity of two strings' token sets (0.0..=1.0).
pub fn similarity(a: &str, b: &str) -> f64 {
    let (ta, tb) = (tokens(a), tokens(b));
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Drop proposed events that are near-duplicates of `existing` texts.
pub fn dedup_guard(proposed: Vec<BackfillEvent>, existing: &[String]) -> Vec<BackfillEvent> {
    proposed
        .into_iter()
        .filter(|p| {
            !existing
                .iter()
                .any(|e| similarity(&p.text, e) >= DUP_THRESHOLD)
        })
        .collect()
}

/// Build a journal Event from a proposed backfill event, stamping dream
/// provenance. `run_id` and `session_id` go into meta for traceability.
pub fn to_event(b: &BackfillEvent, run_id: &str, session_id: &str) -> Event {
    let mut e = Event::new(
        b.task_id.clone(),
        b.event_type,
        Author::Agent,
        Source::Dream,
        b.text.clone(),
    );
    e.status = EventStatus::Suggested;
    e.timestamp = b.timestamp.clone();
    e.meta = serde_json::json!({
        "dream_run_id": run_id,
        "session_id": session_id,
        "backfilled": true,
    });
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    fn ev(text: &str) -> BackfillEvent {
        BackfillEvent {
            event_type: EventType::Finding,
            task_id: "tj-1".into(),
            text: text.into(),
            timestamp: "2026-06-08T10:00:00Z".into(),
        }
    }

    #[test]
    fn drops_near_duplicate_keeps_novel() {
        let existing = vec!["We decided to use SQLite instead of Postgres".to_string()];
        let proposed = vec![
            ev("Decided to use SQLite instead of Postgres"), // near-dup
            ev("The cache layer needs a TTL of 60 seconds"), // novel
        ];
        let kept = dedup_guard(proposed, &existing);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].text.contains("TTL"));
    }

    #[test]
    fn identical_text_is_similarity_one() {
        assert!((similarity("hello world", "hello world") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn disjoint_text_is_similarity_zero() {
        assert_eq!(similarity("alpha beta", "gamma delta"), 0.0);
    }

    #[test]
    fn to_event_stamps_dream_provenance() {
        let b = ev("New constraint discovered");
        let e = to_event(&b, "run-1", "sess-9");
        assert_eq!(e.source, crate::event::Source::Dream);
        assert_eq!(e.author, crate::event::Author::Agent);
        assert_eq!(e.status, crate::event::EventStatus::Suggested);
        assert_eq!(e.timestamp, "2026-06-08T10:00:00Z");
        assert_eq!(e.meta["session_id"], serde_json::json!("sess-9"));
        assert_eq!(e.meta["dream_run_id"], serde_json::json!("run-1"));
        assert_eq!(e.meta["backfilled"], serde_json::json!(true));
        assert_eq!(e.task_id, "tj-1");
    }
}
