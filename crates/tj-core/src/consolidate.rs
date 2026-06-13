//! Memory consolidation (Pillar C): distil a project's recurring decisions and
//! constraints into a handful of durable semantic/procedural facts with a single
//! LLM call.
//!
//! The call goes through the pluggable [`crate::llm`] backend — default
//! `claude-p` on your subscription (no API key), configurable to the Anthropic
//! API, any OpenAI-compatible provider (OpenAI / Codex), or a **free** local
//! Ollama. When no backend is available the caller skips cleanly; we never fall
//! back to a heuristic, which would manufacture low-trust "facts".
//!
//! This is a MANUAL command: one call per run, only when the user asks, never on
//! a hook — so it never resembles the per-prompt classifier burn.

/// A distilled fact and which tier it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidatedFact {
    /// "semantic" (a durable truth about the system) or "procedural" (how the
    /// team works).
    pub tier: String,
    pub text: String,
}

/// Distil `events` into at most `max_facts` durable facts via the chosen
/// backend (`backend` overrides `TJ_BACKEND`; `None` uses the default chain).
/// Returns `(backend label, facts)`, or `None` when no backend is usable or
/// `TJ_CONSOLIDATE_BACKEND=none` forces a skip.
pub fn summarize(
    events: &[String],
    max_facts: usize,
    backend: Option<&str>,
) -> anyhow::Result<Option<(&'static str, Vec<ConsolidatedFact>)>> {
    if std::env::var("TJ_CONSOLIDATE_BACKEND").as_deref() == Ok("none") {
        return Ok(None);
    }
    let llm = match crate::llm::backend_from_env(backend)? {
        Some(b) => b,
        None => return Ok(None),
    };
    if events.is_empty() {
        return Ok(Some((llm.name(), Vec::new())));
    }
    let prompt = build_prompt(events, max_facts);
    let text = llm.complete(&prompt, 512)?;
    Ok(Some((llm.name(), parse_facts(&text))))
}

/// The summarisation prompt. Deliberately strict: durable-only, fixed line
/// format, "output nothing" escape hatch so the model doesn't pad.
pub fn build_prompt(events: &[String], max_facts: usize) -> String {
    let joined = events
        .iter()
        .map(|e| format!("- {}", e.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are given decisions and constraints recorded across ONE software \
project. Distil them into at most {max_facts} DURABLE facts — stable \
conventions or architectural truths that hold across the project, not one-off \
details.\n\n\
Rules:\n\
- One fact per line.\n\
- Each line MUST start with `[semantic]` (a durable truth about the system) or \
`[procedural]` (how the team works).\n\
- Keep each fact to one short sentence.\n\
- If nothing is durable enough, output nothing at all.\n\n\
Decisions and constraints:\n{joined}"
    )
}

/// Parse the model reply into facts. Accepts lines like `[semantic] ...` or
/// `- [procedural] ...`; ignores anything else.
pub fn parse_facts(text: &str) -> Vec<ConsolidatedFact> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', ' ']).trim();
        for tier in ["semantic", "procedural"] {
            let tag = format!("[{tier}]");
            if let Some(rest) = line.strip_prefix(&tag) {
                let fact = rest.trim();
                if fact.chars().count() >= 6 {
                    out.push(ConsolidatedFact {
                        tier: tier.to_string(),
                        text: fact.to_string(),
                    });
                }
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Promote conventions to always-on: a managed block in the project CLAUDE.md.
// ---------------------------------------------------------------------------

const CONV_START: &str = "<!-- task-journal:conventions:start -->";
const CONV_END: &str = "<!-- task-journal:conventions:end -->";

/// Render the consolidated facts as a managed CLAUDE.md block (delimited so it
/// can be regenerated without disturbing hand-written content).
pub fn render_conventions_block(facts: &[ConsolidatedFact]) -> String {
    let mut s = String::from(CONV_START);
    s.push_str(
        "\n## Project conventions (auto-derived by task-journal)\n\
_Regenerate with `task-journal consolidate --write-claude-md`. Lines between the \
markers are overwritten — edit elsewhere._\n\n",
    );
    for f in facts {
        s.push_str(&format!("- ({}) {}\n", f.tier, f.text));
    }
    s.push_str(CONV_END);
    s
}

/// Insert or replace the managed conventions block in `existing` CLAUDE.md text.
/// Replaces the block between the markers if present, else appends it. Never
/// touches anything outside the markers.
pub fn upsert_conventions_block(existing: &str, facts: &[ConsolidatedFact]) -> String {
    let block = render_conventions_block(facts);
    match (existing.find(CONV_START), existing.find(CONV_END)) {
        (Some(start), Some(end_idx)) if end_idx >= start => {
            let end = end_idx + CONV_END.len();
            format!("{}{}{}", &existing[..start], block, &existing[end..])
        }
        _ => {
            let mut out = existing.to_string();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(&block);
            out.push('\n');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(tier: &str, text: &str) -> ConsolidatedFact {
        ConsolidatedFact {
            tier: tier.into(),
            text: text.into(),
        }
    }

    #[test]
    fn conventions_block_appends_then_replaces_idempotently() {
        let facts = vec![fact("semantic", "always lock the DB for money")];
        // Append into existing hand-written content.
        let v1 = upsert_conventions_block("# My project\n\nHand rules.\n", &facts);
        assert!(v1.contains("# My project"));
        assert!(v1.contains("always lock the DB"));
        assert!(v1.contains(CONV_START) && v1.contains(CONV_END));

        // Re-run with new facts → replaces the block, keeps hand content, no dup.
        let facts2 = vec![fact("procedural", "PR into main, squash")];
        let v2 = upsert_conventions_block(&v1, &facts2);
        assert!(v2.contains("# My project"), "hand content preserved");
        assert!(v2.contains("PR into main, squash"));
        assert!(!v2.contains("always lock the DB"), "old facts replaced");
        assert_eq!(
            v2.matches(CONV_START).count(),
            1,
            "exactly one managed block"
        );
    }

    #[test]
    fn parse_facts_extracts_tagged_lines() {
        let reply = "[semantic] Refunds route through the idempotent ledger\n\
                     - [procedural] PR into main, squash-merge\n\
                     some preamble that should be ignored\n\
                     [bogus] not a real tier";
        let facts = parse_facts(reply);
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].tier, "semantic");
        assert!(facts[0].text.contains("idempotent ledger"));
        assert_eq!(facts[1].tier, "procedural");
        assert!(facts[1].text.contains("squash-merge"));
    }

    #[test]
    fn parse_facts_empty_on_no_tagged_lines() {
        assert!(parse_facts("nothing durable here").is_empty());
        assert!(parse_facts("").is_empty());
    }

    #[test]
    fn build_prompt_includes_events_and_cap() {
        let p = build_prompt(&["chose ledger".into(), "PR into main".into()], 5);
        assert!(p.contains("at most 5"));
        assert!(p.contains("- chose ledger"));
        assert!(p.contains("- PR into main"));
        assert!(p.contains("[semantic]") && p.contains("[procedural]"));
    }

    #[test]
    fn summarize_skips_when_backend_forced_none() {
        std::env::set_var("TJ_CONSOLIDATE_BACKEND", "none");
        let r = summarize(&["chose ledger".into()], 5, None).unwrap();
        std::env::remove_var("TJ_CONSOLIDATE_BACKEND");
        assert!(r.is_none());
    }
}
