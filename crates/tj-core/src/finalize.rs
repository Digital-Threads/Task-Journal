//! Finalize — bring a legacy task to a finished shape.
//!
//! One LLM call reads a task's full event history and returns a judgment:
//! a human-readable title, a one-sentence outcome, and whether the events
//! clearly show the task was finished (so `complete` may close it). The
//! model decides — same word in different contexts misleads heuristics, and
//! a title like "пТак обясни…" is natural language yet a useless title, so
//! only a reader of the whole history can call it.

use crate::llm::LlmBackend;
use anyhow::Context;
use serde::Deserialize;

/// The model's verdict on a task, distilled from its events.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FinalizeJudgment {
    /// True when the current title is a poor description of the task and
    /// should be replaced by `title`. False echoes a good human title back.
    #[serde(default)]
    pub retitle: bool,
    /// A short human-readable title (≈5–10 words).
    #[serde(default)]
    pub title: String,
    /// True only when the events clearly show the task was finished. When
    /// unclear, the model leaves it false and `complete` keeps it open.
    #[serde(default)]
    pub done: bool,
    /// `done` | `abandoned` | `superseded` — only used when `done` is true.
    #[serde(default)]
    pub outcome_tag: String,
    /// One sentence: what actually happened / where the task ended.
    #[serde(default)]
    pub outcome: String,
    /// Short rationale for done / still-open — shown to the user.
    #[serde(default)]
    pub reason: String,
}

impl FinalizeJudgment {
    /// Apply the proposed title only when the model flagged the current one
    /// as poor AND offered a non-empty, genuinely different replacement.
    pub fn should_apply_title(&self, current_title: &str) -> bool {
        self.retitle && !self.title.trim().is_empty() && self.title.trim() != current_title.trim()
    }

    /// Map the model's tag to the validated close enum; falls back to `done`
    /// for an empty/unknown tag so the close path never rejects it.
    pub fn normalized_tag(&self) -> &str {
        match self.outcome_tag.trim() {
            "abandoned" => "abandoned",
            "superseded" => "superseded",
            _ => "done",
        }
    }
}

/// Build the judge prompt from a task's current title and its event lines
/// (each line pre-formatted as `[type] text` by the caller).
pub fn build_prompt(current_title: &str, event_lines: &[String]) -> String {
    let history = event_lines.join("\n");
    format!(
        "You are finalizing a software task's journal. Read its full history \
and reply with ONE JSON object, nothing else.\n\n\
Current title: {current_title}\n\n\
Event history (oldest first):\n{history}\n\n\
Return exactly this JSON shape:\n\
{{\n\
  \"retitle\": <true if the current title is a poor description of the task \
(a log line, a chat echo, a URL, a file path, a question fragment) and should \
be replaced; false if it already names the task well>,\n\
  \"title\": \"<a short, human-readable task title, 5-10 words, in the language \
of the history; echo the current title if retitle is false>\",\n\
  \"done\": <true ONLY if the events clearly show the task was finished \
(fix shipped, question answered, decision carried out); false if it is \
unclear or still in progress>,\n\
  \"outcome_tag\": \"<done | abandoned | superseded>\",\n\
  \"outcome\": \"<one sentence: what actually happened or where it ended>\",\n\
  \"reason\": \"<short: why you judged it done or still open>\"\n\
}}\n\
Be conservative about \"done\": if the history does not clearly show the task \
was completed, set done=false."
    )
}

/// Parse the model reply (a JSON object, possibly inside a ```json fence).
pub fn parse_judgment(text: &str) -> anyhow::Result<FinalizeJudgment> {
    let json_str = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    // Tolerate leading/trailing prose by slicing to the outermost braces.
    let slice = match (json_str.find('{'), json_str.rfind('}')) {
        (Some(a), Some(b)) if b > a => &json_str[a..=b],
        _ => json_str,
    };
    serde_json::from_str(slice)
        .with_context(|| format!("finalize JSON parse failed; got: {json_str}"))
}

/// One judge call: prompt → model → parsed judgment, with the token usage the
/// backend reported for the call.
pub fn judge(
    current_title: &str,
    event_lines: &[String],
    backend: &dyn LlmBackend,
) -> anyhow::Result<(FinalizeJudgment, crate::llm::LlmUsage)> {
    let prompt = build_prompt(current_title, event_lines);
    let (reply, usage) = backend.complete_usage(&prompt, 512)?;
    Ok((parse_judgment(&reply)?, usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBackend(String);
    impl LlmBackend for MockBackend {
        fn complete(&self, _prompt: &str, _max_tokens: u32) -> anyhow::Result<String> {
            Ok(self.0.clone())
        }
        fn name(&self) -> &'static str {
            "mock"
        }
    }

    #[test]
    fn parses_plain_json() {
        let j = parse_judgment(
            r#"{"retitle":true,"title":"Fix voucher refund","done":true,
                "outcome_tag":"done","outcome":"Refunded the missing 50%.","reason":"Fix shipped."}"#,
        )
        .unwrap();
        assert!(j.retitle);
        assert_eq!(j.title, "Fix voucher refund");
        assert!(j.done);
        assert_eq!(j.normalized_tag(), "done");
    }

    #[test]
    fn parses_fenced_json_with_prose() {
        let reply = "Here is the result:\n```json\n{\"retitle\":false,\"title\":\"Keep me\",\
\"done\":false,\"outcome_tag\":\"\",\"outcome\":\"\",\"reason\":\"still investigating\"}\n```\n";
        let j = parse_judgment(reply).unwrap();
        assert!(!j.retitle);
        assert!(!j.done);
        assert_eq!(j.reason, "still investigating");
    }

    #[test]
    fn unknown_tag_falls_back_to_done() {
        let j = FinalizeJudgment {
            retitle: false,
            title: String::new(),
            done: true,
            outcome_tag: "weird".into(),
            outcome: String::new(),
            reason: String::new(),
        };
        assert_eq!(j.normalized_tag(), "done");
    }

    #[test]
    fn should_apply_title_only_when_flagged_and_different() {
        let mut j = FinalizeJudgment {
            retitle: true,
            title: "Good title".into(),
            done: false,
            outcome_tag: String::new(),
            outcome: String::new(),
            reason: String::new(),
        };
        assert!(j.should_apply_title("#: 5"));
        // Same title → no churn.
        assert!(!j.should_apply_title("Good title"));
        // Model says keep → never replace, even if different.
        j.retitle = false;
        assert!(!j.should_apply_title("#: 5"));
        // Empty proposal → never replace.
        j.retitle = true;
        j.title = "   ".into();
        assert!(!j.should_apply_title("#: 5"));
    }

    #[test]
    fn prompt_includes_title_and_history() {
        let p = build_prompt(
            "#: 5",
            &["[open] #: 5".into(), "[decision] use SQL pack".into()],
        );
        assert!(p.contains("Current title: #: 5"));
        assert!(p.contains("[decision] use SQL pack"));
        assert!(p.contains("\"done\""));
    }

    #[test]
    fn judge_routes_through_backend() {
        let backend = MockBackend(
            r#"{"retitle":true,"title":"T","done":false,"outcome_tag":"","outcome":"","reason":"r"}"#
                .into(),
        );
        let (j, _usage) = judge("old", &["[open] old".into()], &backend).unwrap();
        assert_eq!(j.title, "T");
        assert!(!j.done);
    }
}
