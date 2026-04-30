//! Mock classifier: returns a pre-set output regardless of input.

use super::*;

pub struct MockClassifier {
    pub canned: ClassifyOutput,
}

impl Classifier for MockClassifier {
    fn classify(&self, _input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
        Ok(self.canned.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    #[test]
    fn mock_returns_canned_output() {
        let m = MockClassifier {
            canned: ClassifyOutput {
                event_type: EventType::Decision,
                task_id_guess: Some("tj-x".into()),
                confidence: 0.95,
                evidence_strength: None,
                suggested_text: "...".into(),
            },
        };
        let out = m
            .classify(&ClassifyInput {
                text: "ignored".into(),
                author_hint: "user".into(),
                recent_tasks: vec![],
            })
            .unwrap();
        assert_eq!(out.event_type, EventType::Decision);
        assert_eq!(out.confidence, 0.95);
    }
}
