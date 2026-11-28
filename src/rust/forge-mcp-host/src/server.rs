//! MCP protocol layer: routes tools, resources, and prompts onto the
//! MissionControlPort. Transport-neutral — the same handler serves
//! stdio and Streamable HTTP.

use std::sync::Arc;

use forge_mcp::{
    parse_tool_call, response_to_json, McError, McRequest, MissionControlPort,
    TASK_AUTHORING_PROMPT, TOOLS,
};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, GetPromptRequestParam, GetPromptResult,
    Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult, PaginatedRequestParam,
    Prompt, PromptMessage, PromptMessageContent, PromptMessageRole, ProtocolVersion,
    ReadResourceRequestParam, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{json, Map, Value};

use crate::{input_schema, Backend};

#[derive(Clone)]
pub struct McpServer {
    backend: Arc<Backend>,
}

impl McpServer {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend: Arc::new(backend),
        }
    }

    async fn execute(&self, request: McRequest) -> Result<forge_mcp::McResponse, McError> {
        self.backend.execute(request).await
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_03_26,
            capabilities: ServerCapabilities {
                tools: Some(Default::default()),
                resources: Some(Default::default()),
                prompts: Some(Default::default()),
                ..Default::default()
            },
            server_info: Implementation {
                name: "forge-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: Some(
                "Mission Control MCP API. Read tools are safe; write tools go through the \
                 Forge runner dispatch API and carry its idempotency and grant rules."
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = TOOLS
            .iter()
            .map(|t| {
                let mut tool = Tool::new(
                    t.name.to_owned(),
                    t.summary.to_owned(),
                    input_schema(t.name),
                );
                tool.annotations = Some(ToolAnnotations {
                    read_only_hint: Some(t.read_only),
                    destructive_hint: Some(t.destructive),
                    ..Default::default()
                });
                tool
            })
            .collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(Map::new()));
        let parsed = match parse_tool_call(&request.name, &args) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "{}: {}",
                    e.code(),
                    e.message()
                ))]));
            }
        };
        match self.execute(parsed).await {
            Ok(response) => {
                let structured = response_to_json(&response);
                let mut result = CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&structured)
                        .unwrap_or_else(|_| structured.to_string()),
                )]);
                result.structured_content = Some(structured);
                Ok(result)
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "{}: {}",
                e.code(),
                e.message()
            ))])),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let recent = Resource::new(
            rmcp::model::RawResource {
                uri: "forge://events/recent".into(),
                name: "Recent Forge events".into(),
                title: None,
                description: Some("The 50 most recent lifecycle events.".into()),
                mime_type: Some("application/json".into()),
                size: None,
                icons: None,
            },
            None,
        );
        Ok(ListResourcesResult {
            resources: vec![recent],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri.clone();
        let mc_request = if uri == "forge://events/recent" {
            McRequest::EventsList { limit: 50 }
        } else if let Some(id) = uri.strip_prefix("forge://workflows/") {
            McRequest::WorkflowGet {
                workflow_id: id.to_owned(),
            }
        } else if let Some(id) = uri.strip_prefix("forge://tasks/") {
            McRequest::WorkGet {
                task_id: id.to_owned(),
            }
        } else {
            return Err(McpError::invalid_params(
                format!("unknown resource {uri}"),
                None,
            ));
        };
        match self.execute(mc_request).await {
            Ok(response) => {
                let value = response_to_json(&response);
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::TextResourceContents {
                        meta: None,
                        uri,
                        mime_type: Some("application/json".into()),
                        text: serde_json::to_string_pretty(&value)
                            .unwrap_or_else(|_| value.to_string()),
                    }],
                })
            }
            Err(e) => Err(McpError::internal_error(e.message(), None)),
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let prompt = Prompt {
            name: "forge_task_authoring".into(),
            title: None,
            description: Some(
                "Author a Forge task: assignment and acceptance criteria as two separate blocks."
                    .into(),
            ),
            arguments: None,
            icons: None,
        };
        Ok(ListPromptsResult {
            prompts: vec![prompt],
            next_cursor: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if request.name != "forge_task_authoring" {
            return Err(McpError::invalid_params(
                format!("unknown prompt {}", request.name),
                None,
            ));
        }
        Ok(GetPromptResult {
            description: Some("Forge task authoring format.".into()),
            messages: vec![PromptMessage {
                role: PromptMessageRole::User,
                content: PromptMessageContent::Text {
                    text: TASK_AUTHORING_PROMPT.into(),
                },
            }],
        })
    }
}

pub fn _unused(_: Value) {
    let _ = json!({});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakePort;
    use forge_mcp::{Decide, McResponse, WorkSubmit};

    fn server() -> McpServer {
        McpServer::new(Backend::Fake(FakePort))
    }

    #[test]
    fn server_info_names_the_service_and_offers_every_surface() {
        let info = server().get_info();
        assert_eq!(info.server_info.name, "forge-mcp");
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_some());
        assert!(info.capabilities.prompts.is_some());
        assert!(info.instructions.is_some());
    }

    #[tokio::test]
    async fn execute_work_get_returns_the_fake_view() {
        let response = server()
            .execute(McRequest::WorkGet {
                task_id: "task-9".into(),
            })
            .await
            .expect("work");
        match response {
            McResponse::Work(view) => {
                assert_eq!(view.task_id, "task-9");
                assert_eq!(view.status, "awaiting_review");
                assert_eq!(view.agent_profile, "muse-db-primary");
            }
            other => panic!("response {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_workflow_get_returns_the_enabled_workflow() {
        let response = server()
            .execute(McRequest::WorkflowGet {
                workflow_id: "wf-9".into(),
            })
            .await
            .expect("workflow");
        match response {
            McResponse::Workflow(view) => {
                assert_eq!(view.workflow_id, "wf-9");
                assert_eq!(view.phase, "implement");
                assert!(view.enabled);
            }
            other => panic!("response {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_events_list_caps_at_five_newest_first() {
        let response = server()
            .execute(McRequest::EventsList { limit: 10 })
            .await
            .expect("events");
        match response {
            McResponse::Events(events) => {
                assert_eq!(events.len(), 5);
                let sequences: Vec<u64> = events.iter().map(|e| e.sequence).collect();
                assert_eq!(sequences, vec![2000, 1999, 1998, 1997, 1996]);
            }
            other => panic!("response {other:?}"),
        }
        let response = server()
            .execute(McRequest::EventsList { limit: 3 })
            .await
            .expect("events");
        match response {
            McResponse::Events(events) => assert_eq!(events.len(), 3),
            other => panic!("response {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_submit_cancel_and_decide_echo_the_task() {
        let submit = McRequest::WorkSubmit(Box::new(WorkSubmit {
            project_alias: "homelab".into(),
            base_revision: "abc".into(),
            agent_profile: "agy-primary".into(),
            prompt: "p".into(),
            acceptance_criteria: "ac".into(),
            idempotency_key: "k".into(),
            workflow_policy_digest: "d".into(),
        }));
        match server().execute(submit).await.expect("submit") {
            McResponse::Submitted { task_id } => {
                assert!(uuid::Uuid::parse_str(&task_id).is_ok());
            }
            other => panic!("response {other:?}"),
        }
        let cancel = McRequest::WorkCancel {
            project_alias: "homelab".into(),
            task_id: "task-9".into(),
            expected_revision: 4,
        };
        match server().execute(cancel).await.expect("cancel") {
            McResponse::Cancelled { task_id } => assert_eq!(task_id, "task-9"),
            other => panic!("response {other:?}"),
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
        match server().execute(decide).await.expect("decide") {
            McResponse::Decided { task_id } => assert_eq!(task_id, "task-9"),
            other => panic!("response {other:?}"),
        }
    }

    #[test]
    fn input_schemas_name_the_required_arguments() {
        let get = crate::input_schema("forge.work_get");
        assert_eq!(get["required"], json!(["task_id"]));
        let submit = crate::input_schema("forge.work_submit");
        let required = submit["required"].as_array().expect("required");
        assert!(required.contains(&json!("workflow_policy_digest")));
        assert!(required.contains(&json!("idempotency_key")));
        let unknown = crate::input_schema("forge.nope");
        assert_eq!(unknown["type"], json!("object"));
        assert!(unknown.get("required").is_none());
    }
}
