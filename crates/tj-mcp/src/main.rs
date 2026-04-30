//! task-journal-mcp: MCP server entry point.
//!
//! Phase 2 wires real implementations into all 5 tools, calling tj-core.

use anyhow::Result;
use std::future::Future;
use rmcp::{
    handler::server::tool::Parameters,
    handler::server::wrapper::Json,
    transport::io::stdio,
    tool, tool_router, tool_handler,
    ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};

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
    pub stub: bool,
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
    pub stub: bool,
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
    pub stub: bool,
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
    pub stub: bool,
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
    pub stub: bool,
}

fn parse_event_type(s: &str) -> anyhow::Result<tj_core::event::EventType> {
    use tj_core::event::EventType::*;
    Ok(match s {
        "open" => Open, "hypothesis" => Hypothesis, "finding" => Finding,
        "evidence" => Evidence, "decision" => Decision, "rejection" => Rejection,
        "constraint" => Constraint, "correction" => Correction,
        "reopen" => Reopen, "supersede" => Supersede,
        "close" => Close, "redirect" => Redirect,
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
    #[tool(name = "task_pack", description = "Return a compact resume pack for a task. Pass mode=compact|full.")]
    async fn task_pack(
        &self,
        Parameters(p): Parameters<TaskPackParams>,
    ) -> Json<TaskPackResult> {
        let result = (|| -> anyhow::Result<TaskPackResult> {
            let (project_hash, events_path, state_path) = project_paths()?;
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
            }
            let pmode = match p.mode.as_deref() {
                Some("full") => tj_core::pack::PackMode::Full,
                _ => tj_core::pack::PackMode::Compact,
            };
            let pack = tj_core::pack::assemble(&conn, &p.task_id, pmode)?;
            Ok(TaskPackResult {
                task_id: pack.task_id,
                mode: match pack.mode { tj_core::pack::PackMode::Compact => "compact".into(), tj_core::pack::PackMode::Full => "full".into() },
                schema_version: pack.schema_version,
                text: pack.text,
                metadata: TaskPackMetadata {
                    stub: false,
                    source_event_count: Some(pack.metadata.source_event_count),
                    cache_hit: Some(pack.metadata.cache_hit),
                },
            })
        })();
        match result {
            Ok(r) => Json(r),
            Err(e) => Json(TaskPackResult {
                task_id: p.task_id,
                mode: p.mode.unwrap_or_else(|| "compact".into()),
                schema_version: "1.0".into(),
                text: format!("[error] {e}"),
                metadata: TaskPackMetadata { stub: false, source_event_count: None, cache_hit: None },
            }),
        }
    }

    #[tool(name = "task_search", description = "Full-text search tasks by query (FTS5).")]
    async fn task_search(
        &self,
        Parameters(p): Parameters<TaskSearchParams>,
    ) -> Json<TaskSearchResult> {
        let result = (|| -> anyhow::Result<Vec<String>> {
            let (project_hash, events_path, state_path) = project_paths()?;
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
            }
            let mut stmt = conn.prepare(
                "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT 50"
            )?;
            let ids: Vec<String> = stmt
                .query_map(rusqlite::params![p.query], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            Ok(ids)
        })();
        Json(TaskSearchResult {
            query: p.query,
            results: result.unwrap_or_default(),
            stub: false,
        })
    }

    #[tool(name = "task_create", description = "Open a new task with title and optional initial context.")]
    async fn task_create(
        &self,
        Parameters(p): Parameters<TaskCreateParams>,
    ) -> Json<TaskCreateResult> {
        let result = (|| -> anyhow::Result<TaskCreateResult> {
            let (_, events_path, _) = project_paths()?;
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            let task_id = format!("tj-{}", &ulid::Ulid::new().to_string()[10..16].to_lowercase());
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

            Ok(TaskCreateResult { task_id, title: p.title.clone(), stub: false })
        })();
        Json(result.unwrap_or_else(|e| TaskCreateResult {
            task_id: format!("[error] {e}"), title: p.title, stub: false
        }))
    }

    #[tool(name = "event_add", description = "Append a typed event (decision, finding, evidence, rejection, etc.) to a task.")]
    async fn event_add(
        &self,
        Parameters(p): Parameters<EventAddParams>,
    ) -> Json<EventAddResult> {
        let result = (|| -> anyhow::Result<EventAddResult> {
            let (_, events_path, _) = project_paths()?;
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            let event_type = parse_event_type(&p.event_type)?;
            let mut event = tj_core::event::Event::new(
                &p.task_id, event_type,
                tj_core::event::Author::Agent, tj_core::event::Source::Chat,
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
                stub: false,
            })
        })();
        Json(result.unwrap_or_else(|e| EventAddResult {
            event_id: format!("[error] {e}"),
            task_id: p.task_id, event_type: p.event_type, stub: false,
        }))
    }

    #[tool(name = "task_close", description = "Close a task with reason and outcome.")]
    async fn task_close(
        &self,
        Parameters(p): Parameters<TaskCloseParams>,
    ) -> Json<TaskCloseResult> {
        let result = (|| -> anyhow::Result<()> {
            let (_, events_path, _) = project_paths()?;
            let mut event = tj_core::event::Event::new(
                &p.task_id, tj_core::event::EventType::Close,
                tj_core::event::Author::Agent, tj_core::event::Source::Chat,
                p.reason.clone(),
            );
            let mut meta = serde_json::Map::new();
            meta.insert("reason".into(), serde_json::Value::String(p.reason.clone()));
            if let Some(o) = &p.outcome { meta.insert("outcome".into(), serde_json::Value::String(o.clone())); }
            event.meta = serde_json::Value::Object(meta);

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            Ok(())
        })();
        Json(TaskCloseResult {
            task_id: p.task_id,
            closed: result.is_ok(),
            stub: false,
        })
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

    let server = TaskJournalServer::default();
    let (stdin, stdout) = stdio();
    server.serve((stdin, stdout)).await?.waiting().await?;
    Ok(())
}
