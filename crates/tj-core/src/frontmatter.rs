//! Pure renderer: a task's settled knowledge → a Claude-memory frontmatter file.
//! One-directional (Task-Journal → Claude memory). No fs, no DB, no JSONL.

/// A task's settled knowledge, pre-fetched by the caller (CLI).
pub struct MemoryInput<'a> {
    pub title: &'a str,
    pub meta: &'a crate::db::TaskMetadata,
    pub decisions: &'a [String],
    pub constraints: &'a [String],
}

/// Kebab-case slug: lowercase, non-alphanumeric runs → single `-`, trimmed.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true; // suppress leading dash
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("task");
    }
    out
}

/// One safe YAML double-quoted scalar: collapse whitespace/newlines to single
/// spaces, escape `\` and `"`.
fn yaml_quote(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let escaped = collapsed.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Render frontmatter + body. Empty sections are omitted.
pub fn render_memory(input: &MemoryInput<'_>) -> String {
    let title = input.title;
    let goal = input.meta.goal.as_deref().filter(|s| !s.is_empty());
    let description = goal.unwrap_or(title);

    let mut s = String::new();
    s.push_str("---\n");
    s.push_str(&format!("name: {}\n", slugify(title)));
    s.push_str(&format!("description: {}\n", yaml_quote(description)));
    s.push_str("metadata:\n  type: project\n");
    s.push_str("---\n");

    s.push_str(&format!("# {title}\n\n"));
    s.push_str(&format!("**Goal:** {}\n", goal.unwrap_or("(not set)")));
    if let Some(o) = input.meta.outcome.as_deref().filter(|s| !s.is_empty()) {
        s.push_str(&format!("**Outcome:** {o}\n"));
    }

    if !input.decisions.is_empty() {
        s.push_str("\n## Key decisions\n");
        for d in input.decisions {
            s.push_str(&format!("- {d}\n"));
        }
    }
    if !input.constraints.is_empty() {
        s.push_str("\n## Constraints\n");
        for c in input.constraints {
            s.push_str(&format!("- {c}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_kebabs_and_trims() {
        assert_eq!(slugify("Add Close Gate!"), "add-close-gate");
        assert_eq!(slugify("  Foo: Bar  "), "foo-bar");
    }

    #[test]
    fn render_has_frontmatter_block_with_type_project() {
        let meta = crate::db::TaskMetadata {
            goal: Some("Ship X".into()),
            ..Default::default()
        };
        let input = MemoryInput {
            title: "Ship X",
            meta: &meta,
            decisions: &[],
            constraints: &[],
        };
        let out = render_memory(&input);
        assert!(out.starts_with("---\n"));
        assert!(out.contains("name: ship-x"));
        assert!(out.contains("metadata:\n  type: project"));
        assert!(out.contains("\n---\n")); // closing fence
    }

    #[test]
    fn render_quotes_and_escapes_description() {
        let meta = crate::db::TaskMetadata {
            goal: Some("fix: a\nb \"q\"".into()),
            ..Default::default()
        };
        let input = MemoryInput {
            title: "T",
            meta: &meta,
            decisions: &[],
            constraints: &[],
        };
        let out = render_memory(&input);
        // description is one quoted scalar: newline collapsed, quotes escaped.
        assert!(out.contains(r#"description: "fix: a b \"q\"""#));
        // no raw newline inside the frontmatter description value
        let fm = out.split("\n---\n").next().unwrap();
        assert!(!fm.contains("fix: a\nb"));
    }

    #[test]
    fn render_omits_empty_sections_and_includes_filled_ones() {
        let meta = crate::db::TaskMetadata {
            goal: Some("G".into()),
            outcome: Some("O".into()),
            ..Default::default()
        };
        let input = MemoryInput {
            title: "T",
            meta: &meta,
            decisions: &["chose A".to_string()],
            constraints: &[],
        };
        let out = render_memory(&input);
        assert!(out.contains("## Key decisions"));
        assert!(out.contains("- chose A"));
        assert!(!out.contains("## Constraints"));
        assert!(out.contains("**Outcome:**"));
        assert!(out.contains("O"));
    }
}
