//! Pack assembler: turns events + derived state into compact resume Markdown.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackMode { Compact, Full }

#[derive(Debug, Clone, Serialize)]
pub struct TaskPack {
    pub task_id: String,
    pub mode: PackMode,
    pub schema_version: String,
    pub text: String,
    pub metadata: PackMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackMetadata {
    pub generated_at: String,
    pub source_event_count: usize,
    pub cache_hit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_mode_round_trips_via_serde() {
        let s = serde_json::to_string(&PackMode::Compact).unwrap();
        assert_eq!(s, "\"Compact\"");
    }
}
