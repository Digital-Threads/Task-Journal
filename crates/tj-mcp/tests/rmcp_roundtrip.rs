//! Compile-time + serde-shape integration test for the rmcp client +
//! transport stack.
//!
//! What this file *does* prove:
//!   - rmcp 0.3 with the `client` feature compiles against this
//!     workspace and our pinned rust toolchain.
//!   - `CallToolRequestParam` round-trips through serde — i.e. the
//!     JSON-RPC envelope we'll send and parse hasn't shifted shape.
//!   - `ClientHandler` + `ClientInfo::default()` compile against
//!     each other — the two pieces a downstream user must wire.
//!
//! What this file does *not* prove:
//!   - End-to-end tool dispatch through `TaskJournalServer`. The
//!     server is defined in `main.rs` (binary crate) and is not
//!     reachable from an integration test. Driving the real
//!     handlers needs `TaskJournalServer` extracted into a
//!     `tj-mcp` lib target — tracked as a follow-up; until then
//!     the same code paths are covered end-to-end via the CLI
//!     integration tests in `tj-cli/tests/cli.rs`.

use rmcp::{model::CallToolRequestParam, model::ClientInfo, ClientHandler};

#[derive(Debug, Clone, Default)]
struct DummyClientHandler;

impl ClientHandler for DummyClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

#[test]
fn dummy_client_handler_compiles_and_provides_default_info() {
    let h = DummyClientHandler;
    let _ = h.get_info();
}

#[test]
fn rmcp_call_tool_request_param_round_trips_via_serde() {
    let req = CallToolRequestParam {
        name: "task_create".into(),
        arguments: Some(
            serde_json::json!({"title": "hello"})
                .as_object()
                .unwrap()
                .clone(),
        ),
    };
    let s = serde_json::to_string(&req).unwrap();
    let back: CallToolRequestParam = serde_json::from_str(&s).unwrap();
    assert_eq!(back.name, req.name);
    assert_eq!(back.arguments, req.arguments);
}

/// Compile-only check that `tokio::io::duplex` returns a transport
/// pair acceptable to rmcp's `ServiceExt::serve`. This catches a
/// regression where `tokio::io::DuplexStream` no longer satisfies
/// the trait bounds without us having to actually run the server.
#[allow(dead_code)]
fn _duplex_is_a_valid_rmcp_transport() {
    fn assert_async_read_write<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static>() {
    }
    assert_async_read_write::<tokio::io::DuplexStream>();
}
