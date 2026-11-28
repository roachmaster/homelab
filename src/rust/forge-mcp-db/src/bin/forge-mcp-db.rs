//! The Homelab MCP server binary (mcp_server.py's main):
//! --config PATH [--check-config]. Static tokens keep working
//! when the OAuth state database cannot be opened.

use std::sync::Arc;

use forge_mcp_db::composition::AppFactory;
use forge_mcp_db::config::load_config;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path: Option<String> = None;
    let mut check_config = false;
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--config" => {
                config_path = argv.get(index + 1).cloned();
                index += 2;
            }
            "--check-config" => {
                check_config = true;
                index += 1;
            }
            other => {
                eprintln!("unrecognized argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(config_path) = config_path else {
        eprintln!("--config is required");
        std::process::exit(2);
    };
    let config = match load_config(&config_path) {
        Ok(config) => Arc::new(config),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let app = match AppFactory::new(config.clone()).create() {
        Ok(app) => Arc::new(app),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if let Some(error) = app.auth.error() {
        eprintln!("token file problem: {error}");
        if check_config {
            std::process::exit(2);
        }
    }
    if check_config {
        println!("config ok");
        return;
    }
    let listener =
        match std::net::TcpListener::bind((config.listen_host.as_str(), config.listen_port)) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!(
                    "cannot bind {}:{}: {error}",
                    config.listen_host, config.listen_port
                );
                std::process::exit(1);
            }
        };
    if let Ok(address) = listener.local_addr() {
        eprintln!("listening on {address}");
    }
    forge_mcp_db::http::serve(listener, app);
}
