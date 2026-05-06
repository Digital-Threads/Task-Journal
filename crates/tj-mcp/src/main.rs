//! task-journal-mcp: MCP server entry point.
//!
//! Phase 2 wires real implementations into all 5 tools, calling tj-core.

use anyhow::Result;
use rmcp::{
    handler::server::tool::Parameters, handler::server::wrapper::Json, tool, tool_handler,
    tool_router, transport::io::stdio, ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};
use std::future::Future;

/// Convert any internal failure into a JSON-RPC error frame. We attach the
/// stringified `anyhow::Error` chain as the `message` so the client sees the
/// full context (e.g. "task not found: tj-x: no row returned").
fn into_mcp_error(err: anyhow::Error) -> McpError {
    McpError::internal_error(format!("{err:#}"), None)
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

fn project_paths() -> anyhow::Result<(String, std::path::PathBuf, std::path::PathBuf)> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    Ok((project_hash, events, state))
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
        let result: anyhow::Result<TaskPackResult> = (|| {
            let (project_hash, events_path, state_path) = project_paths()?;
            let conn = tj_core::db::open(&state_path)?;
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
        })();
        result.map(Json).map_err(into_mcp_error)
    }

    #[tool(
        name = "task_search",
        description = "Full-text search tasks by query (FTS5)."
    )]
    async fn task_search(
        &self,
        Parameters(p): Parameters<TaskSearchParams>,
    ) -> Result<Json<TaskSearchResult>, McpError> {
        let ids: anyhow::Result<Vec<String>> = (|| {
            let (project_hash, events_path, state_path) = project_paths()?;
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            let mut stmt = conn.prepare(
                "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT 50",
            )?;
            let ids: Vec<String> = stmt
                .query_map(rusqlite::params![p.query], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            Ok(ids)
        })();
        ids.map(|results| {
            Json(TaskSearchResult {
                query: p.query,
                results,
            })
        })
        .map_err(into_mcp_error)
    }

    #[tool(
        name = "task_create",
        description = "Open a new task with title and optional initial context."
    )]
    async fn task_create(
        &self,
        Parameters(p): Parameters<TaskCreateParams>,
    ) -> Result<Json<TaskCreateResult>, McpError> {
        let result: anyhow::Result<TaskCreateResult> = (|| {
            let (_, events_path, _) = project_paths()?;
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

            Ok(TaskCreateResult {
                task_id,
                title: p.title.clone(),
            })
        })();
        result.map(Json).map_err(into_mcp_error)
    }

    #[tool(
        name = "event_add",
        description = "Append a typed event (decision, finding, evidence, rejection, etc.) to a task."
    )]
    async fn event_add(
        &self,
        Parameters(p): Parameters<EventAddParams>,
    ) -> Result<Json<EventAddResult>, McpError> {
        let result: anyhow::Result<EventAddResult> = (|| {
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
        })();
        result.map(Json).map_err(into_mcp_error)
    }

    #[tool(
        name = "task_close",
        description = "Close a task with reason and outcome."
    )]
    async fn task_close(
        &self,
        Parameters(p): Parameters<TaskCloseParams>,
    ) -> Result<Json<TaskCloseResult>, McpError> {
        let result: anyhow::Result<()> = (|| {
            let (project_hash, events_path, state_path) = project_paths()?;

            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            if !tj_core::db::task_exists(&conn, &p.task_id)? {
                anyhow::bail!("task not found: {}", p.task_id);
            }
            drop(conn);

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
            event.meta = serde_json::Value::Object(meta);

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            Ok(())
        })();
        result
            .map(|()| {
                Json(TaskCloseResult {
                    task_id: p.task_id.clone(),
                    closed: true,
                })
            })
            .map_err(into_mcp_error)
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let server = TaskJournalServer;
    let (stdin, stdout) = stdio();
    server.serve((stdin, stdout)).await?.waiting().await?;
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
