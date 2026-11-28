//! Step bindings for features/grok_acp_permissions.feature, over the
//! real Grok ACP transport and permission broker in forge-work-worker.

use forge_scene::bdd::{ensure, verify, Steps};
use forge_work_worker::grok_acp::{wire, GrokAcpTransport, NativePaths, Permissions};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const SID: &str = "00000000-0000-4000-8000-000000000041";

/// A private temporary directory, removed when the scenario ends.
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "forge-scene-acp-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct World {
    transport: Option<GrokAcpTransport>,
    captured: Option<Value>,
    reply: Option<Value>,
    update_error: Option<String>,
    update_output: Option<Vec<u8>>,
    permissions: Option<Permissions>,
    admitted: Option<bool>,
    directory: Option<Directory>,
}

fn tool(name: &str, raw: Value) -> Value {
    json!({"sessionUpdate": "tool_call", "toolCallId": "call-1", "rawInput": raw,
           "_meta": {"x.ai/tool": {"version": 1, "namespace": "grok_build", "name": name}}})
}

fn descriptor(command: &str) -> Value {
    json!({"toolCallId": "call-1",
           "_meta": {"x.ai/tool": {"version": 1, "namespace": "grok_build",
               "name": "run_terminal_command",
               "input": {"command": command, "description": "read inventory"}}}})
}

fn start_transport() -> Result<GrokAcpTransport, String> {
    let paths = NativePaths::new(PathBuf::from("/nonexistent"), Vec::new(), Vec::new());
    let permissions = Permissions::new("plan", paths, false)?;
    let mut transport = GrokAcpTransport::new("Bound task", permissions)?;
    transport.drain();
    transport.feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-initialize",
                                "result": {"protocolVersion": 1}})))?;
    transport.drain();
    transport.feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-new",
                                "result": {"sessionId": SID}})))?;
    transport.drain();
    Ok(transport)
}

fn update(world: &mut World, value: Value) -> Result<Vec<u8>, String> {
    let transport = world.transport.as_mut().ok_or("no transport")?;
    transport.feed(&wire(&json!({"jsonrpc": "2.0", "method": "session/update",
                                "params": {"sessionId": SID, "update": value}})))
}

fn request_permission(world: &mut World, call: Value) -> Result<(), String> {
    let options = json!([
        {"optionId": "bdd-allow", "name": "Allow once", "kind": "allow_once"},
        {"optionId": "bdd-reject", "name": "Reject once", "kind": "reject_once"},
        {"optionId": "bdd-always", "name": "Always", "kind": "allow_always"},
    ]);
    let transport = world.transport.as_mut().ok_or("no transport")?;
    transport.feed(&wire(&json!({"jsonrpc": "2.0", "id": "permission-1",
        "method": "session/request_permission",
        "params": {"sessionId": SID, "toolCall": call, "options": options}})))?;
    let reply: Value =
        serde_json::from_slice(&transport.drain()).map_err(|e| e.to_string())?;
    world.reply = Some(reply);
    Ok(())
}

fn steps() -> Steps<World> {
    Steps::new()
        .given("the permission broker can judge a captured tool call", |w: &mut World, _| {
            w.transport = Some(start_transport()?);
            Ok(())
        })
        .given("the broker can admit read-only searches", |w, _| {
            let directory = Directory::new();
            let workspace = directory.0.join("workspace");
            fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
            fs::write(workspace.join("file"), "source").map_err(|e| e.to_string())?;
            let paths = NativePaths::new(workspace, Vec::new(), Vec::new());
            w.permissions = Some(Permissions::new("plan", paths, false)?);
            w.directory = Some(directory);
            Ok(())
        })
        .when("a terminal call is captured with its arguments", |w, _| {
            let call = tool(
                "run_terminal_command",
                json!({"command": "git status", "description": "read inventory"}),
            );
            update(w, call.clone())?;
            w.captured = Some(call);
            Ok(())
        })
        .when(
            "a permission request restates them in a complete descriptor",
            |w, _| request_permission(w, descriptor("git status")),
        )
        .when("a permission request describes a different command", |w, _| {
            request_permission(w, descriptor("git push"))
        })
        .when("a permission request presents changed arguments", |w, _| {
            let changed = tool("run_terminal_command", json!({"command": "git push"}));
            request_permission(w, changed)
        })
        .when("a progress update reports different arguments", |w, _| {
            match update(
                w,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "call-1",
                       "rawInput": {"command": "git push"}}),
            ) {
                Ok(output) => w.update_output = Some(output),
                Err(error) => w.update_error = Some(error),
            }
            Ok(())
        })
        .when("a tool call arrives with incomplete native metadata", |w, _| {
            let partial = json!({"sessionUpdate": "tool_call", "toolCallId": "call-9",
                "rawInput": {"command": "git status"},
                "_meta": {"x.ai/tool": {"name": "run_terminal_command"}}});
            match update(w, partial) {
                Ok(output) => w.update_output = Some(output),
                Err(error) => w.update_error = Some(error),
            }
            Ok(())
        })
        .when("a grep names no path and stays within its bounds", |w, _| {
            let permissions = w.permissions.as_ref().ok_or("no broker")?;
            w.admitted = Some(permissions.admit(&tool(
                "grep",
                json!({"pattern": "needle", "glob": null, "-i": false,
                       "head_limit": 25, "type": "rust"}),
            )));
            Ok(())
        })
        .when("a grep asks for more context than the bound allows", |w, _| {
            let permissions = w.permissions.as_ref().ok_or("no broker")?;
            w.admitted = Some(permissions.admit(&tool(
                "grep",
                json!({"pattern": "needle", "-A": 5000}),
            )));
            Ok(())
        })
        .then("the request is allowed once", |w, _| {
            let reply = w.reply.as_ref().ok_or("no reply")?;
            ensure(
                reply["result"]["outcome"]
                    == json!({"outcome": "selected", "optionId": "bdd-allow"}),
                format!("outcome was {}", reply["result"]["outcome"]),
            )?;
            ensure(
                reply["result"]["_meta"]["homelab/permission"] == json!("admitted"),
                "the reply did not record admission",
            )
        })
        .then("the request is rejected once", |w, _| {
            let reply = w.reply.as_ref().ok_or("no reply")?;
            ensure(
                reply["result"]["outcome"]
                    == json!({"outcome": "selected", "optionId": "bdd-reject"}),
                format!("outcome was {}", reply["result"]["outcome"]),
            )
        })
        .then("the update is accepted silently", |w, _| {
            ensure(w.update_error.is_none(), "the update was refused")?;
            ensure(
                w.update_output.as_ref().map(|o| o.is_empty()).unwrap_or(false),
                "the update produced output",
            )
        })
        .then("the captured call can still be judged", |w, _| {
            let captured = w.captured.clone().ok_or("no captured call")?;
            request_permission(w, captured)?;
            let reply = w.reply.as_ref().ok_or("no reply")?;
            ensure(
                reply["result"]["outcome"]
                    == json!({"outcome": "selected", "optionId": "bdd-allow"}),
                format!("outcome was {}", reply["result"]["outcome"]),
            )
        })
        .then("the update is refused", |w, _| {
            let error = w.update_error.as_ref().ok_or("the update was accepted")?;
            ensure(
                error.contains("Invalid ACP native tool metadata"),
                format!("refusal was {error}"),
            )
        })
        .then("the grep is admitted", |w, _| {
            ensure(w.admitted == Some(true), "the grep was refused")
        })
        .then("the grep is refused", |w, _| {
            ensure(w.admitted == Some(false), "the grep was admitted")
        })
}

#[test]
fn grok_acp_permissions_feature() {
    let report = verify(
        include_str!("../features/grok_acp_permissions.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("grok_acp_permissions.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 7);
}
