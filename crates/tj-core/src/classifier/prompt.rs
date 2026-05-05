//! Prompt builder for the classifier.

use crate::classifier::ClassifyInput;

pub fn build(input: &ClassifyInput) -> String {
    let recent = if input.recent_tasks.is_empty() {
        "(no active tasks)".to_string()
    } else {
        input
            .recent_tasks
            .iter()
            .take(10)
            .map(|t| {
                let trimmed_events: Vec<String> = t
                    .last_events
                    .iter()
                    .take(3)
                    .map(|s| s.chars().take(120).collect::<String>())
                    .collect();
                format!(
                    "- {} \"{}\": {}",
                    t.task_id,
                    t.title,
                    if trimmed_events.is_empty() {
                        "(no events)".into()
                    } else {
                        trimmed_events.join("; ")
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "You classify chat chunks for an AI-coding-agent task journal.\n\n\
         EVENT TYPE DEFINITIONS (choose the most specific match):\n\
         - hypothesis: An UNVERIFIED theory or assumption (\"maybe the bug is in X\", \"I think we should try Y\"). NOT yet confirmed.\n\
         - finding: A VERIFIED discovery backed by code reading or logs (\"found that function X does Y at line Z\", \"the config sets X=Y\").\n\
         - evidence: Test results, benchmarks, QA outcomes, reproduction steps, logs proving something works/fails. Set evidence_strength: weak (anecdotal), medium (single test), strong (comprehensive/e2e).\n\
         - decision: A chosen approach or architecture choice (\"will use strategy X because Y\"). The team commits to this.\n\
         - rejection: An approach explicitly REJECTED (\"tried X but it won't work because Y\"). Important for avoiding repeated work.\n\
         - constraint: An external limitation discovered (\"API rate limit is 100/min\", \"must support PHP 7.4\").\n\
         - correction: The chunk CORRECTS a previous finding/hypothesis that turned out wrong. Use when the text says \"actually\", \"correction\", \"was wrong about\".\n\
         - close: Task is done — fix shipped, PR merged, verified. Use when text indicates completion.\n\
         - reopen: A previously closed task needs more work.\n\
         - supersede: This task replaces another task entirely.\n\
         - redirect: This chunk actually belongs to a different task than initially thought.\n\n\
         IMPORTANT DISTINCTIONS:\n\
         - hypothesis vs finding: hypothesis = \"I think\"/\"maybe\"/\"could be\"; finding = \"I see\"/\"the code shows\"/\"confirmed that\"\n\
         - finding vs evidence: finding = discovered a fact; evidence = ran a test/experiment that PROVES something\n\
         - decision vs hypothesis: decision = committed choice; hypothesis = exploring an option\n\n\
         Active tasks (top candidates):\n{recent}\n\n\
         New {author} chunk:\n{text}\n\n\
         Decide:\n\
         1. Which existing task this belongs to (or null if unrelated/small-talk)\n\
         2. Best event_type from the definitions above\n\
         3. Confidence 0.0-1.0 (0.9+ = very clear match, 0.7-0.9 = likely, <0.7 = uncertain)\n\
         4. evidence_strength (weak|medium|strong) — REQUIRED if event_type is evidence, null otherwise\n\
         5. A 1-2 sentence suggested_text capturing the essence. Be specific: include file names, function names, IDs when present.\n\n\
         Respond ONLY with strict JSON, no commentary:\n\
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
    fn prompt_truncates_event_lines_to_keep_size_bounded() {
        let input = ClassifyInput {
            text: "abc".into(),
            author_hint: "user".into(),
            recent_tasks: (0..20)
                .map(|i| TaskContext {
                    task_id: format!("tj-{i:03}"),
                    title: format!("Task {i}"),
                    last_events: (0..30)
                        .map(|j| format!("[finding] very long evidence text {i}/{j} ").repeat(20))
                        .collect(),
                })
                .collect(),
        };
        let p = build(&input);
        assert!(
            p.len() < 64 * 1024,
            "prompt must stay under 64KB; got {}",
            p.len()
        );
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
