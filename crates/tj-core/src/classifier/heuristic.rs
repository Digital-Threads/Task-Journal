//! Heuristic classifier — pattern-matching on common phrasing cues.
//!
//! Zero network, zero cost. Catches the obvious cases (explicit decisions,
//! rejections, test results, file:line findings) so the hybrid classifier
//! can avoid an API roundtrip 60-80% of the time. Returns `None` when no
//! pattern hits, letting `HybridClassifier` escalate to the LLM backend.

use super::{ClassifyInput, ClassifyOutput};
use crate::event::EventType;

/// Minimum text length to consider. Shorter chunks rarely carry
/// reasoning signal — let the LLM fallback (or no-op) handle them.
const MIN_TEXT_LEN: usize = 30;

/// Maximum suggested-text length we forward. Matches what the LLM
/// backends do.
const SUGGESTED_TEXT_MAX: usize = 300;

/// Try to classify `input.text` by keyword pattern. Returns `None` if no
/// rule matched — the caller should fall back to the LLM backend.
///
/// Rules are ordered most-specific-first; the first hit wins. Confidence
/// values are conservative: a heuristic match at 0.85 is "I'm pretty sure
/// this is a decision" but still under the 0.95 we'd expect from haiku.
pub fn try_heuristic(input: &ClassifyInput) -> Option<ClassifyOutput> {
    let text = input.text.trim();
    if text.len() < MIN_TEXT_LEN {
        return None;
    }
    let lower = text.to_lowercase();
    let task_id_guess = input.recent_tasks.first().map(|t| t.task_id.clone());

    for (patterns, etype, confidence) in RULES {
        for p in *patterns {
            if lower.contains(p) {
                return Some(ClassifyOutput {
                    event_type: *etype,
                    task_id_guess,
                    confidence: *confidence,
                    evidence_strength: None,
                    suggested_text: truncate(text, SUGGESTED_TEXT_MAX),
                    artifacts: None,
                });
            }
        }
    }
    None
}

/// Pattern table. Order matters — rejection should test before decision
/// because "decided to abandon" should land as Rejection, not Decision.
const RULES: &[(&[&str], EventType, f64)] = &[
    // Rejection: explicit abandonment / "this doesn't work".
    (
        &[
            "won't work",
            "doesn't work",
            "didn't work",
            "rejected",
            "abandoned",
            "tried but failed",
            "tried that, didn't",
            "не работает",
            "не подходит",
            "отказались",
            "отказался",
        ],
        EventType::Rejection,
        0.85,
    ),
    // Evidence: test outcomes / experiment results.
    (
        &[
            "test passed",
            "tests pass",
            "test failed",
            "tests fail",
            "regression test added",
            "previously failing, now green",
            "now green",
            "ci is green",
            "ci passes",
            "ci failed",
            "тест прошёл",
            "тесты зелёные",
        ],
        EventType::Evidence,
        0.85,
    ),
    // Decision: explicit commitment to an approach.
    (
        &[
            "decided to",
            "we'll use",
            "we will use",
            "we'll go with",
            "going with",
            "chose to",
            "the approach is",
            "решил использовать",
            "решили использовать",
            "будем использовать",
            "выбрал",
            "идём с",
        ],
        EventType::Decision,
        0.8,
    ),
    // Finding: verified observation, file:line references, "confirmed".
    (
        &[
            "confirmed:",
            "confirmed that",
            "verified that",
            "the code shows",
            "found that",
            "uses < instead",
            "uses <= instead",
            "off-by-one",
            "race condition",
        ],
        EventType::Finding,
        0.75,
    ),
    // Constraint: external limits.
    (
        &[
            "rate limit",
            "rate-limit",
            "requires ",
            "must be ",
            "limitation:",
            "not supported",
            "ограничение",
            "не поддерживает",
        ],
        EventType::Constraint,
        0.7,
    ),
    // Hypothesis: explicit uncertainty markers.
    (
        &[
            "i think",
            "i suspect",
            "maybe ",
            "could be ",
            "hypothesis:",
            "wondering if",
            "не уверен",
            "возможно",
        ],
        EventType::Hypothesis,
        0.65,
    ),
    // Correction: explicit reversal of an earlier statement.
    (
        &[
            "correction:",
            "ignore previous",
            "actually, ",
            "wait, that's wrong",
            "scratch that",
        ],
        EventType::Correction,
        0.7,
    ),
];

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> ClassifyInput {
        ClassifyInput {
            text: text.into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![super::super::TaskContext {
                task_id: "tj-xyz".into(),
                title: "test".into(),
                last_events: vec![],
            }],
        }
    }

    #[test]
    fn matches_decision_keyword() {
        let out = try_heuristic(&input(
            "After comparing both, we'll use postgres for the journal store",
        ))
        .unwrap();
        assert_eq!(out.event_type, EventType::Decision);
        assert!(out.confidence >= 0.75);
        assert_eq!(out.task_id_guess.as_deref(), Some("tj-xyz"));
    }

    #[test]
    fn matches_rejection_before_decision_when_both_present() {
        // Rejection rule is first in table — "doesn't work" should win over
        // any incidental "we'll use" later in the same chunk.
        let out = try_heuristic(&input(
            "Tried the proxy approach but it doesn't work under load; we'll use direct connections instead",
        ))
        .unwrap();
        assert_eq!(out.event_type, EventType::Rejection);
    }

    #[test]
    fn matches_evidence_for_test_results() {
        let out = try_heuristic(&input(
            "Regression test added; previously failing, now green on CI",
        ))
        .unwrap();
        assert_eq!(out.event_type, EventType::Evidence);
    }

    #[test]
    fn matches_finding_with_file_reference() {
        let out = try_heuristic(&input(
            "Confirmed: src/auth/refresh.rs uses < instead of <=, off-by-one at expiry boundary",
        ))
        .unwrap();
        assert_eq!(out.event_type, EventType::Finding);
    }

    #[test]
    fn returns_none_for_neutral_chatter() {
        assert!(try_heuristic(&input(
            "Reading the surrounding code to understand the call site."
        ))
        .is_none());
    }

    #[test]
    fn returns_none_for_short_text() {
        assert!(try_heuristic(&input("ok")).is_none());
        assert!(try_heuristic(&input("decided to fix it")).is_none());
    }

    #[test]
    fn russian_decision_phrases_match() {
        let out = try_heuristic(&input(
            "После обсуждения решили использовать SQLite вместо Postgres — проще для embed",
        ))
        .unwrap();
        assert_eq!(out.event_type, EventType::Decision);
    }

    #[test]
    fn suggested_text_is_truncated() {
        let long = "we'll use ".to_string() + &"x".repeat(500);
        let out = try_heuristic(&input(&long)).unwrap();
        assert!(out.suggested_text.chars().count() <= 300);
    }
}
