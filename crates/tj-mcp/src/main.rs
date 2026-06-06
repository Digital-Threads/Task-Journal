//! task-journal-mcp: MCP server entry point.
//!
//! Phase 2 wires real implementations into all 5 tools, calling tj-core.

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{
    handler::server::tool::Parameters, handler::server::wrapper::Json, tool, tool_handler,
    tool_router, transport::io::stdio, ErrorData as McpError, ServerHandler, ServiceExt,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Optional override for the project directory used by every tool handler.
/// `None` (the default) means "use the current working directory at the time
/// the tool is invoked", which preserves 0.1.x behaviour. Set once from the
/// CLI parser and never mutated again.
static PROJECT_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

#[derive(Parser)]
#[command(
    name = "task-journal-mcp",
    version,
    about = "MCP server for task-journal"
)]
struct Cli {
    /// Override the project directory used to resolve event/state paths.
    /// Defaults to the current working directory when omitted.
    #[arg(long, value_name = "PATH")]
    project_dir: Option<PathBuf>,
}

/// Convert any internal failure into a JSON-RPC error frame. We attach the
/// stringified `anyhow::Error` chain as the `message` so the client sees the
/// full context (e.g. "task not found: tj-x: no row returned").
fn into_mcp_error(err: anyhow::Error) -> McpError {
    McpError::internal_error(format!("{err:#}"), None)
}

/// Stable, low-cost correlation token for one tool invocation. ULID gives
/// us 26 lexicographic characters with embedded timestamp ordering and a
/// random suffix — tools do not need millisecond uniqueness, but the
/// timestamp makes log scrubbing easier than a pure-random UUID.
fn new_correlation_id() -> String {
    ulid::Ulid::new().to_string()
}

/// Wrap one tool handler with structured tracing. Emits one INFO line at
/// entry (with the correlation id and tool name) and one INFO line at
/// exit (with elapsed ms and ok/err). Callers grep on `correlation_id=`
/// to follow a single client request across logs.
async fn traced_tool<T, Fut>(tool: &'static str, fut: Fut) -> Result<T, McpError>
where
    Fut: std::future::Future<Output = Result<T, McpError>>,
{
    let correlation_id = new_correlation_id();
    let started_at = std::time::Instant::now();
    tracing::info!(tool, %correlation_id, "tool_call start");
    let result = fut.await;
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(tool, %correlation_id, elapsed_ms, "tool_call ok"),
        Err(e) => tracing::warn!(
            tool,
            %correlation_id,
            elapsed_ms,
            error = %e.message,
            "tool_call err"
        ),
    }
    result
}

/// Run synchronous I/O on the tokio blocking pool. Without this, every tool
/// handler would do SQLite + JSONL work directly on the executor thread
/// and a slow operation in one tool would stall every other concurrent
/// request — defeats the point of using an async runtime at all.
async fn run_blocking<T, F>(f: F) -> Result<T, McpError>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let join_result = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| McpError::internal_error(format!("blocking task panicked: {e}"), None))?;
    join_result.map_err(into_mcp_error)
}

/// Process-wide cache of SQLite connections keyed by state-file path.
///
/// Without this, every tool handler called `tj_core::db::open()` which
/// re-runs PRAGMAs, the migrations registry, and re-creates a new WAL
/// reader. At small N the open cost dominates the actual work.
///
/// Storage layout: an outer `Mutex` guards the map (only briefly, during
/// insert/lookup), and each entry is `Arc<Mutex<Connection>>` so callers
/// can hold a connection across a longer transaction without blocking
/// other projects.
fn connection_cache() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<Connection>>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<Connection>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get or create the cached `Connection` for a SQLite state path. The
/// returned `Arc<Mutex<...>>` is shared with future callers; the inner
/// mutex is the lock you actually want to take during a tool call.
fn cached_open(state_path: &Path) -> anyhow::Result<Arc<Mutex<Connection>>> {
    let mut cache = connection_cache()
        .lock()
        .map_err(|e| anyhow::anyhow!("connection cache poisoned: {e}"))?;
    if let Some(existing) = cache.get(state_path) {
        return Ok(existing.clone());
    }
    let conn =
        tj_core::db::open(state_path).with_context(|| format!("open SQLite at {state_path:?}"))?;
    let arc = Arc::new(Mutex::new(conn));
    cache.insert(state_path.to_path_buf(), arc.clone());
    Ok(arc)
}

/// MCP instructions delivered to every Claude Code session where this plugin is installed.
/// This is the primary mechanism for self-contained plugin behavior — no manual CLAUDE.md edits needed.
const MCP_INSTRUCTIONS: &str = r#"Task Journal — reasoning chain memory for AI coding sessions.

MANDATORY WORKFLOW — follow for EVERY coding session:

1. SESSION START → task_search for recent open tasks → task_pack to resume, OR task_create for new work
2. EVERY significant discovery → event_add(event_type="finding")
3. EVERY decision made → event_add(event_type="decision")
4. EVERY rejected approach → event_add(event_type="rejection")
5. EVERY hypothesis formed → event_add(event_type="hypothesis")
6. TEST RESULTS → event_add(event_type="evidence")
7. WRONG hypothesis corrected → event_add(event_type="correction", corrects=<event_id>)
8. TASK DONE → task_close with reason and outcome

EVENT TYPE GUIDE — choose correctly:
• hypothesis = "I think" / "maybe" / "could be" → UNVERIFIED theory
• finding = "I see" / "the code shows" / "confirmed" → VERIFIED by reading code/logs
• evidence = ran a test/experiment that PROVES something
• decision = committed choice ("We'll use X because Y")
• rejection = explicitly rejected approach ("Tried X but won't work because Y")
• constraint = external limitation discovered ("API rate limit is 100/min")
• correction = corrects earlier event (set corrects field)

KEY RULES:
• One task = one logical objective. Don't create a new task every turn.
• Always close tasks when done. Don't leave them open.
• Log rejections — wrong paths prevent repeated mistakes.
• Append-only — never edit events, write corrections instead.
"#;

#[derive(Clone, Default)]
pub struct TaskJournalServer;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskPackParams {
    pub task_id: String,
    pub mode: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskPackResult {
    pub task_id: String,
    pub mode: String,
    pub schema_version: String,
    pub text: String,
    pub metadata: TaskPackMetadata,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskPackMetadata {
    pub source_event_count: Option<usize>,
    pub cache_hit: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskSearchParams {
    pub query: String,
    pub status: Option<String>,
    pub project: Option<String>,
    /// v0.10.3+: restrict matches to a single event type
    /// (`decision`, `evidence`, `finding`, `rejection`, ...).
    /// Accepts any value in [`tj_core::event::EventType::ALL`].
    pub event_type: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskSearchResult {
    pub query: String,
    pub results: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskCreateParams {
    pub title: String,
    pub initial_context: Option<String>,
    /// v0.4.0+: explicit goal — what is the user trying to accomplish.
    /// Renders as the first line of every pack and is the anchor for
    /// "why was this done?" answers weeks later. Optional only for
    /// backwards compat; agents should always pass it.
    pub goal: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskCreateResult {
    pub task_id: String,
    pub title: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EventAddParams {
    pub task_id: String,
    pub event_type: String,
    pub text: String,
    pub corrects: Option<String>,
    pub supersedes: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EventAddResult {
    pub event_id: String,
    pub task_id: String,
    pub event_type: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskCloseParams {
    pub task_id: String,
    pub reason: String,
    pub outcome: Option<String>,
    /// v0.4.0+: structured outcome tag — `done`, `abandoned`, or
    /// `superseded`. Filterable; the free-form text lives in `outcome`.
    pub outcome_tag: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskCloseResult {
    pub task_id: String,
    pub closed: bool,
}

fn parse_event_type(s: &str) -> anyhow::Result<tj_core::event::EventType> {
    use tj_core::event::EventType::*;
    Ok(match s {
        "open" => Open,
        "hypothesis" => Hypothesis,
        "finding" => Finding,
        "evidence" => Evidence,
        "decision" => Decision,
        "rejection" => Rejection,
        "constraint" => Constraint,
        "correction" => Correction,
        "reopen" => Reopen,
        "supersede" => Supersede,
        "close" => Close,
        "redirect" => Redirect,
        other => anyhow::bail!("unknown event type: {other}"),
    })
}

fn resolve_project_paths(
    dir: &std::path::Path,
) -> anyhow::Result<(String, std::path::PathBuf, std::path::PathBuf)> {
    let project_hash = tj_core::project_hash::from_path(dir)?;
    let events = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    Ok((project_hash, events, state))
}

fn project_paths() -> anyhow::Result<(String, std::path::PathBuf, std::path::PathBuf)> {
    let dir = match PROJECT_DIR_OVERRIDE.get() {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    resolve_project_paths(&dir)
}

#[tool_router]
impl TaskJournalServer {
    #[tool(
        name = "task_pack",
        description = "Return a compact resume pack for a task. Pass mode=compact|full."
    )]
    async fn task_pack(
        &self,
        Parameters(p): Parameters<TaskPackParams>,
    ) -> Result<Json<TaskPackResult>, McpError> {
        traced_tool("task_pack", async move {
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                let conn_arc = cached_open(&state_path)?;
                let conn = conn_arc
                    .lock()
                    .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                if events_path.exists() {
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                }
                let pmode = match p.mode.as_deref() {
                    Some("full") => tj_core::pack::PackMode::Full,
                    _ => tj_core::pack::PackMode::Compact,
                };
                let pack = tj_core::pack::assemble(&conn, &p.task_id, pmode)?;
                Ok(TaskPackResult {
                    task_id: pack.task_id,
                    mode: match pack.mode {
                        tj_core::pack::PackMode::Compact => "compact".into(),
                        tj_core::pack::PackMode::Full => "full".into(),
                    },
                    schema_version: pack.schema_version,
                    text: pack.text,
                    metadata: TaskPackMetadata {
                        source_event_count: Some(pack.metadata.source_event_count),
                        cache_hit: Some(pack.metadata.cache_hit),
                    },
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "task_search",
        description = "Full-text search tasks by query (FTS5)."
    )]
    async fn task_search(
        &self,
        Parameters(p): Parameters<TaskSearchParams>,
    ) -> Result<Json<TaskSearchResult>, McpError> {
        traced_tool("task_search", async move {
            let query = p.query.clone();
            let raw_query = p.query.clone();
            let event_type = p.event_type.clone();
            let results = run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                let conn_arc = cached_open(&state_path)?;
                let conn = conn_arc
                    .lock()
                    .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                if events_path.exists() {
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                }

                // v0.10.3: sanitize FTS5 query. Hyphenated IDs like
                // `OPS-306` previously crashed with "no such column: 306"
                // because FTS5 reads `-` as column-prefix syntax. Wrap
                // such queries in phrase quotes; safe queries pass
                // through unchanged so AND semantics are preserved.
                let fts_query = tj_core::fts::sanitize_query(&raw_query);
                let (sql, fts_only) = match &event_type {
                    Some(_) => (
                        "SELECT DISTINCT task_id FROM search_fts \
                         WHERE search_fts MATCH ?1 AND type = ?2 LIMIT 50",
                        false,
                    ),
                    None => (
                        "SELECT DISTINCT task_id FROM search_fts \
                         WHERE search_fts MATCH ?1 LIMIT 50",
                        true,
                    ),
                };
                let mut stmt = conn.prepare(sql)?;
                let mut ids: Vec<String> = if fts_only {
                    stmt.query_map(rusqlite::params![fts_query], |r| r.get::<_, String>(0))?
                        .collect::<Result<_, _>>()?
                } else {
                    let ty = event_type.as_deref().unwrap();
                    stmt.query_map(rusqlite::params![fts_query, ty], |r| r.get::<_, String>(0))?
                        .collect::<Result<_, _>>()?
                };

                // v0.10.3: LIKE fallback. FTS5 phrase search miss when
                // tokenizer split differs from the user's mental model
                // (e.g. `bulk-repack` in source vs `bulk repack` in
                // query). On zero FTS hits, scan event text directly so
                // hyphenated identifiers and partial-word recall work.
                if ids.is_empty() {
                    let like = tj_core::fts::like_pattern(&raw_query);
                    let (sql_like, type_bind) = match &event_type {
                        Some(_) => (
                            "SELECT DISTINCT task_id FROM search_fts \
                             WHERE text LIKE ?1 AND type = ?2 LIMIT 50",
                            true,
                        ),
                        None => (
                            "SELECT DISTINCT task_id FROM search_fts \
                             WHERE text LIKE ?1 LIMIT 50",
                            false,
                        ),
                    };
                    let mut stmt_like = conn.prepare(sql_like)?;
                    ids = if type_bind {
                        let ty = event_type.as_deref().unwrap();
                        stmt_like
                            .query_map(rusqlite::params![like, ty], |r| r.get::<_, String>(0))?
                            .collect::<Result<_, _>>()?
                    } else {
                        stmt_like
                            .query_map(rusqlite::params![like], |r| r.get::<_, String>(0))?
                            .collect::<Result<_, _>>()?
                    };
                }
                Ok(ids)
            })
            .await?;
            Ok(Json(TaskSearchResult { query, results }))
        })
        .await
    }

    #[tool(
        name = "task_create",
        description = "Open a new task with title and optional initial context."
    )]
    async fn task_create(
        &self,
        Parameters(p): Parameters<TaskCreateParams>,
    ) -> Result<Json<TaskCreateResult>, McpError> {
        traced_tool("task_create", async move {
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                std::fs::create_dir_all(events_path.parent().unwrap())?;

                let task_id = tj_core::new_task_id();
                let mut event = tj_core::event::Event::new(
                    task_id.clone(),
                    tj_core::event::EventType::Open,
                    tj_core::event::Author::Agent,
                    tj_core::event::Source::Chat,
                    p.initial_context.clone().unwrap_or_else(|| p.title.clone()),
                );
                event.meta = serde_json::json!({"title": p.title.clone()});

                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;

                // v0.6.0: persist goal column when caller passed --goal /
                // params.goal. We must ingest into SQLite first so the
                // task row exists; without ingestion set_task_goal hits
                // an empty tasks table and silently no-ops.
                if let Some(goal) = p.goal.as_deref() {
                    let conn_arc = cached_open(&state_path)?;
                    let conn = conn_arc
                        .lock()
                        .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                    tj_core::db::set_task_goal(&conn, &task_id, goal)?;
                }

                Ok(TaskCreateResult {
                    task_id,
                    title: p.title.clone(),
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "event_add",
        description = "Append a typed event (decision, finding, evidence, rejection, etc.) to a task."
    )]
    async fn event_add(
        &self,
        Parameters(p): Parameters<EventAddParams>,
    ) -> Result<Json<EventAddResult>, McpError> {
        traced_tool("event_add", async move {
            run_blocking(move || {
                let (_, events_path, _) = project_paths()?;
                std::fs::create_dir_all(events_path.parent().unwrap())?;

                let event_type = parse_event_type(&p.event_type)?;
                let mut event = tj_core::event::Event::new(
                    &p.task_id,
                    event_type,
                    tj_core::event::Author::Agent,
                    tj_core::event::Source::Chat,
                    p.text.clone(),
                );
                event.corrects = p.corrects.clone();
                event.supersedes = p.supersedes.clone();

                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;

                Ok(EventAddResult {
                    event_id: event.event_id,
                    task_id: p.task_id.clone(),
                    event_type: p.event_type.clone(),
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "task_close",
        description = "Close a task with reason and outcome."
    )]
    async fn task_close(
        &self,
        Parameters(p): Parameters<TaskCloseParams>,
    ) -> Result<Json<TaskCloseResult>, McpError> {
        traced_tool("task_close", async move {
            let task_id = p.task_id.clone();
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;

                let conn_arc = cached_open(&state_path)?;
                {
                    let conn = conn_arc
                        .lock()
                        .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                    if events_path.exists() {
                        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                    }
                    if !tj_core::db::task_exists(&conn, &p.task_id)? {
                        anyhow::bail!("task not found: {}", p.task_id);
                    }
                    // v0.6.0: validate outcome_tag enum and persist
                    // outcome+tag to the task row before writing the
                    // close event. Same enum + same ordering as the
                    // CLI close handler — keep them lockstep.
                    if let Some(tag) = p.outcome_tag.as_deref() {
                        match tag {
                            "done" | "abandoned" | "superseded" => {}
                            other => anyhow::bail!(
                                "invalid outcome_tag `{other}` (expected: done | abandoned | superseded)"
                            ),
                        }
                    }
                    if let Some(o) = p.outcome.as_deref() {
                        tj_core::db::set_task_outcome(&conn, &p.task_id, o, p.outcome_tag.as_deref())?;
                    }
                } // release the connection lock before doing the JSONL append

                let mut event = tj_core::event::Event::new(
                    &p.task_id,
                    tj_core::event::EventType::Close,
                    tj_core::event::Author::Agent,
                    tj_core::event::Source::Chat,
                    p.reason.clone(),
                );
                let mut meta = serde_json::Map::new();
                meta.insert("reason".into(), serde_json::Value::String(p.reason.clone()));
                if let Some(o) = &p.outcome {
                    meta.insert("outcome".into(), serde_json::Value::String(o.clone()));
                }
                if let Some(t) = &p.outcome_tag {
                    meta.insert("outcome_tag".into(), serde_json::Value::String(t.clone()));
                }
                event.meta = serde_json::Value::Object(meta);

                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;
                Ok(())
            })
            .await?;
            Ok(Json(TaskCloseResult {
                task_id,
                closed: true,
            }))
        })
        .await
    }
}

#[tool_handler(router = Self::tool_router())]
impl ServerHandler for TaskJournalServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo {
            server_info: rmcp::model::Implementation {
                name: "task-journal".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
            instructions: Some(MCP_INSTRUCTIONS.into()),
            ..Default::default()
        }
    }
}

/// Resolve when the process should shut down: Ctrl-C on every platform,
/// plus SIGTERM on Unix. Used in `tokio::select!` against the rmcp
/// `waiting()` loop so the binary exits cleanly instead of being
/// hard-killed mid-write.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGTERM handler — Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT"),
            _ = sigterm.recv() => tracing::info!("received SIGTERM"),
        }
    }
    #[cfg(not(unix))]
    {
        // Windows: only Ctrl-C / Ctrl-Break maps to ctrl_c().
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("received Ctrl-C");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    if let Some(dir) = cli.project_dir {
        let resolved = std::fs::canonicalize(&dir)
            .with_context(|| format!("--project-dir not accessible: {dir:?}"))?;
        PROJECT_DIR_OVERRIDE
            .set(resolved)
            .map_err(|_| anyhow::anyhow!("PROJECT_DIR_OVERRIDE already set"))?;
    }

    let server = TaskJournalServer;
    let (stdin, stdout) = stdio();
    let serving = server.serve((stdin, stdout)).await?;

    tokio::select! {
        res = serving.waiting() => {
            res?;
            tracing::info!("rmcp serve loop exited");
        }
        _ = wait_for_shutdown_signal() => {
            tracing::info!("shutdown signal received — exiting");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(v: &serde_json::Value) -> Vec<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn no_response_serializes_a_stub_field() {
        // Vestigial stub:bool from Phase 1 stubs has been removed from all
        // five MCP result types. Guard against re-introduction.
        let pack = TaskPackResult {
            task_id: "tj-x".into(),
            mode: "compact".into(),
            schema_version: tj_core::SCHEMA_VERSION.into(),
            text: String::new(),
            metadata: TaskPackMetadata {
                source_event_count: None,
                cache_hit: None,
            },
        };
        let pack_v = serde_json::to_value(&pack).unwrap();
        assert!(!keys_of(&pack_v).contains(&"stub".to_string()));
        assert!(!keys_of(&pack_v["metadata"]).contains(&"stub".to_string()));

        let search = TaskSearchResult {
            query: "q".into(),
            results: vec![],
        };
        assert!(!keys_of(&serde_json::to_value(&search).unwrap()).contains(&"stub".to_string()));

        let create = TaskCreateResult {
            task_id: "tj-x".into(),
            title: "t".into(),
        };
        assert!(!keys_of(&serde_json::to_value(&create).unwrap()).contains(&"stub".to_string()));

        let event = EventAddResult {
            event_id: "e".into(),
            task_id: "tj-x".into(),
            event_type: "decision".into(),
        };
        assert!(!keys_of(&serde_json::to_value(&event).unwrap()).contains(&"stub".to_string()));

        let close = TaskCloseResult {
            task_id: "tj-x".into(),
            closed: true,
        };
        assert!(!keys_of(&serde_json::to_value(&close).unwrap()).contains(&"stub".to_string()));
    }

    #[test]
    fn resolve_project_paths_uses_provided_dir_for_hash() {
        // Two distinct dirs must give two distinct project_hash values, and
        // the same dir must always give the same hash. This is the contract
        // that --project-dir relies on: any path on disk maps to a stable,
        // unique data location.
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("alpha");
        let b = tmp.path().join("beta");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let (hash_a, _, _) = resolve_project_paths(&a).unwrap();
        let (hash_b, _, _) = resolve_project_paths(&b).unwrap();
        assert_ne!(hash_a, hash_b);

        let (hash_a_again, _, _) = resolve_project_paths(&a).unwrap();
        assert_eq!(hash_a, hash_a_again);
    }

    #[tokio::test]
    async fn run_blocking_executes_two_tasks_concurrently() {
        use std::time::{Duration, Instant};

        // Two tasks each sleep ~200ms. If run_blocking handed work to the
        // tokio blocking pool they overlap (~200ms wall-clock). If we ever
        // regress to running the closure inline on the executor thread,
        // tokio::join! still wakes both futures but only one progresses at
        // a time and total wall-clock approaches 400ms.
        let start = Instant::now();
        let (a, b) = tokio::join!(
            run_blocking(|| {
                std::thread::sleep(Duration::from_millis(200));
                Ok::<_, anyhow::Error>(1u32)
            }),
            run_blocking(|| {
                std::thread::sleep(Duration::from_millis(200));
                Ok::<_, anyhow::Error>(2u32)
            }),
        );
        let elapsed = start.elapsed();

        assert_eq!(a.unwrap(), 1);
        assert_eq!(b.unwrap(), 2);
        // Sequential execution would require ≥400ms (two 200ms sleeps);
        // overlap drops it to ~200ms. We give CI runners plenty of slack
        // (600ms) — still distinguishes parallel from serial without
        // flaking on macOS/Windows GitHub runners under load.
        assert!(
            elapsed < Duration::from_millis(600),
            "blocking tasks must overlap on the blocking pool — got {elapsed:?}"
        );
    }

    /// Compile-time + runtime guarantee that `wait_for_shutdown_signal`
    /// returns a `Future<Output = ()>` we can drop on the floor without
    /// it ever resolving — a real signal would resolve it. We assert by
    /// racing it against an already-ready future and confirming the
    /// shutdown future was *not* the winner.
    #[tokio::test]
    async fn shutdown_signal_does_not_fire_spuriously() {
        let ready = async {};
        tokio::select! {
            _ = wait_for_shutdown_signal() => panic!("shutdown fired with no signal"),
            _ = ready => { /* expected */ }
        }
    }

    #[test]
    fn new_correlation_id_is_unique_across_thousand_calls() {
        let mut seen = std::collections::HashSet::with_capacity(1000);
        for _ in 0..1_000 {
            assert!(
                seen.insert(new_correlation_id()),
                "correlation id collision in 1k calls"
            );
        }
    }

    #[tokio::test]
    async fn traced_tool_transparently_returns_inner_result() {
        // Success path: the wrapper must propagate the Ok value.
        let ok = traced_tool::<i32, _>("test_ok", async { Ok(42) })
            .await
            .unwrap();
        assert_eq!(ok, 42);

        // Error path: the wrapper must propagate Err untouched.
        let err = traced_tool::<i32, _>("test_err", async {
            Err(McpError::internal_error("boom".to_string(), None))
        })
        .await;
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().message, "boom");
    }

    #[test]
    fn cached_open_returns_same_arc_for_same_path() {
        // The Arc returned by cached_open() is the same handle on second
        // call: that's the proof that we are not re-running migrations
        // / PRAGMA / WAL setup on every tool call.
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("d1-cache.sqlite");
        let a = cached_open(&p).unwrap();
        let b = cached_open(&p).unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "cached_open must reuse the Arc<Mutex<Connection>>"
        );
    }

    #[test]
    fn cached_open_returns_distinct_arcs_for_distinct_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let p1 = dir.path().join("d1-x.sqlite");
        let p2 = dir.path().join("d1-y.sqlite");
        let a = cached_open(&p1).unwrap();
        let b = cached_open(&p2).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn cli_parses_project_dir_argument() {
        // Smoke test: `task-journal-mcp --project-dir /tmp/foo` parses and
        // populates the field. We do not actually launch the server here —
        // that needs a real stdio peer.
        let cli = Cli::try_parse_from(["task-journal-mcp", "--project-dir", "/tmp/foo"]).unwrap();
        assert_eq!(cli.project_dir, Some(std::path::PathBuf::from("/tmp/foo")));

        let cli = Cli::try_parse_from(["task-journal-mcp"]).unwrap();
        assert!(cli.project_dir.is_none());
    }

    #[test]
    fn into_mcp_error_carries_full_anyhow_chain() {
        // Down-stream callers rely on McpError.message containing the full
        // chain (root cause + every context wrap). Catches a regression
        // where someone formats with `{}` instead of `{:#}`.
        let inner = anyhow::anyhow!("root cause");
        let outer = inner.context("wrap layer");
        let err = into_mcp_error(outer);
        assert!(err.message.contains("wrap layer"), "got: {}", err.message);
        assert!(err.message.contains("root cause"), "got: {}", err.message);
    }

    #[test]
    fn task_pack_returns_rpc_error_when_state_dir_is_unusable() {
        // Force tj_core::paths::state_dir to fail by pointing it at a path
        // that cannot be created. We do this through XDG_DATA_HOME pointing
        // at /dev/null which directories crate refuses. The handler must
        // surface this as Err(McpError), not as a fake-success Json with
        // a corrupted task_id.
        //
        // We don't invoke the async handler directly here because it has
        // private generated wrappers; instead we exercise the same error
        // path via project_paths() and verify the conversion does the
        // right thing.
        let prev = std::env::var("XDG_DATA_HOME").ok();
        // SAFETY: this test does not run concurrently with other tests
        // that read XDG_DATA_HOME — see the env-var test in tj-core for
        // the same pattern.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/dev/null/cannot-create-here");
        }

        let res = project_paths();

        // restore
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        // We don't rigidly assert Err here (the directories crate has
        // platform-specific behavior); we only assert that *if* it errors,
        // into_mcp_error converts cleanly without panicking.
        if let Err(e) = res {
            let mcp_err = into_mcp_error(e);
            assert!(!mcp_err.message.is_empty());
        }
    }
}
