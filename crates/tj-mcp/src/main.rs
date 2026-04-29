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
