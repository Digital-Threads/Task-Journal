//! tj-core: append-only event log + derived SQLite state for Task Journal.

#![deny(rust_2018_idioms)]

/// On-disk + on-wire schema version for events and packs. Bump when a
/// breaking change is made to the JSONL event shape or the pack JSON
/// envelope. Single source of truth across the workspace — never inline.
pub const SCHEMA_VERSION: &str = "1.0";

/// Build a fresh task identifier of the form `tj-<10 lowercase base32>`.
///
/// 50 bits of entropy from the ULID random suffix → birthday-collision
/// threshold ≈ 33 million tasks per project. The previous 6-char form
/// only gave ~4096; old IDs remain valid since storage keys are strings.
pub fn new_task_id() -> String {
    format!(
        "tj-{}",
        &ulid::Ulid::new().to_string()[10..20].to_lowercase()
    )
}

#[cfg(test)]
mod task_id_tests {
    use super::new_task_id;
    use std::collections::HashSet;

    #[test]
    fn new_task_id_has_expected_shape() {
        let id = new_task_id();
        assert!(id.starts_with("tj-"), "{id}");
        assert_eq!(id.len(), 13, "{id}");
        assert!(
            id[3..]
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{id}"
        );
    }

    #[test]
    fn new_task_id_unique_over_ten_thousand() {
        let mut seen = HashSet::with_capacity(10_000);
        for _ in 0..10_000 {
            let id = new_task_id();
            assert!(seen.insert(id.clone()), "collision: {id}");
        }
    }
}

pub mod artifacts;
pub mod classifier;
pub mod completeness;
pub mod db;
pub mod dream;
pub mod event;
pub mod frontmatter;
pub mod fts;
pub mod pack;
pub mod paths;
pub mod project_hash;
pub mod recall;
pub mod reminder;
pub mod session;
pub mod session_id;
pub mod storage;
pub mod title;

#[cfg(test)]
mod schema_version_tests {
    /// Source-level guard: production sites must reference `SCHEMA_VERSION`
    /// rather than inlining a literal. If you bump the version, do it in
    /// the const — never in a struct literal.
    #[test]
    fn pack_assembler_does_not_inline_schema_version_literal() {
        let pack_src = include_str!("pack.rs");
        assert!(
            !pack_src.contains("schema_version: \""),
            "pack.rs has an inline schema_version string literal — use crate::SCHEMA_VERSION"
        );
    }

    #[test]
    fn schema_version_matches_event_default() {
        let evt = crate::event::Event::new(
            "tj-x",
            crate::event::EventType::Open,
            crate::event::Author::User,
            crate::event::Source::Cli,
            "x".into(),
        );
        assert_eq!(evt.schema_version, super::SCHEMA_VERSION);
    }
}
