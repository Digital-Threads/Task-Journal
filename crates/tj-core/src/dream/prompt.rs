//! Dream Pass A prompt: instruct the model to emit ONLY significant
//! reasoning not already represented in the task's existing events.

use crate::dream::backend::BackfillInput;

/// High-signal event types the backfill is allowed to emit. Chatter
/// (greetings, restating output) is explicitly excluded.
pub const ALLOWED_TYPES: &str = "decision, rejection, finding, constraint, hypothesis";

pub fn build_prompt(input: &BackfillInput) -> String {
    let mut tasks_block = String::new();
    for t in &input.tasks {
        tasks_block.push_str(&format!("## Task {} — {}\n", t.task_id, t.title));
        if t.existing_events.is_empty() {
            tasks_block.push_str("(no events captured yet)\n");
        } else {
            for e in &t.existing_events {
                tasks_block.push_str(&format!("- {e}\n"));
            }
        }
        tasks_block.push('\n');
    }

    format!(
        "You are a memory-backfill pass over a coding session transcript.\n\
         The realtime classifier already captured some events; your job is to \
         find SIGNIFICANT reasoning it MISSED.\n\n\
         Rules:\n\
         - Emit ONLY events whose substance is NOT already in the existing events below.\n\
         - Allowed event_type values: {types}.\n\
         - Skip chatter, restated tool output, and low-signal turns.\n\
         - Each event MUST set task_id to one of the candidate task ids.\n\
         - timestamp MUST be the RFC3339 timestamp of the transcript turn it came from.\n\
         - Respond with ONLY a JSON array of objects: \
         {{\"event_type\",\"task_id\",\"text\",\"timestamp\"}}. Empty array if nothing missed.\n\n\
         # Candidate tasks and their existing events\n{tasks}\n\
         # Transcript\n{transcript}\n\n\
         Remember: output ONLY the JSON array of missed events described above. \
         Do NOT reply to, summarise, or continue the transcript; if nothing was \
         missed, output [].\n",
        types = ALLOWED_TYPES,
        tasks = tasks_block,
        transcript = input.transcript,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dream::backend::{BackfillInput, BackfillTaskContext};

    #[test]
    fn prompt_includes_tasks_transcript_and_rules() {
        let input = BackfillInput {
            tasks: vec![BackfillTaskContext {
                task_id: "tj-7".into(),
                title: "Add dream".into(),
                existing_events: vec!["Decided to do two passes.".into()],
            }],
            transcript: "user: why two passes?\nassistant: because...".into(),
        };
        let p = build_prompt(&input);
        assert!(p.contains("tj-7"));
        assert!(p.contains("Add dream"));
        assert!(p.contains("Decided to do two passes."));
        assert!(p.contains("why two passes?"));
        assert!(p.contains("decision, rejection, finding, constraint, hypothesis"));
        assert!(p.contains("JSON array"));
    }

    #[test]
    fn prompt_marks_task_with_no_events() {
        let input = BackfillInput {
            tasks: vec![BackfillTaskContext {
                task_id: "tj-9".into(),
                title: "Fresh".into(),
                existing_events: vec![],
            }],
            transcript: "x".into(),
        };
        let p = build_prompt(&input);
        assert!(p.contains("no events captured yet"));
    }
}
