//! task-journal-mcp: MCP server entry point.
//!
//! Phase 1 wires the server with a `tool_router` containing 5 stub tools.
//! Phase 2+ replaces stubs with real implementations.

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

#[tool_router]
impl TaskJournalServer {
    #[tool(name = "task_pack", description = "Return a compact resume pack for a task. Pass mode=compact|full.")]
    async fn task_pack(
        &self,
        Parameters(p): Parameters<TaskPackParams>,
    ) -> Json<TaskPackResult> {
        Json(TaskPackResult {
            task_id: p.task_id,
            mode: p.mode.unwrap_or_else(|| "compact".into()),
            schema_version: "1.0".into(),
            text: "[STUB] task_pack not yet implemented (Phase 2)".into(),
            metadata: TaskPackMetadata { stub: true },
        })
    }

    #[tool(name = "task_search", description = "Search tasks by query, status, project.")]
    async fn task_search(
        &self,
        Parameters(p): Parameters<TaskSearchParams>,
    ) -> Json<TaskSearchResult> {
        Json(TaskSearchResult {
            query: p.query,
            results: vec![],
            stub: true,
        })
    }

    #[tool(name = "task_create", description = "Open a new task with title and optional initial context.")]
    async fn task_create(
        &self,
        Parameters(p): Parameters<TaskCreateParams>,
    ) -> Json<TaskCreateResult> {
        Json(TaskCreateResult {
            // ULID chars 10-15 are random (chars 0-9 are timestamp, would collide
            // for tasks within 12 days). See tj-cli for the canonical comment.
            task_id: format!("tj-stub-{}", &ulid::Ulid::new().to_string()[10..16].to_lowercase()),
            title: p.title,
            stub: true,
        })
    }

    #[tool(name = "event_add", description = "Append a typed event (decision, finding, etc.) to a task.")]
    async fn event_add(
        &self,
        Parameters(p): Parameters<EventAddParams>,
    ) -> Json<EventAddResult> {
        Json(EventAddResult {
            event_id: ulid::Ulid::new().to_string(),
            task_id: p.task_id,
            event_type: p.event_type,
            stub: true,
        })
    }

    #[tool(name = "task_close", description = "Close a task with reason and outcome.")]
    async fn task_close(
        &self,
        Parameters(p): Parameters<TaskCloseParams>,
    ) -> Json<TaskCloseResult> {
        Json(TaskCloseResult {
            task_id: p.task_id,
            closed: true,
            stub: true,
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
