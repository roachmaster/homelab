//! Step bindings for features/forge_mcp_host.feature, driving the real
//! McpServer, Backend::Fake(FakePort), and input_schema in forge-mcp-host.

use forge_mcp::{Decide, McRequest, McResponse, WorkSubmit};
use forge_mcp_host::{input_schema, Backend, FakePort, McpServer, ServerHandler};
use forge_scene::bdd::{ensure, verify, Steps};
use serde_json::{json, Map, Value};
use uuid::Uuid;

#[derive(Default)]
struct World {
    server_name: Option<String>,
    has_tools: bool,
    has_resources: bool,
    has_prompts: bool,
    has_instructions: bool,
    work_response: Option<McResponse>,
    workflow_response: Option<McResponse>,
    events_ten: Vec<u64>,
    events_three_len: usize,
    submitted_id: Option<String>,
    cancelled_id: Option<String>,
    decided_id: Option<String>,
    work_get_schema: Option<Map<String, Value>>,
    work_submit_schema: Option<Map<String, Value>>,
    unknown_schema: Option<Map<String, Value>>,
}

fn fake_server() -> McpServer {
    McpServer::new(Backend::Fake(FakePort))
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio current-thread runtime")
        .block_on(future)
}

fn steps() -> Steps<World> {
    Steps::new()
        .given("the MCP server can report its capabilities", |_, _| Ok(()))
        .given(
            "the MCP server can dispatch requests over the fake backend",
            |_, _| Ok(()),
        )
        .given("the MCP host can describe tool input schemas", |_, _| Ok(()))
        .when("server info is queried", |w, _| {
            let info = fake_server().get_info();
            w.server_name = Some(info.server_info.name);
            w.has_tools = info.capabilities.tools.is_some();
            w.has_resources = info.capabilities.resources.is_some();
            w.has_prompts = info.capabilities.prompts.is_some();
            w.has_instructions = info.instructions.is_some();
            Ok(())
        })
        .when("a work get request asks for task-9", |w, _| {
            let resp = block_on(fake_server().execute(McRequest::WorkGet {
                task_id: "task-9".into(),
            }))
            .map_err(|e| format!("{e:?}"))?;
            w.work_response = Some(resp);
            Ok(())
        })
        .when("a workflow get request asks for wf-9", |w, _| {
            let resp = block_on(fake_server().execute(McRequest::WorkflowGet {
                workflow_id: "wf-9".into(),
            }))
            .map_err(|e| format!("{e:?}"))?;
            w.workflow_response = Some(resp);
            Ok(())
        })
        .when(
            "events are requested with limit ten and then limit three",
            |w, _| {
                let server = fake_server();
                let ten = block_on(server.execute(McRequest::EventsList { limit: 10 }))
                    .map_err(|e| format!("{e:?}"))?;
                if let McResponse::Events(events) = ten {
                    w.events_ten = events.into_iter().map(|e| e.sequence).collect();
                }
                let three = block_on(server.execute(McRequest::EventsList { limit: 3 }))
                    .map_err(|e| format!("{e:?}"))?;
                if let McResponse::Events(events) = three {
                    w.events_three_len = events.len();
                }
                Ok(())
            },
        )
        .when("submit, cancel, and decide requests are executed", |w, _| {
            let server = fake_server();
            let submit = McRequest::WorkSubmit(Box::new(WorkSubmit {
                project_alias: "homelab".into(),
                base_revision: "abc".into(),
                agent_profile: "agy-primary".into(),
                prompt: "p".into(),
                acceptance_criteria: "ac".into(),
                idempotency_key: "k".into(),
                workflow_policy_digest: "d".into(),
            }));
            if let McResponse::Submitted { task_id } =
                block_on(server.execute(submit)).map_err(|e| format!("{e:?}"))?
            {
                w.submitted_id = Some(task_id);
            }
            let cancel = McRequest::WorkCancel {
                project_alias: "homelab".into(),
                task_id: "task-9".into(),
                expected_revision: 4,
            };
            if let McResponse::Cancelled { task_id } =
                block_on(server.execute(cancel)).map_err(|e| format!("{e:?}"))?
            {
                w.cancelled_id = Some(task_id);
            }
            let decide = McRequest::Decide(Box::new(Decide {
                project_alias: "homelab".into(),
                task_id: "task-9".into(),
                expected_revision: 4,
                plan_attempt_id: "attempt-1".into(),
                plan_sha256: "sha".into(),
                agent_profile: "agy-primary".into(),
                approve: true,
                idempotency_key: "k".into(),
            }));
            if let McResponse::Decided { task_id } =
                block_on(server.execute(decide)).map_err(|e| format!("{e:?}"))?
            {
                w.decided_id = Some(task_id);
            }
            Ok(())
        })
        .when(
            "schemas are queried for work_get, work_submit, and an unknown tool",
            |w, _| {
                w.work_get_schema = Some(input_schema("forge.work_get"));
                w.work_submit_schema = Some(input_schema("forge.work_submit"));
                w.unknown_schema = Some(input_schema("forge.nope"));
                Ok(())
            },
        )
        .then(
            "the service name is forge-mcp with tools, resources, prompts, and instructions enabled",
            |w, _| {
                ensure(
                    w.server_name.as_deref() == Some("forge-mcp"),
                    format!("server_name was {:?}", w.server_name),
                )?;
                ensure(
                    w.has_tools && w.has_resources && w.has_prompts && w.has_instructions,
                    "one or more MCP capability surfaces were missing",
                )
            },
        )
        .then(
            "the reply returns task-9 awaiting review on muse-db-primary",
            |w, _| match w.work_response.as_ref() {
                Some(McResponse::Work(view)) => {
                    ensure(view.task_id == "task-9", format!("task_id was {}", view.task_id))?;
                    ensure(
                        view.status == "awaiting_review",
                        format!("status was {}", view.status),
                    )?;
                    ensure(
                        view.agent_profile == "muse-db-primary",
                        format!("agent_profile was {}", view.agent_profile),
                    )
                }
                other => Err(format!("unexpected work response: {other:?}")),
            },
        )
        .then(
            "the reply returns wf-9 enabled in the implement phase",
            |w, _| match w.workflow_response.as_ref() {
                Some(McResponse::Workflow(view)) => {
                    ensure(
                        view.workflow_id == "wf-9",
                        format!("workflow_id was {}", view.workflow_id),
                    )?;
                    ensure(view.phase == "implement", format!("phase was {}", view.phase))?;
                    ensure(view.enabled, "workflow was not enabled")
                }
                other => Err(format!("unexpected workflow response: {other:?}")),
            },
        )
        .then(
            "the first reply has five descending sequences and the second has three",
            |w, _| {
                ensure(
                    w.events_ten == vec![2000, 1999, 1998, 1997, 1996],
                    format!("sequences were {:?}", w.events_ten),
                )?;
                ensure(
                    w.events_three_len == 3,
                    format!("second length was {}", w.events_three_len),
                )
            },
        )
        .then(
            "submit yields a valid UUID while cancel and decide echo task-9",
            |w, _| {
                let submitted = w.submitted_id.as_deref().ok_or("missing submitted id")?;
                ensure(
                    Uuid::parse_str(submitted).is_ok(),
                    format!("submitted id not UUID: {submitted}"),
                )?;
                ensure(
                    w.cancelled_id.as_deref() == Some("task-9"),
                    format!("cancelled_id was {:?}", w.cancelled_id),
                )?;
                ensure(
                    w.decided_id.as_deref() == Some("task-9"),
                    format!("decided_id was {:?}", w.decided_id),
                )
            },
        )
        .then(
            "known tools list their required fields and the unknown tool returns an unconstrained object",
            |w, _| {
                let get = w.work_get_schema.as_ref().ok_or("missing work_get schema")?;
                ensure(get["required"] == json!(["task_id"]), "work_get required mismatch")?;
                let submit = w
                    .work_submit_schema
                    .as_ref()
                    .ok_or("missing work_submit schema")?;
                let required = submit["required"]
                    .as_array()
                    .ok_or("work_submit required not array")?;
                ensure(
                    required.contains(&json!("workflow_policy_digest"))
                        && required.contains(&json!("idempotency_key")),
                    "work_submit missing required fields",
                )?;
                let unknown = w.unknown_schema.as_ref().ok_or("missing unknown schema")?;
                ensure(unknown["type"] == json!("object"), "unknown type not object")?;
                ensure(unknown.get("required").is_none(), "unknown schema had required")
            },
        )
}

#[test]
fn forge_mcp_host_feature() {
    let report = verify(include_str!("../features/forge_mcp_host.feature"), &steps())
        .unwrap_or_else(|errors| panic!("forge_mcp_host.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
