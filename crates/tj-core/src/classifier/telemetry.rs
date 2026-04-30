//! Append-only classifier telemetry: one JSONL line per classification call.

use serde::{Deserialize, Serialize};
use anyhow::Context;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryRecord {
    pub timestamp: String,
    pub project_hash: String,
    pub task_id_guess: Option<String>,
    pub event_type: String,
    pub confidence: f64,
    pub status: String,
    pub error: Option<String>,
}

pub fn append(metrics_path: impl AsRef<Path>, record: &TelemetryRecord) -> anyhow::Result<()> {
    if let Some(parent) = metrics_path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record).context("serialize telemetry")?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true).append(true).open(&metrics_path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_and_read_back_roundtrip() {
        let d = TempDir::new().unwrap();
        let path = d.path().join("metrics.jsonl");

        let r1 = TelemetryRecord {
            timestamp: "2026-04-30T00:00:00Z".into(),
            project_hash: "feedface".into(),
            task_id_guess: Some("tj-x".into()),
            event_type: "decision".into(),
            confidence: 0.92,
            status: "confirmed".into(),
            error: None,
        };
        let r2 = TelemetryRecord { confidence: 0.4, status: "suggested".into(), ..r1.clone() };
        append(&path, &r1).unwrap();
        append(&path, &r2).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 2);
    }
}
