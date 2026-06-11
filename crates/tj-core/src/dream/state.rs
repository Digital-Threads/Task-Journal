//! Per-project dream watermark: the timestamp of the last successful
//! dream run. Sessions modified after this are in scope for the next run.

use rusqlite::Connection;

/// Read the last dream run timestamp (RFC3339), if any.
pub fn last_dream_at(conn: &Connection, project_hash: &str) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT last_dream_at FROM dream_state WHERE project_hash = ?1")?;
    let mut rows = stmt.query(rusqlite::params![project_hash])?;
    Ok(match rows.next()? {
        Some(r) => Some(r.get::<_, String>(0)?),
        None => None,
    })
}

/// Upsert the watermark to `at` (RFC3339).
pub fn set_last_dream_at(conn: &Connection, project_hash: &str, at: &str) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO dream_state(project_hash, last_dream_at, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(project_hash) DO UPDATE SET last_dream_at = ?2, updated_at = ?3",
        rusqlite::params![project_hash, at, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn watermark_round_trips_and_upserts() {
        let d = TempDir::new().unwrap();
        let conn = crate::db::open(d.path().join("s.sqlite")).unwrap();

        assert_eq!(last_dream_at(&conn, "ph").unwrap(), None);

        set_last_dream_at(&conn, "ph", "2026-06-08T10:00:00+00:00").unwrap();
        assert_eq!(
            last_dream_at(&conn, "ph").unwrap().as_deref(),
            Some("2026-06-08T10:00:00+00:00")
        );

        set_last_dream_at(&conn, "ph", "2026-06-09T10:00:00+00:00").unwrap();
        assert_eq!(
            last_dream_at(&conn, "ph").unwrap().as_deref(),
            Some("2026-06-09T10:00:00+00:00")
        );
    }
}
