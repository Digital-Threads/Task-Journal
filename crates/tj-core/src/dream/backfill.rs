//! Pass A per-session backfill: dedup-guard + provenance stamping.

use crate::dream::backend::BackfillEvent;
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
}
