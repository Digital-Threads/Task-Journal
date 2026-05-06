//! tj-core: append-only event log + derived SQLite state for Task Journal.

#![deny(rust_2018_idioms)]

/// On-disk + on-wire schema version for events and packs. Bump when a
/// breaking change is made to the JSONL event shape or the pack JSON
/// envelope. Single source of truth across the workspace — never inline.
pub const SCHEMA_VERSION: &str = "1.0";

pub mod classifier;
pub mod db;
pub mod event;
pub mod pack;
pub mod paths;
pub mod project_hash;
pub mod session;
pub mod storage;

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
