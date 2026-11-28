//! Client example: drives the benchmark's Forge client API against an
//! in-process stand-in for the Forge runtime (no token, no server):
//! selects a model, then asks it for a fixture command.
//!
//! Run with `cargo run -p forge-agent-driver --example forge_client`.

use std::cell::RefCell;
use std::collections::BTreeMap;

use forge_agent_driver::benchmark::{Case, ForgeClient, ForgeError};
use serde_json::{Value, json};

/// Answers the runtime and chat endpoints the benchmark uses.
struct LocalRuntime {
    model: RefCell<Option<String>>,
}

impl ForgeClient for LocalRuntime {
    fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<Value, ForgeError> {
        match (method, path) {
            ("GET", "/v1/runtime") => Ok(match &*self.model.borrow() {
                Some(model) => json!({"state": "ready", "model": model}),
                None => json!({"state": "stopped"}),
            }),
            ("PUT", "/v1/runtime") => {
                let model = payload
                    .and_then(|body| body.get("model"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                *self.model.borrow_mut() = model;
                Ok(Value::Null)
            }
            ("POST", "/v1/chat/completions") => Ok(json!({
                "choices": [{
                    "message": {"content": "wc -l items.txt"},
                    "finish_reason": "stop",
                }],
                "usage": {"completion_tokens": 5},
            })),
            _ => Err(ForgeError(format!("{method} {path} returned HTTP 404"))),
        }
    }
}

fn main() -> Result<(), ForgeError> {
    let forge = LocalRuntime {
        model: RefCell::new(None),
    };
    forge.select("qwen2.5-coder-7b")?;
    println!("runtime: {}", forge.request("GET", "/v1/runtime", None)?);

    let case = Case {
        id: "count".to_string(),
        task: "Count the lines in the file.".to_string(),
        files: BTreeMap::from([("items.txt".to_string(), "a\nb\n".to_string())]),
        reference: "wc -l items.txt".to_string(),
        stdout: "2 items.txt\n".to_string(),
    };
    let (command, finish, usage) = forge.generate("qwen2.5-coder-7b", &case)?;
    println!(
        "command: {} (finish {}, usage {usage})",
        command.unwrap_or_default(),
        finish.unwrap_or_default()
    );
    Ok(())
}
