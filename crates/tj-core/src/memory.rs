//! Global cross-project memory index (Pillar B).
//!
//! A single SQLite file (`data_dir/memory.sqlite`) mirrors the *high-signal*
//! events — decisions, rejections, constraints (and, later, consolidated
//! semantic/procedural/preference facts) — from every project, together with
//! their embeddings. This is what lets the agent recall relevant prior
//! reasoning across its whole history, not just the current repo — the thing
//! single-project memory tools can't do.
//!
//! The index is a denormalised cache: the per-project JSONL logs remain the
//! source of truth. It is rebuilt idempotently by [`sync_from_project`] and
//! queried by [`search`].

use rusqlite::Connection;

/// Event types worth surfacing proactively: a committed choice, a ruled-out
/// path, or an external limit. These are the reasoning the agent most wants
/// before repeating itself.
pub const HIGH_SIGNAL_TYPES: [&str; 3] = ["decision", "rejection", "constraint"];

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS global_memory (
  event_id     TEXT PRIMARY KEY,
  project_hash TEXT NOT NULL,
  task_id      TEXT NOT NULL,
  type         TEXT NOT NULL,
  tier         TEXT NOT NULL DEFAULT 'episodic',
  text         TEXT NOT NULL,
  model        TEXT NOT NULL,
  dim          INTEGER NOT NULL,
  vec          BLOB NOT NULL,
  created_at   TEXT NOT NULL,
  superseded   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_gm_type ON global_memory(type);
CREATE INDEX IF NOT EXISTS idx_gm_model ON global_memory(model);
CREATE VIRTUAL TABLE IF NOT EXISTS global_fts USING fts5(event_id UNINDEXED, text);
CREATE TABLE IF NOT EXISTS preferences (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  text       TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL
);
"#;

/// Open (creating + migrating) the global memory database at `path`.
pub fn open(path: impl AsRef<std::path::Path>) -> anyhow::Result<Connection> {
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    // busy_timeout FIRST: the one-time WAL conversion takes a brief exclusive lock
    // on the first open of a fresh/rollback-mode DB; with the timeout already in
    // effect, two processes first-opening at once wait instead of hitting
    // SQLITE_BUSY on the conversion itself.
    conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// A cross-project recall hit.
pub struct GlobalHit {
    pub event_id: String,
    pub project_hash: String,
    pub task_id: String,
    pub event_type: String,
    pub tier: String,
    pub text: String,
    pub score: f32,
}

/// Copy this project's high-signal embedded events into the global index.
/// Idempotent (`INSERT OR REPLACE` on `event_id`); call after embedding a
/// project. Returns how many rows were synced. `superseded` is flagged from the
/// `decisions.superseded_by` projection so contradicted decisions can be
/// down-ranked at query time.
pub fn sync_from_project(
    global: &Connection,
    project: &Connection,
    project_hash: &str,
) -> anyhow::Result<usize> {
    let placeholders = HIGH_SIGNAL_TYPES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT e.event_id, e.task_id, f.type, e.tier, f.text, e.model, e.dim, e.vec, e.created_at,
                CASE WHEN d.superseded_by IS NOT NULL THEN 1 ELSE 0 END
           FROM embeddings e
           JOIN search_fts f ON f.event_id = e.event_id
           LEFT JOIN decisions d ON d.decision_id = e.event_id
          WHERE f.type IN ({placeholders})"
    );
    let mut stmt = project.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(HIGH_SIGNAL_TYPES.iter()), |r| {
        Ok((
            r.get::<_, String>(0)?,  // event_id
            r.get::<_, String>(1)?,  // task_id
            r.get::<_, String>(2)?,  // type
            r.get::<_, String>(3)?,  // tier
            r.get::<_, String>(4)?,  // text
            r.get::<_, String>(5)?,  // model
            r.get::<_, i64>(6)?,     // dim
            r.get::<_, Vec<u8>>(7)?, // vec
            r.get::<_, String>(8)?,  // created_at
            r.get::<_, i64>(9)?,     // superseded
        ))
    })?;

    let mut n = 0usize;
    for row in rows {
        let (event_id, task_id, ty, tier, text, model, dim, vec, created_at, superseded) = row?;
        global.execute(
            "INSERT OR REPLACE INTO global_memory
               (event_id, project_hash, task_id, type, tier, text, model, dim, vec, created_at, superseded)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                event_id, project_hash, task_id, ty, tier, text, model, dim, vec, created_at, superseded
            ],
        )?;
        // Mirror into FTS5 for the fast keyword path (proactive hook).
        global.execute(
            "DELETE FROM global_fts WHERE event_id = ?1",
            rusqlite::params![event_id],
        )?;
        global.execute(
            "INSERT INTO global_fts(event_id, text) VALUES (?1, ?2)",
            rusqlite::params![event_id, text],
        )?;
        n += 1;
    }
    Ok(n)
}

/// Fast keyword (FTS5) search over the global index — no embedding, so it's
/// cheap enough to run on every prompt in the proactive hook (loading a model
/// per prompt would be too slow). Builds an OR query from the prompt's
/// alphanumeric tokens (≥4 chars) and ranks by BM25.
pub fn keyword_search(conn: &Connection, prompt: &str, k: usize) -> anyhow::Result<Vec<GlobalHit>> {
    let mut seen = std::collections::HashSet::new();
    let terms: Vec<String> = prompt
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 4)
        .map(|t| t.to_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let query = terms.join(" OR ");
    let mut stmt = conn.prepare(
        "SELECT g.event_id, g.project_hash, g.task_id, g.type, g.tier, g.text, g.superseded,
                bm25(global_fts)
           FROM global_fts
           JOIN global_memory g ON g.event_id = global_fts.event_id
          WHERE global_fts MATCH ?1
          ORDER BY bm25(global_fts)
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![query, k as i64], |r| {
        let bm: f64 = r.get(7)?;
        let superseded: i64 = r.get(6)?;
        // BM25 is lower-is-better; negate so higher == more relevant, then
        // nudge contradicted reasoning down.
        let score = (-bm) as f32 - if superseded != 0 { 0.5 } else { 0.0 };
        Ok(GlobalHit {
            event_id: r.get(0)?,
            project_hash: r.get(1)?,
            task_id: r.get(2)?,
            event_type: r.get(3)?,
            tier: r.get(4)?,
            text: r.get(5)?,
            score,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Semantic search across the whole global index for the embedder's `model`.
/// Returns the top `k` hits by cosine, with a small penalty applied to
/// superseded/contradicted entries so live reasoning ranks above stale.
pub fn search(
    conn: &Connection,
    query_vec: &[f32],
    model: &str,
    k: usize,
) -> anyhow::Result<Vec<GlobalHit>> {
    let mut stmt = conn.prepare(
        "SELECT event_id, project_hash, task_id, type, tier, text, vec, superseded
           FROM global_memory WHERE model = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![model], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Vec<u8>>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;

    let mut hits = Vec::new();
    for row in rows {
        let (event_id, project_hash, task_id, event_type, tier, text, blob, superseded) = row?;
        let mut score = crate::embed::cosine(query_vec, &crate::embed::from_blob(&blob));
        if superseded != 0 {
            score -= 0.1; // down-rank contradicted reasoning
        }
        hits.push(GlobalHit {
            event_id,
            project_hash,
            task_id,
            event_type,
            tier,
            text,
            score,
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(k);
    Ok(hits)
}

/// Count of indexed entries (test/stats helper).
pub fn count(conn: &Connection) -> anyhow::Result<usize> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM global_memory", [], |r| r.get(0))?;
    Ok(n as usize)
}

// ---------------------------------------------------------------------------
// Preference tier (Pillar C): user-level, cross-project memory injected every
// session — "I prefer terse output", "always use X here", etc.
// ---------------------------------------------------------------------------

/// Record a durable user preference. De-duplicated on text (a repeat is a
/// no-op). Returns whether a new preference was stored.
pub fn add_preference(conn: &Connection, text: &str, created_at: &str) -> anyhow::Result<bool> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        anyhow::bail!("preference text is empty");
    }
    let changed = conn.execute(
        "INSERT OR IGNORE INTO preferences(text, created_at) VALUES (?1, ?2)",
        rusqlite::params![trimmed, created_at],
    )?;
    Ok(changed > 0)
}

/// All stored preferences, oldest first.
pub fn list_preferences(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT text FROM preferences ORDER BY id")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::Embedder;

    fn finding(text: &str) -> crate::event::Event {
        // A decision event so it passes the HIGH_SIGNAL_TYPES filter.
        crate::event::Event::new(
            "tj-x",
            crate::event::EventType::Decision,
            crate::event::Author::User,
            crate::event::Source::Cli,
            text.into(),
        )
    }

    #[test]
    fn sync_then_search_finds_cross_project_decision() {
        let d = tempfile::TempDir::new().unwrap();
        let proj = crate::db::open(d.path().join("p.sqlite")).unwrap();
        let global = open(d.path().join("memory.sqlite")).unwrap();
        let emb = crate::embed::HashEmbedder::new(256);

        for text in [
            "chose to route refunds through the idempotent payment ledger",
            "use postgres advisory locks for the cron job leader election",
        ] {
            crate::db::index_event(&proj, &finding(text)).unwrap();
        }
        crate::db::embed_pending(&proj, "projhash", &emb, "t", 100).unwrap();

        let synced = sync_from_project(&global, &proj, "projhash").unwrap();
        assert_eq!(synced, 2);
        assert_eq!(count(&global).unwrap(), 2);

        let q = emb.embed_one("refund ledger idempotent").unwrap();
        let hits = search(&global, &q, emb.model_id(), 5).unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits[0].text.contains("refund"),
            "the refund decision must rank first across the global index, got: {}",
            hits[0].text
        );
        assert_eq!(hits[0].project_hash, "projhash");
    }

    #[test]
    fn keyword_search_matches_prompt_terms() {
        let d = tempfile::TempDir::new().unwrap();
        let proj = crate::db::open(d.path().join("p.sqlite")).unwrap();
        let global = open(d.path().join("memory.sqlite")).unwrap();
        let emb = crate::embed::HashEmbedder::new(64);
        crate::db::index_event(
            &proj,
            &finding("chose the idempotent payment ledger for refunds"),
        )
        .unwrap();
        crate::db::index_event(
            &proj,
            &finding("rejected kafka for the audit log; too heavy"),
        )
        .unwrap();
        crate::db::embed_pending(&proj, "ph", &emb, "t", 100).unwrap();
        sync_from_project(&global, &proj, "ph").unwrap();

        let hits = keyword_search(&global, "should we add a refund ledger here?", 5).unwrap();
        assert!(
            !hits.is_empty(),
            "prompt terms must match the ledger decision"
        );
        assert!(hits[0].text.contains("ledger"));

        // No overlapping ≥4-char term => no hit.
        assert!(keyword_search(&global, "tiny ui css fix", 5)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn preferences_store_dedup_and_list_in_order() {
        let d = tempfile::TempDir::new().unwrap();
        let g = open(d.path().join("memory.sqlite")).unwrap();
        assert!(add_preference(&g, "prefer terse output", "t1").unwrap());
        assert!(add_preference(&g, "respond in Russian", "t2").unwrap());
        // Duplicate is a no-op.
        assert!(!add_preference(&g, "prefer terse output", "t3").unwrap());
        assert_eq!(
            list_preferences(&g).unwrap(),
            vec![
                "prefer terse output".to_string(),
                "respond in Russian".to_string()
            ]
        );
    }

    #[test]
    fn search_filters_by_model() {
        let d = tempfile::TempDir::new().unwrap();
        let proj = crate::db::open(d.path().join("p.sqlite")).unwrap();
        let global = open(d.path().join("memory.sqlite")).unwrap();
        let emb = crate::embed::HashEmbedder::new(64);
        crate::db::index_event(&proj, &finding("decided to adopt the outbox pattern")).unwrap();
        crate::db::embed_pending(&proj, "ph", &emb, "t", 100).unwrap();
        sync_from_project(&global, &proj, "ph").unwrap();

        let q = emb.embed_one("outbox").unwrap();
        assert_eq!(search(&global, &q, "other-model", 5).unwrap().len(), 0);
        assert_eq!(search(&global, &q, emb.model_id(), 5).unwrap().len(), 1);
    }

    #[test]
    fn open_sets_wal_and_busy_timeout() {
        let d = tempfile::TempDir::new().unwrap();
        let conn = open(d.path().join("memory.sqlite")).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(timeout > 0, "busy_timeout must be > 0, got {timeout}");
    }
}
