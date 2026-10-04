//! Doc-consistency guards for the bundled `task-journal` skill.
//!
//! The skill is the agent-facing contract for the five MCP tools. These tests
//! keep it honest against the real tool signatures so it can never again drift
//! into documenting params that do not exist (the `evidence_strength` bug) or
//! drop the ones that do (`goal` / `alternatives` / `outcome_tag`).

use std::path::PathBuf;

fn skill_md() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin/skills/task-journal/SKILL.md");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("bundled skill missing at {}: {e}", path.display()));
    // Normalize line endings: on a Windows checkout git may convert LF -> CRLF
    // (autocrlf), which would break the `\n`-anchored frontmatter assertions.
    raw.replace("\r\n", "\n")
}

#[test]
fn skill_is_present_with_valid_frontmatter() {
    let s = skill_md();
    assert!(
        s.starts_with("---\n"),
        "skill must open with YAML frontmatter"
    );
    assert!(
        s.contains("\nname: task-journal\n"),
        "frontmatter must declare name: task-journal"
    );
    // Frontmatter must be closed.
    assert!(
        s[4..].contains("\n---\n"),
        "frontmatter block must be closed with a --- line"
    );
}

#[test]
fn skill_documents_real_event_add_params_not_phantom_ones() {
    let s = skill_md();
    // `event_add` has no `evidence_strength` param — that field lives only on
    // the internal classifier output, never on the MCP tool. The skill must not
    // tell agents to pass it.
    assert!(
        !s.contains("evidence_strength"),
        "skill must not reference evidence_strength as an event_add param"
    );
    // The params the tools actually expose must be present.
    for needle in [
        "goal",
        "alternatives",
        "outcome_tag",
        "task_close",
        "task_search",
    ] {
        assert!(s.contains(needle), "skill must document `{needle}`");
    }
}

#[test]
fn skill_frames_self_tagging_as_primary() {
    let s = skill_md();
    assert!(
        s.contains("self-tagging"),
        "skill must frame self-tagging as the primary capture path"
    );
}

#[test]
fn skill_teaches_the_chronicle() {
    let s = skill_md();
    for needle in [
        "## Chronicle",
        "module_list",
        "module_page",
        "module_save",
        "module_link",
        "module_backfill_candidates",
        "modules=",
        "module_notes",
        "📚 Chronicle",
        "confirm",
        "catch-all",
        "offset",
    ] {
        assert!(s.contains(needle), "skill must mention {needle}");
    }
}

#[test]
fn map_command_exists_and_asks_the_user_first() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin/commands/map.md");
    let s = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("map command missing at {}: {e}", path.display()))
        .replace("\r\n", "\n");

    assert!(
        s.starts_with("---\n") && s.contains("\ndescription:"),
        "frontmatter"
    );
    for needle in [
        "module_list",
        "module_save",
        "module_backfill_candidates",
        "module_link",
        "confirm",
        "catch-all",
    ] {
        assert!(s.contains(needle), "map command must mention {needle}");
    }
    // Saving comes only after the user's answer.
    assert!(s.find("confirm").unwrap() < s.find("module_save").unwrap());
}
