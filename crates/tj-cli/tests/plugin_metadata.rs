//! The plugin ships three JSON manifests that each carry a version, and
//! Claude Code uses them for different things: the marketplace entry decides
//! what `claude plugin update` offers, `plugin.json` names the installed
//! version, and `package.json` is what an npm install would report. They drifted
//! apart once (0.14.2 / 0.28.4 / 0.10.3) and the marketplace advertised a
//! three-month-old release. These tests pin them to the crate version.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("resolve repo root")
}

fn json_at(rel: &str) -> serde_json::Value {
    let path = repo_root().join(rel);
    let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("parse {rel}: {e}"))
}

#[test]
fn plugin_manifests_carry_the_crate_version() {
    let want = env!("CARGO_PKG_VERSION");

    let plugin = json_at("plugin/.claude-plugin/plugin.json");
    assert_eq!(plugin["version"], want, "plugin.json version is stale");

    let package = json_at("plugin/package.json");
    assert_eq!(package["version"], want, "plugin/package.json is stale");

    let market = json_at(".claude-plugin/marketplace.json");
    assert_eq!(
        market["metadata"]["version"], want,
        "marketplace metadata version is stale"
    );
    assert_eq!(
        market["plugins"][0]["version"], want,
        "marketplace plugin entry version is stale"
    );
}

/// The MCP server is declared once, in `plugin.json`. A second declaration in
/// `plugin/.mcp.json` made Claude Code see the same stdio server twice.
#[test]
fn the_mcp_server_is_declared_exactly_once() {
    let plugin = json_at("plugin/.claude-plugin/plugin.json");
    assert!(
        plugin["mcpServers"]["task-journal"]["command"].is_string(),
        "plugin.json must declare the MCP server"
    );
    assert!(
        !repo_root().join("plugin/.mcp.json").exists(),
        "plugin/.mcp.json duplicates the plugin.json declaration"
    );
}
