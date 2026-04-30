//! Prompt builder for the classifier.

use crate::classifier::ClassifyInput;

pub fn build(input: &ClassifyInput) -> String {
    let recent = if input.recent_tasks.is_empty() {
        "(no active tasks)".to_string()
    } else {
        input.recent_tasks.iter().take(10).map(|t| {
            let trimmed_events: Vec<String> = t.last_events.iter().take(3)
                .map(|s| s.chars().take(120).collect::<String>())
                .collect();
            format!("- {} \"{}\": {}",
                t.task_id, t.title,
                if trimmed_events.is_empty() { "(no events)".into() } else { trimmed_events.join("; ") }
            )
        }).collect::<Vec<_>>().join("\n")
    };

    format!(
        "You classify chat chunks for an AI-coding-agent task journal.\n\
         Active tasks (top candidates):\n{recent}\n\n\
         New {author} chunk:\n{text}\n\n\
         Decide:\n\
         1. Which existing task this belongs to (or null if unrelated)\n\
         2. Best event_type from: hypothesis, finding, evidence, decision, rejection, constraint, correction, reopen, supersede, close, redirect\n\
         3. Confidence 0.0-1.0\n\
         4. evidence_strength (weak|medium|strong) if event_type is evidence, else omit\n\
         5. A 1-2 sentence suggested_text that captures the essence\n\n\
         Respond ONLY with strict JSON matching this shape, no commentary:\n\
         {{\"event_type\":\"...\",\"task_id_guess\":\"...\"|null,\"confidence\":0.0,\"evidence_strength\":\"...\"|null,\"suggested_text\":\"...\"}}",
        author=input.author_hint, text=input.text
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::*;

    #[test]
    fn prompt_includes_text_and_recent_tasks() {
        let input = ClassifyInput {
            text: "We adopted PKCE.".into(),
            author_hint: "assistant".into(),
            recent_tasks: vec![TaskContext {
                task_id: "tj-7f3a".into(),
                title: "OAuth login".into(),
                last_events: vec!["[hypothesis] PKCE vs implicit".into()],
            }],
        };
        let p = build(&input);
        assert!(p.contains("We adopted PKCE."));
        assert!(p.contains("tj-7f3a"));
        assert!(p.contains("PKCE vs implicit"));
        assert!(p.contains("strict JSON"));
    }

    #[test]
    fn prompt_handles_empty_tasks() {
        let input = ClassifyInput {
            text: "Hello".into(),
            author_hint: "user".into(),
            recent_tasks: vec![],
        };
        let p = build(&input);
        assert!(p.contains("(no active tasks)"));
    }
}
