//! Classifier eval harness.
//!
//! Two execution modes:
//!
//! 1. **Default (CI-safe).** Loads the labeled fixture, exercises the
//!    prompt builder against every input, and asserts:
//!    - the fixture has at least 30 examples
//!    - every example has a recognised `expected` event_type
//!    - the prompt builder always emits the input text into the prompt
//!    No model API is called. Deterministic, hermetic.
//!
//! 2. **Opt-in real classifier (`TJ_CLASSIFIER_EVAL=on`).** Calls
//!    `ClaudeCliClassifier::default()` against every fixture row and
//!    computes accuracy. Asserts accuracy ≥ 0.7 (initial floor; will
//!    ratchet up as the dataset grows). Requires a working `claude`
//!    CLI on PATH (subscription mode). Skipped silently if the env
//!    var is not set so the default `cargo test` run is fast and free.

use serde::Deserialize;
use std::collections::HashSet;

use tj_core::classifier::{
    cli::ClaudeCliClassifier, prompt, Classifier, ClassifyInput, ClassifyOutput,
};
use tj_core::event::EventType;

const FIXTURE: &str = include_str!("fixtures/classifier_eval.jsonl");
const ACCURACY_FLOOR: f64 = 0.70;

#[derive(Deserialize)]
struct Example {
    text: String,
    expected: String,
}

fn load_examples() -> Vec<Example> {
    FIXTURE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad fixture line: {l} — {e}")))
        .collect()
}

fn known_event_types() -> HashSet<String> {
    EventType::ALL
        .iter()
        .map(|t| {
            serde_json::to_value(t)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn fixture_has_minimum_size_and_known_types() {
    let examples = load_examples();
    assert!(
        examples.len() >= 30,
        "fixture must have ≥ 30 labeled rows, got {}",
        examples.len()
    );
    let known = known_event_types();
    for ex in &examples {
        assert!(
            known.contains(&ex.expected),
            "unknown expected event type '{}'",
            ex.expected
        );
    }
}

#[test]
fn prompt_builder_includes_every_fixture_input() {
    let examples = load_examples();
    for ex in &examples {
        let input = ClassifyInput {
            text: ex.text.clone(),
            author_hint: "assistant".into(),
            recent_tasks: vec![],
        };
        let p = prompt::build(&input);
        assert!(
            p.contains(&ex.text),
            "prompt missing fixture text: {}",
            ex.text
        );
    }
}

/// Real-classifier accuracy run. Skipped unless `TJ_CLASSIFIER_EVAL=on`.
/// Wired through `ClaudeCliClassifier::default()` so it runs against the
/// user's `claude -p` subscription if available.
#[test]
fn classifier_meets_accuracy_floor_on_labeled_dataset() {
    if std::env::var("TJ_CLASSIFIER_EVAL").as_deref() != Ok("on") {
        eprintln!(
            "skipping: set TJ_CLASSIFIER_EVAL=on to run the real-classifier eval against {} fixtures",
            load_examples().len()
        );
        return;
    }

    let classifier = ClaudeCliClassifier::default();
    let examples = load_examples();
    let mut correct = 0usize;
    let mut total = 0usize;
    let mut misses: Vec<(String, String, String)> = Vec::new();
    for ex in &examples {
        total += 1;
        let input = ClassifyInput {
            text: ex.text.clone(),
            author_hint: "assistant".into(),
            recent_tasks: vec![],
        };
        let out: ClassifyOutput = match classifier.classify(&input) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("classifier error on '{}': {e}", ex.text);
                continue;
            }
        };
        let predicted = serde_json::to_value(out.event_type)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        if predicted == ex.expected {
            correct += 1;
        } else {
            misses.push((ex.text.clone(), ex.expected.clone(), predicted));
        }
    }

    let accuracy = if total == 0 {
        0.0
    } else {
        correct as f64 / total as f64
    };
    eprintln!(
        "classifier eval: {correct}/{total} correct ({:.1}%)",
        accuracy * 100.0
    );
    if !misses.is_empty() {
        eprintln!("misses:");
        for (text, expected, predicted) in &misses {
            eprintln!("  expected={expected} predicted={predicted}: {text}");
        }
    }
    assert!(
        accuracy >= ACCURACY_FLOOR,
        "classifier accuracy {:.2} below floor {:.2}",
        accuracy,
        ACCURACY_FLOOR
    );
}
