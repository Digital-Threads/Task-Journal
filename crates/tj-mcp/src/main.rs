//! task-journal-mcp: MCP server entry point.
//!
//! Phase 1 wires the server with a `tool_router` containing 5 stub tools.
//! Phase 2+ replaces stubs with real implementations.

use anyhow::Result;
use rmcp::{
    transport::io::stdio,
    tool, tool_router, tool_handler,
    ServerHandler, ServiceExt,
};

#[derive(Clone, Default)]
pub struct TaskJournalServer;

#[tool_router]
impl TaskJournalServer {
    // Stub tools added in Tasks 17-18.
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
