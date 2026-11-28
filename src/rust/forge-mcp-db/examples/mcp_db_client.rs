//! Client example: composes an MCP app the way the binary does, but
//! binds an in-memory query backend instead of PostgreSQL, then lists
//! tools and runs one.
//!
//! Run with `cargo run -p forge-mcp-db --example mcp_db_client`.

use std::sync::Arc;

use forge_mcp_db::app::McpApp;
use forge_mcp_db::auth::Principal;
use forge_mcp_db::backend::{Cell, Param, QueryBackend, QueryOutcome, ToolError};
use forge_mcp_db::composition::{create_limiter, select_auth};
use forge_mcp_db::config::load_config;
use forge_mcp_db::presence::NoPresence;
use serde_json::json;

/// An in-memory database that knows one table.
struct InMemoryCatalog;

impl QueryBackend for InMemoryCatalog {
    fn run(
        &self,
        _sql: &str,
        _params: &[Param],
        _limit: i64,
        _offset: i64,
        _server_side: bool,
    ) -> Result<QueryOutcome, ToolError> {
        Ok(QueryOutcome {
            columns: vec!["table_schema".to_string(), "table_name".to_string()],
            rows: vec![vec![
                Cell::Text("public".to_string()),
                Cell::Text("tasks".to_string()),
            ]],
            has_more: false,
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        std::env::temp_dir().join(format!("forge-mcp-db-example-{}.json", std::process::id()));
    std::fs::write(&path, "{}")?;
    let config = load_config(path.to_str().unwrap_or_default());
    let _ = std::fs::remove_file(&path);
    let config = Arc::new(config.map_err(|error| error.to_string())?);

    // Presence is off: the NoPresence null object answers for it.
    let app = McpApp::new(
        Arc::clone(&config),
        select_auth(&config, None),
        Arc::new(InMemoryCatalog),
        create_limiter(&config),
        Arc::new(NoPresence),
        None,
    );
    let principal = Principal {
        token_id: "example".to_string(),
        name: "example".to_string(),
        scopes: ["read".to_string()].into_iter().collect(),
        subject: None,
    };

    let tools = app
        .handle_rpc(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            &principal,
        )
        .ok_or("tools/list returned no reply")?;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    println!("tools: {}", names.join(", "));

    let reply = app.call_tool(&json!({"name": "list_tables", "arguments": {}}), &principal);
    println!("list_tables: {reply}");
    Ok(())
}
