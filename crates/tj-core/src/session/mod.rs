//! Claude Code session JSONL parser and backfill logic.
//!
//! Parses `~/.claude/projects/<encoded-path>/<uuid>.jsonl` files
//! and extracts task-journal events retroactively.

pub mod discovery;
pub mod extractor;
pub mod parser;
