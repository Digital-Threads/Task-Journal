//! Phase 1 smoke test: verify the binary builds and is reachable from CARGO_BIN_EXE.
//! Phase 3 will wire a real MCP client for protocol-level tests.

#[test]
fn binary_exists_after_build() {
    let p = env!("CARGO_BIN_EXE_task-journal-mcp");
    assert!(std::path::Path::new(p).exists(), "binary not built: {p}");
}
