//! Dream — offline memory passes over session transcripts.
//!
//! Pass A (backfill): re-read a session transcript and append the
//! significant typed events the realtime classifier missed. Additive —
//! the JSONL source of truth is never mutated.

pub mod backend;
pub mod http;
pub mod prompt;
pub mod state;
